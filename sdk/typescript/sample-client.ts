import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";

import type {
  Command,
  CommandEnvelope,
  ResponsePayload,
  RpcRequest,
  ServerFrame,
} from "./src/generated/cool_protocol.js";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const defaultBinary = join(
  repositoryRoot,
  "target",
  "debug",
  process.platform === "win32" ? "cool.exe" : "cool",
);

class SampleClient {
  readonly process: ChildProcessWithoutNullStreams;
  readonly events: ServerFrame[] = [];
  readonly pending = new Map<
    number,
    { resolve: (payload: ResponsePayload) => void; reject: (error: Error) => void }
  >();
  nextId = 1;
  fatalError: Error | null = null;

  constructor(binary: string) {
    this.process = spawn(binary, ["app-server"], {
      cwd: repositoryRoot,
      stdio: ["pipe", "pipe", "pipe"],
    });
    createInterface({ input: this.process.stdout }).on("line", (line) => {
      const frame = JSON.parse(line) as ServerFrame;
      if ("result" in frame && typeof frame.id === "number") {
        this.pending.get(frame.id)?.resolve(frame.result);
        this.pending.delete(frame.id);
      } else if ("error" in frame) {
        const error = new Error(`${frame.error.coolCode}: ${frame.error.message}`);
        if (typeof frame.id === "number") {
          this.pending.get(frame.id)?.reject(error);
          this.pending.delete(frame.id);
        } else {
          this.rejectAll(error);
        }
      } else {
        this.events.push(frame);
      }
    });
    this.process.on("error", (error) => this.rejectAll(error));
    this.process.on("exit", (code, signal) => {
      this.rejectAll(new Error(`cool app-server exited: code=${code} signal=${signal}`));
    });
  }

  rejectAll(error: Error): void {
    this.fatalError = error;
    for (const pending of this.pending.values()) pending.reject(error);
    this.pending.clear();
  }

  command(command: Command): Promise<ResponsePayload> {
    const id = this.nextId++;
    const envelope: CommandEnvelope = {
      protocolVersion: 1,
      commandId: `sample-${id}`,
      command,
    };
    const request: RpcRequest = {
      jsonrpc: "2.0",
      id,
      method: "cool.command",
      params: envelope,
    };
    return new Promise((resolveResponse, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`request ${id} timed out`));
      }, 5_000);
      this.pending.set(id, {
        resolve: (response) => {
          clearTimeout(timer);
          resolveResponse(response);
        },
        reject: (error) => {
          clearTimeout(timer);
          reject(error);
        },
      });
      this.process.stdin.write(`${JSON.stringify(request)}\n`);
    });
  }

  async waitForPromptEvents(): Promise<void> {
    const deadline = Date.now() + 5_000;
    while (this.events.length < 5) {
      if (this.fatalError) throw this.fatalError;
      if (Date.now() >= deadline) throw new Error("event stream timed out");
      await new Promise((resolveDelay) => setTimeout(resolveDelay, 5));
    }
  }
}

const client = new SampleClient(process.env.COOL_BIN ?? defaultBinary);
try {
  const initialized = await client.command({
    method: "initialize",
    params: {
      clientName: "typescript-m5-sample",
      clientVersion: "1",
      supportedProtocolVersions: [1],
      capabilities: [],
    },
  });
  if (initialized.kind !== "initialized") throw new Error("initialize contract mismatch");

  const created = await client.command({
    method: "session.create",
    params: {
      idempotencyKey: "sample-session",
      title: "M7 sample",
      projectKey: null,
    },
  });
  if (created.kind !== "session_created") throw new Error("session.create contract mismatch");

  const prompted = await client.command({
    method: "session.prompt",
    params: {
      idempotencyKey: "sample-prompt",
      sessionId: created.value.sessionId,
      content: [{ type: "text", text: "M7 handshake" }],
      model: null,
    },
  });
  if (prompted.kind !== "prompt_accepted") throw new Error("session.prompt contract mismatch");
  await client.waitForPromptEvents();

  const eventKinds = client.events.flatMap((frame) =>
    "params" in frame && frame.params.type === "event" ? [frame.params.value.event.kind] : [],
  );
  if (
    eventKinds.join(",") !==
    "run.started,item.completed,content.delta,item.completed,run.completed"
  ) {
    throw new Error(`unexpected event sequence: ${eventKinds.join(",")}`);
  }
  console.log(
    JSON.stringify({
      status: "ok",
      sessionId: created.value.sessionId,
      runId: prompted.value.runId,
      eventKinds,
    }),
  );
} finally {
  client.process.stdin.end();
  client.process.kill();
}
