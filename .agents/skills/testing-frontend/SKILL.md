---
name: testing-frontend
description: How to run and visually test the cool-ai-harness React/Vite frontend on this Windows box — dev server, backend-down error-state testing, mobile viewport emulation, CDP screenshots.
---

# Testing the cool-ai-harness frontend

## Dev server
- Repo: `C:\Users\Administrator\repos\cool-ai-harness`, frontend in `frontend/`.
- Windows: use PowerShell (bash env is broken). npm must be invoked as `npm.cmd`, e.g. `npm.cmd run dev` in `frontend/`. Node/npm live on PATH via `C:\hostedtoolcache\node\20.19.0\x64`.
- Vite binds `127.0.0.1:5173` explicitly (see `vite.config.ts` — IPv6-only default fails curl on this Windows setup) and proxies `/api` → `http://127.0.0.1:8000` (`cool serve`).
- To test error/empty states, just run the dev server with the Rust backend down — every React Query call fails fast (client `retry: 1`) and each page shows `QueryErrorState`.

## Browser / CDP
- Chrome runs with remote debugging at `http://localhost:29229` (`/json` lists page targets; `webSocketDebuggerUrl` per page).
- File-backed screenshots: connect to the page target via `System.Net.WebSockets.ClientWebSocket` in PowerShell and send `Page.captureScreenshot` (a working script pattern is at `C:\Users\Administrator\ui-audit\cdp-shot.ps1` if still present — recreate from that recipe otherwise).

## Mobile viewport (<768px flips to mobile layout)
- Chrome's minimum window width on this box is ~516px — resize the window via P/Invoke `MoveWindow` to ~420px; it clamps to ~516 but that's still under the `max-width: 767px` breakpoint, so the real mobile layout activates (MobileAppBar on non-chat routes, hamburger → MobileDrawer).
- Alternative: CDP `Emulation.setDeviceMetricsOverride` for a true 390px viewport.
- Maximize/restore via `ShowWindowAsync(hwnd, 3)` — no wmctrl/xdotool on Windows.

## App facts useful for audits
- Routes in `src/App.tsx`; `/mobile-preview` renders outside `AppLayout` (no sidebar). Wildcard `*` redirects to `/`.
- `/settings?setup=provider` auto-opens the provider create dialog; closing it navigates to `/`.
- Dark mode: `.dark` class is toggled only by `lib/telegramWebApp.ts` (Telegram `colorScheme`) — there is no theme toggle in the UI by design.
- The Telegram SDK script loads unconditionally; in a plain browser its WebView shim still fires `postEvent` console logs (`web_app_ready`, `web_app_request_theme`, …) on every navigation — expected noise, not a bug.

## Console noise vs real errors
- Expected noise: `[vite] connecting/connected`, React DevTools info, `Telegram.WebView postEvent …` logs, and failed `/api` proxy fetches (net::ERR_CONNECTION_REFUSED) when backend is down.
- Real problems to look for: React render errors, key warnings, unhandled rejections.

## Devin Secrets Needed
- none
