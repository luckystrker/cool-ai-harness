import { idempotencyKey, sdk } from "./sdk"

// Canonical extension admin surface (M11 WS1/WS3). The records are the generated
// App Protocol types; no mapper is needed because the wire shape is camelCase.
export type {
  ExtensionDiagnosticRecord,
  ExtensionStatusResult,
  HookRecord,
  McpServerRecord,
  McpToolPolicyRecord,
  PluginRecord,
  SkillRecord,
  WorkerRecord,
} from "./generated/cool_protocol"

export const extensionsApi = {
  /** Read-only snapshot of plugins, workers, hook reviews, skills and MCP state. */
  status: () => sdk.extensionsStatus({}),
  /** Enable or disable one installed plugin. */
  setPluginEnabled: (plugin: string, enabled: boolean) =>
    sdk.extensionsPluginEnabled({ idempotencyKey: idempotencyKey(), plugin, enabled }),
  /** Approve or reject one hook declaration (trustHash must match the current one). */
  setHookReview: (plugin: string, hook: string, trustHash: string, approved: boolean) =>
    sdk.extensionsHookReview({
      idempotencyKey: idempotencyKey(),
      plugin,
      hook,
      trustHash,
      approved,
    }),
}
