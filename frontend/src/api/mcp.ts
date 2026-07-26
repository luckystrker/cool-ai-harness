import { api } from "./client"
import type {
  MCPConnectResponse,
  MCPHealthResponse,
  MCPServerCreate,
  MCPServerListResponse,
  MCPServerUpdate,
  MCPToolListResponse,
  MCPServer,
  MCPStoreSearchResponse,
  MCPStoreInstallRequest,
  MCPStoreInstallResponse,
} from "./types"

export const mcpApi = {
  /** List all configured MCP servers with status and tools. */
  listServers: () => api.get<MCPServerListResponse>("/api/mcp/servers"),

  /** Add a new MCP server configuration. */
  addServer: (body: MCPServerCreate) => api.post<MCPServer>("/api/mcp/servers", body),

  /** Update an existing MCP server configuration. */
  updateServer: (name: string, body: MCPServerUpdate) =>
    api.patch<MCPServer>(`/api/mcp/servers/${encodeURIComponent(name)}`, body),

  /** Remove an MCP server. */
  removeServer: (name: string) =>
    api.delete<void>(`/api/mcp/servers/${encodeURIComponent(name)}`),

  /** Connect to an MCP server and discover tools. */
  connect: (name: string) =>
    api.post<MCPConnectResponse>(`/api/mcp/servers/${encodeURIComponent(name)}/connect`),

  /** Disconnect an MCP server. */
  disconnect: (name: string) =>
    api.post<MCPConnectResponse>(`/api/mcp/servers/${encodeURIComponent(name)}/disconnect`),

  /** Health-check a connected server. */
  health: (name: string) =>
    api.get<MCPHealthResponse>(`/api/mcp/servers/${encodeURIComponent(name)}/health`),

  /** List all tools across connected MCP servers. */
  listTools: () => api.get<MCPToolListResponse>("/api/mcp/tools"),

  /** Reconnect all enabled servers. */
  reconnectAll: () => api.post<MCPServerListResponse>("/api/mcp/reconnect-all"),

  // --- Store / Marketplace ---

  /** Search the official MCP Registry. */
  storeSearch: (q: string, limit = 10) =>
    api.get<MCPStoreSearchResponse>(
      `/api/mcp/store/search?q=${encodeURIComponent(q)}&limit=${limit}`
    ),

  /** List popular servers from the MCP Registry. */
  storePopular: (limit = 20) =>
    api.get<MCPStoreSearchResponse>(`/api/mcp/store/popular?limit=${limit}`),

  /** Install a server from the MCP Registry. */
  storeInstall: (body: MCPStoreInstallRequest) =>
    api.post<MCPStoreInstallResponse>("/api/mcp/store/install", body),
}
