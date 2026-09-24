import { idempotencyKey, sdk } from "./sdk"
import { toMcpServer, toMcpTool, toMcpStoreSearch, toMcpStoreInstall } from "./mappers"
import type {
  MCPConnectResponse,
  MCPHealthResponse,
  MCPServer,
  MCPServerCreate,
  MCPServerListResponse,
  MCPServerUpdate,
  MCPToolListResponse,
  MCPStoreSearchResponse,
  MCPStoreInstallRequest,
  MCPStoreInstallResponse,
} from "./types"

export const mcpApi = {
  /** List all configured MCP servers with status and tools. */
  listServers: async (): Promise<MCPServerListResponse> => {
    const result = await sdk.mcpListServers({})
    return { servers: result.servers.map(toMcpServer) }
  },

  /** Add a new MCP server configuration. */
  addServer: async (body: MCPServerCreate): Promise<MCPServer> =>
    toMcpServer(
      await sdk.mcpAddServer({
        idempotencyKey: idempotencyKey(),
        name: body.name,
        transport: body.transport ?? "stdio",
        command: body.command ?? "",
        args: body.args ?? [],
        env: body.env ?? {},
        url: body.url ?? "",
        headers: body.headers ?? {},
        enabled: body.enabled ?? true,
        description: body.description ?? "",
        capabilities: body.capabilities ?? [],
        timeoutS: body.timeout_s ?? 30,
        version: "",
        author: "",
        compatibility: "",
      })
    ),

  /** Update an existing MCP server configuration. */
  updateServer: async (name: string, body: MCPServerUpdate): Promise<MCPServer> =>
    toMcpServer(
      await sdk.mcpUpdateServer({
        idempotencyKey: idempotencyKey(),
        name,
        transport: body.transport ?? null,
        command: body.command ?? null,
        args: body.args ?? null,
        env: body.env ?? null,
        url: body.url ?? null,
        headers: body.headers ?? null,
        enabled: body.enabled ?? null,
        description: body.description ?? null,
        capabilities: body.capabilities ?? null,
        timeoutS: body.timeout_s ?? null,
      })
    ),

  /** Remove an MCP server. */
  removeServer: async (name: string): Promise<void> => {
    await sdk.mcpRemoveServer({ name })
  },

  /** Connect to an MCP server and discover tools. */
  connect: async (name: string): Promise<MCPConnectResponse> => {
    const result = await sdk.mcpConnect({ name })
    return {
      name: result.name,
      status: result.status,
      tools_count: result.toolsCount,
      error: result.error ?? null,
    }
  },

  /** Disconnect an MCP server. */
  disconnect: async (name: string): Promise<MCPConnectResponse> => {
    const result = await sdk.mcpDisconnect({ name })
    return {
      name: result.name,
      status: result.status,
      tools_count: result.toolsCount,
      error: result.error ?? null,
    }
  },

  /** Health-check a connected server. */
  health: async (name: string): Promise<MCPHealthResponse> => {
    const result = await sdk.mcpHealth({ name })
    return { name: result.name, healthy: result.healthy }
  },

  /** List all tools across connected MCP servers. */
  listTools: async (): Promise<MCPToolListResponse> => {
    const result = await sdk.mcpListTools({})
    return { tools: result.tools.map(toMcpTool) }
  },

  /** Reconnect all enabled servers. */
  reconnectAll: async (): Promise<MCPServerListResponse> => {
    const result = await sdk.mcpReconnectAll({})
    return { servers: result.servers.map(toMcpServer) }
  },

  // --- Store / Marketplace (network access to the MCP Registry) ---

  /** Search the official MCP Registry. */
  storeSearch: async (q: string, limit = 10): Promise<MCPStoreSearchResponse> =>
    toMcpStoreSearch(await sdk.mcpStoreSearch({ query: q, limit })),

  /** List popular servers from the MCP Registry. */
  storePopular: async (limit = 20): Promise<MCPStoreSearchResponse> =>
    toMcpStoreSearch(await sdk.mcpStorePopular({ limit })),

  /** Install a server from the MCP Registry. */
  storeInstall: async (
    body: MCPStoreInstallRequest
  ): Promise<MCPStoreInstallResponse> =>
    toMcpStoreInstall(
      await sdk.mcpStoreInstall({
        idempotencyKey: idempotencyKey(),
        registryName: body.registry_name,
        localName: body.local_name ?? "",
      })
    ),
}
