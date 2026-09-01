# M3 plugin compatibility matrix

M3 makes Agent Plugins 1.0 the portable contract. Vendor layouts are read-only compatibility
inputs: `cool plugin doctor` reports what can be normalized and what remains inactive. It never
executes plugin content while inspecting a bundle.

| Input | Component | M3 behavior | Diagnostic status |
|---|---|---|---|
| Agent Plugins 1.0 | `plugin.json` | Strict schema validation | supported / unsafe |
| Agent Plugins 1.0 | `skills/*/SKILL.md` | Agent Skills validation and canonical `Skill` | supported / unsafe |
| Agent Plugins 1.0 | `mcp.json` | Canonical stdio and Streamable HTTP declarations | supported / unsafe |
| Cool extension | hooks | Canonical declarative hook plus definition/source trust hash | supported / unsafe |
| Codex | `.codex-plugin/plugin.json` | Identity and metadata normalization | transformed |
| Codex | `skills/` | Canonical Agent Skills validation | transformed / unsafe |
| Codex | `.mcp.json`, `.app.json`, `hooks/hooks.json` | Detected and reported, inactive until semantic adapters | ignored |
| Claude Code | optional `.claude-plugin/plugin.json` | Identity and metadata normalization, including manifestless default layouts | transformed |
| Claude Code | `skills/` or root `SKILL.md` | Canonical Agent Skills validation | transformed / unsafe |
| Claude Code | commands, agents, hooks, MCP, LSP, output styles, themes, monitors, bin, settings | Detected and reported, inactive until semantic adapters | ignored |

`supported` means the portable meaning is preserved. `transformed` means the source declaration is
represented by the canonical model without execution. `ignored` is visible but inactive. `unsafe`
blocks the affected capability or bundle. A compatibility adapter never silently upgrades an
ignored declaration into executable behavior.

Representative fixtures live in `backend/tests/fixtures/plugins/`. They mirror the documented
directory shapes and exercise semantic diagnostics; they are test data, not vendored releases from
OpenAI or Anthropic.

Lifecycle commands operate on Tier-1 portable bundles:

```text
cool plugin install PATH
cool plugin install GIT_URL --git --revision FULL_COMMIT_SHA
cool plugin list
cool plugin enable NAME
cool plugin disable NAME
cool plugin update NAME PATH
cool plugin remove NAME
cool plugin remove NAME --purge-data
cool plugin validate PATH
cool plugin doctor PATH_OR_INSTALLED_NAME
```

The explicit `--purge-data` flag is the separate confirmation boundary for deletion of mutable
plugin data. Normal removal preserves it. Install and update leave the bundle disabled; `list` and
installed `doctor` expose required runtimes and capabilities before an explicit `enable`.
