# M3 plugin supply-chain threat model

## Scope and trust boundary

M3 discovers, validates, installs and activates declarative plugin metadata on the current Python
runtime. Plugin content is untrusted. Parsing a manifest or skill must not import Python modules,
run scripts, start MCP servers or execute hooks. Runtime execution remains behind the existing MCP,
tool and capability boundaries. The local/single-user process is the supported deployment profile;
the store is designed for one writer at a time.

## Assets

- application configuration, secrets, database and workspace content;
- integrity of installed plugin code and declarations;
- persistent plugin-owned user data;
- provenance, diagnostics and approval/trust decisions;
- availability of healthy sibling components and bundles.

## Threats and controls

| Threat | M3 control |
|---|---|
| Moving Git branch/tag changes installed code | Git installs require a full 40-character commit SHA, verify checked-out `HEAD`, and record the revision. |
| Local source changes after install | Content is copied to a content-addressed installation and loaded only when its SHA-256 tree hash still matches the lockfile. |
| Install immediately activates untrusted code | Install and update always leave the bundle disabled; startup projects only explicitly enabled, integrity-checked bundles. |
| Lockfile redirects reads/writes or deletion to another bundle | Every entry is type-checked and its paths must equal `installations/<name>/<hash>` and `data/<name>` before loading or deletion. |
| Symlink, junction, reparse or path traversal escapes the bundle | Installation and compatibility inspection reject link-like paths; resolved component paths must remain under the bundle root. |
| Manifest or component corruption disables unrelated plugins | Integrity and component errors become diagnostics; loading continues for healthy siblings and bundles. |
| Inspection executes attacker-controlled code | Validators and compatibility adapters read JSON/YAML/Markdown only. There are no plugin Python callbacks or dynamic library entrypoints. |
| MCP declaration overwrites reserved environment | `PLUGIN_ROOT` and `PLUGIN_DATA` are reserved and injected by the loader; plugin values cannot replace them. |
| Relative command or working directory escapes roots | Executable paths and working directories are normalized and confined to the immutable install or mutable data root. |
| Insecure remote MCP target or malicious headers | Non-loopback HTTP, credentials/fragments and invalid/duplicate header names are rejected; client-generated headers win case-insensitively. |
| Hook definition changes after approval | Trust hash covers source, revision, content hash, handler, matcher, ordering, concurrency and capabilities. |
| Uninstall destroys user state | Installations and mutable data use separate roots; remove preserves data unless `--purge-data` is explicit. |
| Vendor-specific fields gain accidental authority | Compatibility adapters emit transformed/ignored/unsafe diagnostics and do not activate unsupported executable declarations. |
| Command-option injection through Git source | Empty/NUL/option-looking sources are rejected and subprocesses run without a shell. |
| Plugin shadows a sibling skill/MCP/tool | Existing skills and native MCP configs win with diagnostics; plugin MCP namespaces and provider tool names use stable safe encodings and tool registration refuses collisions. |
| Enable hides executable/network requirements | Lock entries and installed doctor output expose bare MCP/hook runtimes plus implicit `execute`/`network` and explicit capabilities before enable. |

## Residual risks and later phases

- The JSON lockfile is replaced atomically but has no inter-process transaction lock. Concurrent
  lifecycle writers are unsupported in the M3 local/single-user profile; a Rust store must add an
  OS-level writer lock.
- A local user with write access to the store can tamper with both installations and the lockfile.
  Hash validation detects installation drift but is not a signature or remote identity proof.
- Resolved runtime dependencies record bare executable names, not immutable package checksums.
  Dependency acquisition and worker lockfiles belong to M8.
- M3 parses hook declarations but does not execute them. Supervision, approval persistence, audit
  events and capability enforcement are M8 responsibilities.
- Tier-2 MCP/hooks/apps/agents remain diagnostic-only until semantic adapters preserve their
  security meaning. OpenCode executable plugins remain isolated-worker work for M8.
- Plugin lifecycle has no multi-user actor/authorization model. VPS and Telegram exposure require
  later authentication, authorization and audit work; M3 is not a safe public deployment claim.

## Verification obligations

Tests cover strict canonical conformance, component isolation, disabled-by-default activation,
content and lockfile type/path tampering, sibling-safe removal, Windows junctions, data
preservation, explicit purge, pinned Git checkout, runtime/dependency/capability preview, registry
collisions, generated HTTP headers, provider-safe MCP tools and semantic diagnostics for
representative portable/Codex/Claude layouts. Independent review inspected the actual diff and
untracked fixtures before M3 was marked complete.
