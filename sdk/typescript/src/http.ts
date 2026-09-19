// HTTP/SSE transport for the M11 Rust Web facade (`cool serve`).
//
// The transport speaks only the canonical App Protocol: commands go to
// `POST /api/rpc` as `RpcRequest`/`ServerFrame` JSON, and run events arrive on
// the cursor/reconnect SSE stream `GET /api/events`.
//
// Identity: the facade maps one identity to one durable protocol connection.
// Browsers get a `cool_client` cookie automatically. In Node (no cookie jar)
// you MUST pass the same stable `clientId` to both `HttpTransport` and
// `streamRunEvents`, otherwise a run's live events are delivered to the
// connection that started it, not to a fresh subscriber connection. A
// different connection can still follow a run by issuing `run.subscribe`
// (CoolSdk.runSubscribe) before opening the stream; the runtime then fans the
// run's live events out to it.

import { CoolProtocolError, type CoolTransport } from "./client.js";
import type * as protocol from "./generated/cool_protocol.js";

export interface HttpTransportOptions {
  /** Origin of the Rust facade, e.g. `http://127.0.0.1:8000`. */
  baseUrl: string;
  /** Bearer token when `cool serve --token`/`COOL_API_TOKEN` is configured. */
  token?: string;
  /** Stable browser identity; the facade also sets a `cool_client` cookie. */
  clientId?: string;
  /** Injectable fetch for tests and non-global environments. */
  fetchImpl?: typeof fetch;
}

interface ServerFrame {
  jsonrpc: "2.0";
  id?: number;
  result?: protocol.ResponsePayload;
  error?: protocol.ProtocolError;
}

/** `CoolTransport` over the facade's JSON-RPC endpoint. */
export class HttpTransport implements CoolTransport {
  private nextId = 1;

  constructor(private readonly options: HttpTransportOptions) {}

  async initialize(
    clientName = "cool-web",
    clientVersion = "1"
  ): Promise<protocol.InitializeResult> {
    const result = await this.request({
      method: "initialize",
      params: {
        clientName,
        clientVersion,
        supportedProtocolVersions: [1],
        capabilities: [],
      },
    });
    if (result.kind !== "initialized") throw new Error("cool http: initialize was not answered");
    return result.value;
  }

  async request(command: protocol.Command): Promise<protocol.ResponsePayload> {
    const id = this.nextId++;
    const frame = await this.exchange({
      jsonrpc: "2.0",
      id,
      method: "cool.command",
      params: { protocolVersion: 1, commandId: `sdk-${id}`, command },
    });
    if (frame.error) throw new CoolProtocolError(frame.error);
    if (!frame.result) throw new Error("cool http: response carried no result");
    return frame.result;
  }

  private async exchange(body: unknown): Promise<ServerFrame> {
    const doFetch = this.options.fetchImpl ?? fetch;
    const headers: Record<string, string> = { "content-type": "application/json" };
    if (this.options.token) headers.authorization = `Bearer ${this.options.token}`;
    if (this.options.clientId) headers["x-cool-client"] = this.options.clientId;
    const response = await doFetch(`${this.options.baseUrl}/api/rpc`, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
      credentials: "same-origin",
    });
    if (!response.ok) {
      throw new Error(`cool http: command failed with status ${response.status}`);
    }
    return (await response.json()) as ServerFrame;
  }
}

export interface StreamRunEventsOptions {
  baseUrl: string;
  token?: string;
  clientId?: string;
  fetchImpl?: typeof fetch;
  signal?: AbortSignal;
  /** Exclusive durable cursor: yield only events with `seq > afterSeq`. */
  afterSeq?: number;
  limit?: number;
}

/** Canonical cursor/reconnect run-event stream. Reconnecting with the last
 * seen `seq` never duplicates or skips a durable event. */
export async function* streamRunEvents(
  runId: string,
  options: StreamRunEventsOptions
): AsyncGenerator<protocol.EventEnvelope> {
  const doFetch = options.fetchImpl ?? fetch;
  const query = new URLSearchParams({ runId });
  if (options.afterSeq !== undefined) query.set("afterSeq", String(options.afterSeq));
  if (options.limit !== undefined) query.set("limit", String(options.limit));
  const headers: Record<string, string> = { accept: "text/event-stream" };
  // `fetch` can set Authorization, so the token never needs to reach the URL
  // (and therefore access logs / browser history). The facade still accepts
  // `?token=` for native `EventSource` callers.
  if (options.token) headers.authorization = `Bearer ${options.token}`;
  if (options.clientId) headers["x-cool-client"] = options.clientId;
  const response = await doFetch(`${options.baseUrl}/api/events?${query.toString()}`, {
    headers,
    signal: options.signal,
    credentials: "same-origin",
  });
  if (!response.ok || !response.body) {
    throw new Error(`cool http: event stream failed with status ${response.status}`);
  }

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let eventName = "";
  let data: string[] = [];

  const flush = (): protocol.EventEnvelope | undefined => {
    const name = eventName;
    const payload = data.join("\n");
    eventName = "";
    data = [];
    if (name !== "run.event" || !payload) return undefined;
    return JSON.parse(payload) as protocol.EventEnvelope;
  };

  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let index = buffer.indexOf("\n");
    while (index >= 0) {
      const line = buffer.slice(0, index).replace(/\r$/, "");
      buffer = buffer.slice(index + 1);
      if (line === "") {
        const envelope = flush();
        if (envelope) yield envelope;
      } else if (line.startsWith("event:")) {
        eventName = line.slice(6).trim();
      } else if (line.startsWith("data:")) {
        data.push(line.slice(5).replace(/^ /, ""));
      }
      index = buffer.indexOf("\n");
    }
  }
}
