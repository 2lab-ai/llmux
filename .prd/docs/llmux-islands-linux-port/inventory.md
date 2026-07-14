# Shipped surface and protocol inventory

This is the parity baseline for the Linux port. It is derived from the current
Swift sources, not from the older planned-only documents 11 and 12.

## Window and navigation

| Shipped macOS behavior | Evidence | Required Linux result |
|---|---|---|
| Closed top island with provider in-flight counters | NotchView.swift, IslandUsageModel.swift | Qt tray tooltip/menu/warning icon through Plasma's StatusNotifierItem backend, plus a compact closed surface when layer shell is available |
| Click, hover-delay, notification, alert, and boot open reasons | NotchViewModel.swift | Tray activation is primary; optional edge hover is additive; focus-loss closes; boot animation is preserved |
| Usage, Statistics, and Menu content modes | NotchView.swift, NotchMenuView.swift | Three equivalent QML/Kirigami routes with state retained while switching |
| Per-screen placement and dynamic height | NotchWindowController.swift, ScreenSelector.swift | QScreen selector plus LayerShellQt top anchor on Wayland; regular positioned window fallback on X11 |
| Deterministic demo and PNG snapshots | SnapshotMode.swift, DemoMode.swift | Demo fixture and headless/offscreen QML snapshot targets |

## Usage and account operations

| Feature | Current behavior | Linux parity |
|---|---|---|
| Polling | GET /llmux/dashboard every 10 seconds; manual refresh; status fallback | Same endpoint, interval, manual refresh, and offline state |
| Tiles | Provider, account, tier/status, token expiry, 5h/7d/Fable gauges and reset stamps | Same semantic fields and warning thresholds |
| Privacy | Server-owned email_anonymous when supported; demo aliases | Never place raw account names in display state when masking is enabled |
| Current/in-flight | Current account and per-provider counts | Same grouping and counters |
| Pause/resume | POST /llmux/pause-account with paused boolean | Context action, optimistic busy state, authoritative refresh |
| Remove | POST /llmux/remove-account with confirm=true | Confirmation dialog and authoritative refresh |
| API key add | POST /llmux/add-account | Kirigami form; secrets never enter logs, receipts, or persisted UI state |
| Claude/Codex login | start/status/cancel login state machine | Browser flow progress, cancel, success/error receipt |
| Grok login | Device-code URI and user code while daemon polls | Copy/open verification UI plus terminal verification receipt |

## Statistics and receipts

The shipped “recent activity” surface is the request receipt feature. It is
metadata-only: it does not expose prompt or response bodies.

| Surface | Required fields |
|---|---|
| Overview | requests, tokens, API-equivalent cost, error rate, account summary, top models |
| Heatmap | 24h and 72h best-effort token windows with the daemon-supplied data-quality label |
| Models | requests, ok/errors, in-flight, last use, cache read/create availability, serving accounts, cost |
| Clients | abbreviated client id, requests, tokens, errors, cost, last-seen when available |
| Health | status, credential type, cooldown/block reason, token expiry, refresh state |
| In-flight request receipt | id, method/path, account, provider/model, effort/fast, elapsed time |
| Completed request receipt | timestamp, status, method/path, account, model, tokens, cache availability, API-equivalent cost, duration |
| Note receipt | timestamp, text, error flag |
| Verification receipt | operation id, kind, target, start/end, outcome, human-safe message; no secret material |

Completed request receipts retain the dashboard order and cap. In-flight rows
remain distinct from completed rows. Cost and quality qualifiers are rendered
verbatim from the daemon contract.

## Menu, settings, and platform operations

| Shipped macOS item | Linux implementation |
|---|---|
| Screen picker | QScreen list; current/removed-screen fallback |
| Notification sound picker and preview | freedesktop notification sound-name preview; “none” supported |
| Email anonymous | POST /llmux/settings, then refresh |
| Show Fable weekly | local display preference |
| Host/port/API key and reconnect | XDG config, validation, explicit keep/replace/clear credential semantics, secrets stored with 0600 mode, and visible unauthenticated state after clearing a remote key |
| Update now and stable/preview channel | Package-aware maintenance adapter; never mutate a pacman-owned binary behind pacman |
| Events list/add/edit/delete | GET dashboard events plus POST /llmux/events upsert/remove |
| Launch at login | XDG autostart desktop entry or systemd user unit; idempotent |
| Accessibility row | Not required on Plasma because the port avoids global pointer monitoring; report “not required” |
| About/releases/source | Kirigami about/settings card and external-link effects |
| Quit | orderly poll cancellation, tray unregister, window close |

## Platform services

- Local daemon: locate the sibling llmux binary, start detached only for a
  loopback endpoint, and probe readiness. Never spawn for a remote endpoint.
- Tray: Qt `SystemTrayIcon` with Plasma StatusNotifierItem integration,
  activation, context menu, tooltip, and warning icon. Qt's portable API does
  not expose a StatusNotifierItem attention-status setter.
- Windowing: LayerShellQt on Plasma Wayland; X11 placement fallback. A normal
  Kirigami window remains available if layer shell is unavailable.
- Notifications: Qt's StatusNotifierItem-backed native tray message, with
  message activation routed back to the reducer; sound remains a freedesktop
  sound-theme adapter.
- Login/browser: QDesktopServices opens only URLs returned or approved by the
  daemon.

## Wire endpoints

| Method and path | Purpose |
|---|---|
| GET /llmux/status | connection fallback |
| GET /llmux/dashboard | all read state, analytics, activity receipts, events |
| POST /llmux/add-account | add API key |
| POST /llmux/remove-account | remove account |
| POST /llmux/pause-account | pause/resume account |
| POST /llmux/settings | email anonymity |
| POST /llmux/events | event upsert/remove |
| POST /llmux/login/start | Claude/Codex/Grok login start |
| GET /llmux/login/status | login and device-code progress |
| POST /llmux/login/cancel | cancel login |

Every remote request carries x-api-key when configured. Error bodies are
sanitized before entering semantic UI state.

## Explicit non-features

- No credential-file reads and no provider token handling in the GUI.
- No prompt/response body capture in receipts.
- No global mouse interception on Plasma.
- Legacy Swift views that are no longer reachable from the shipped navigation
  are not port requirements.
- A stable release or system package publication remains a user release gate.
