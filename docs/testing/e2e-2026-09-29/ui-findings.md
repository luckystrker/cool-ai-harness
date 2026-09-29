# Cool AI Harness — E2E UI Findings Report

**Date:** 2026-09-29 · **App:** `cool.exe serve` @ http://127.0.0.1:8000 (profile=local, --allow-shell)
**Method:** Full browser pass in Chrome covering 11 test areas; tool results verified on disk and via `/api/rpc` where the UI was ambiguous. Screen recording: `C:\Users\Administrator\screencasts\rec-f435903e-5d39-4112-9f7d-5f4788f67ae0\rec-f435903e-5d39-4112-9f7d-5f4788f67ae0-edited.mp4`

## Worked end-to-end

| # | Area | Result |
|---|------|--------|
| 1 | Settings → Providers | OpenRouter row shows masked key hint (sk-…cf14), base URL, enabled-model list. Edit-dialog model probe fetched the live OpenRouter catalog; search + checkbox + save persisted `stealth/space-bunny-alpha`. |
| 2 | Chat + workspace policy (WS1) | `.cool/policy.json` honored — `read_file`/`write_file` ran with **no approval prompts**, streamed fine. `ui-test.txt` created on disk containing `UI-OK` (verified). Tool cards render name + ok status + expandable args. |
| 3 | Approvals (WS2, no policy) | File edit produced an inline approval card (tool name, diff preview, Allow/Deny + "don't ask again" scope: session/project/user). Allow → card flips Approved, run continues, `data.txt` gained the line on disk. Choosing "project" wrote `.cool/policy.json` into WS2 — subsequent identical edit ran without asking (rule persisted). |
| 4 | ask_user | Question card renders with the question, clickable options + free-text input; the chosen answer flows back into the run and the assistant acknowledges it. |
| 5 | Shell error surfacing | Shell rejection renders as a red expandable `shell · error` card with Arguments + Error (`security policy rejected tool: host launcher cannot isolate network access`). Model retried 3 variants, then explained the failure honestly. |
| 6 | Subagents | Launch form works: parent-conversation picker, task, role (3 builtins). Run #1 Running → Completed with correct file listing of WS1. Output dialog shows prompt, rendered result, and full message transcript. |
| 8 | Session fork | "Fork from here" on an assistant message created Conversation #5 with the full history incl. tool cards; sent a different prompt, reply worked (`FORKED-BRANCH`). Original untouched. |
| 9 | Pages sweep | Memory, Wiki, Deep Research, Tasks, Constructor, Subagents, Analytics, Budgets, Inspector, Extensions, Settings, chat — all render with sane empty states. **Console clean**: zero JS errors/warnings (only 7 benign Chrome autofill "form field should have id/name" hints). |
| 10 | Multimodal | Paperclip → file picked → chip w/ thumbnail + size; message sent with `[image: 1]`; the model **actually read it** ("E2E-IMG" in white on blue — correct). Attachments side panel lists the file with a working `/api/conversations/1/artifacts/1/download` link. |
| 11 | Error resilience | Bogus model `bogus/nonexistent-model-xyz` → run failed fast (7ms), recorder shows Failed/Failure recorded, UI stayed usable, switching the model back restored normal replies (`recovered`). |

## Bugs / issues found

### Major
1. **Inspector is unreachable dead UI.** Its run picker is always empty: the page calls legacy RPC `runs.list` which returns `[]` for every conversation (verified directly for convs 1, 2, 3), while the runtime writes to the session-run store (`session.runs` shows 6 runs on conv #1). Timeline, Compare, and Replay can never be reached. → Backend should map session runs into `runs.list` (or Inspector should consume `session.runs`).
2. **Directory browser can't drill into folders.** Child paths are built as `\\?\C:\Users\Administrator/cool-e2e-workspace` (mixed separators + `\\?\` prefix); the server returns "path is not a directory". Blocks picking any subdirectory in the workdir dialog and Project dialog. (Workaround used: `conversations.update` via RPC.)
3. **Fork silently drops conversation settings.** `conversations.create`-based fork carries `workingDirectory` and history but leaves `model`, `permissions`, `capabilityPolicy` = null — the forked chat shows "Set model"/"Mode" and loses the parent's safety posture without telling the user.
4. **Stale/misleading approval states.** (a) A run left `awaiting_approval` shows "Awaiting approval" after page reload but **no approval card re-renders** — unrecoverable dead end via UI. (b) If `approval.resolve` fails client-side, the card flips to "Denied — the agent was notified" although the backend never got a decision (run still pending). Both observed live; fixed only by resolving via RPC.

### Medium
5. **Failed runs surface no reason in the chat.** A `run.failed` (reason `provider_http`) renders as an *empty assistant bubble* with just "7ms". No toast, no inline error text. Only the recorder sidebar says "Failed". The backend event carries only a reason code — UI should at least render that ("Run failed: provider_http").
6. **Analytics & Budgets record nothing.** Zero LLM calls / tokens / tool calls / spend despite ~8 completed runs with real token counts today — the telemetry pipeline isn't fed by the session-run runtime (same class of gap as the Inspector's `runs.list`).
7. **Shell/git unusable on Windows (known issue, still present).** Host launcher fails closed with "cannot isolate network access" for *all* process execution — including after granting `network=Allow` in Settings → Agent → Capability policy (verified the conversation inherited `{"network":"allow"}` via RPC; error unchanged). The capability toggle exists and saves but cannot unblock the v1 JobObject launcher. `git` and shell-out subagents are affected too.
8. **Empty assistant turns** — the model frequently returns empty turns (finishReason `stop`, zero content) especially after tool results; they render as empty bubbles and the recorder still reports "Completed". Model-side flakiness, but the UI gives no signal the turn was empty.

### Minor
9. Subagent output dialog shows tool messages as `(empty)`; run card shows `0 tokens`.
10. Recorder "Execution: Completed without tools" is misleading on runs that did call tools (it reflects only the final segment).
11. Attachments panel shows "0" while a file is staged in the composer (counts only sent attachments).
12. Constructor → "Blueprints" section has no empty-state text (bare header) unlike every other section.
13. Occasional sidebar/dropdown click-registration quirks (rows needed a second click or keyboard Enter) — low confidence, possibly automation artifacts.

## Console health
- No uncaught JS errors or warnings across the whole sweep.
- DevTools Issues: 7 × benign "form field element should have an id or name attribute" (autofill heuristic).

## Untestable / not exercised
- Inspector timeline / two-run compare / replay (blocked by bug #1).
- Shell and `git` execution (blocked by issue #7 — known launcher limitation).
- Deep Research run, Tasks scheduling, Memory/Wiki writes, Breakpoints, "Rewind to here", Projects/Projects dialog, Extension/MCP install flow — pages render but flows weren't exercised.

## Artifacts
- Recording: `C:\Users\Administrator\screencasts\rec-f435903e-5d39-4112-9f7d-5f4788f67ae0\rec-f435903e-5d39-4112-9f7d-5f4788f67ae0-edited.mp4`
- Key screenshots (all under `C:\Users\Administrator\screenshots\`):
  - `e2e-01-providers.png` — Settings → Providers, OpenRouter card with masked key
  - `e2e-02-toolcard-expanded.png` — read_file tool card expanded (args + result)
  - `e2e-03-approval-card.png` — approval card on write_file (Allow/Deny + don't-ask-again scope)
  - `e2e-04-after-approval.png` — run completed after approval
  - `e2e-05-askuser-answered.png` — ask_user answered; follow-up read_file approval
  - `e2e-06-askuser-done.png` — run continued after the answer
  - `e2e-07-capability-network-allow.png` — capability matrix with network=Allow
  - `e2e-13-shell-still-blocked.png` — shell error card even with network=Allow
  - `e2e-14-subagent-output.png` — subagent run output dialog
  - `e2e-15-inspector-empty-runs.png` — Inspector run dropdown empty (bug)
  - `e2e-16-fork-reply.png` — forked conversation #5 reply
  - `e2e-17-multimodal.png` — image attach + vision-correct answer
  - `e2e-18-badmodel-failed.png` — bogus model run shows Failed w/o reason text
  - `e2e-19-recovered.png` — recovery after switching the model back
