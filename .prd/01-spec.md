# llmux — Product specification

Status: **maintained, shipped**. This contract describes the `0.2.x` product
(repository version `0.2.17` at the 2026-07-15 audit). Update it when the
product boundary changes.

Historical note: early proxy/OAuth mechanics drew from
[KarpelesLab/teamclaude](https://github.com/KarpelesLab/teamclaude) (MIT). The
current system is a Rust multi-provider daemon with native macOS and Linux/KDE
clients.

## Product statement

llmux keeps Claude Code as the stable agent harness and makes the account/model
layer swappable behind one Anthropic-compatible endpoint. It combines
multi-account quota scheduling, model-to-provider routing, Responses-family
translation, daemon lifecycle, and observable local control surfaces for one
human using their own accounts.

## Problem

An agent harness accumulates durable value: tool permissions, repository
instructions, subagents, MCP servers, hooks, context behavior, and user muscle
memory. Moving to another provider CLI to try a model duplicates that
investment and causes behavioral drift.

Subscriptions create a second problem. Quota is split across fixed 5-hour,
weekly, and provider-specific windows. Unused capacity disappears at reset,
while switching accounts too often destroys prompt-cache locality. Manual
rotation cannot balance both concerns reliably, and idle OAuth credentials
must be refreshed even when no foreground dashboard is open.

## Product principles

1. **Preserve the harness.** Claude Code remains the client and owns agent
   behavior; llmux owns the downstream traffic boundary.
2. **Route before scheduling.** A model chooses a compatible provider group;
   the scheduler chooses an account only inside that group.
3. **Treat quota as perishable.** Capacity near reset is more urgent, but
   account stickiness still protects prompt caches.
4. **One daemon is the truth.** Credentials, leases, usage, settings, history,
   and receipts are daemon-owned. UIs are clients.
5. **Make trust explicit.** Loopback is trusted; off-loopback requires the
   daemon key and an operator-provided encrypted transport.
6. **Keep platforms native.** macOS and KDE share semantics, not a widget tree.

## Goals

1. Drop-in Claude Code proxying through `ANTHROPIC_BASE_URL`.
2. Multiple personal accounts across Claude, Codex, and Grok.
3. Quota-aware, health-aware, request-safe account selection.
4. Live provider/model switching without moving the user to another harness.
5. Persistent credential refresh and daemon operation.
6. Terminal and native visibility into health, usage, cost, activity, and
   operator actions.
7. Local CLI use plus an explicit one-daemon/many-client remote topology.
8. Reproducible macOS/Linux CLI builds and native companion verification.

## Non-goals

- Hosted or multi-tenant service operation.
- Team credential pooling, subscription resale, or brokerage.
- TLS termination or encrypted overlay management.
- Request-content/task-type routing; model names are the routing signal.
- Replacing Claude Code's context accounting, compaction, tools, or permission
  model.
- Pretending all providers expose identical quota windows.
- A portable cross-platform UI widget toolkit.
- Shipping the Linux Islands GUI through AUR/pacman/xbrew until a separate
  packaging decision is made.

## Functional requirements

### FR1 — Proxy ingress and trust boundary

- Listen on configurable port `3456` by default. The daemon binds all
  interfaces so remote clients are possible.
- Exempt loopback peers from the llmux client API key. Require
  `proxy.api_key` as `x-api-key` for every off-loopback route, including the
  data plane and `/llmux/*` control plane.
- Accept Anthropic Messages-style traffic and relay streaming responses with
  backpressure and disconnect handling.
- Bound buffered request bodies at 64 MiB by default and return 413 when the
  cap is exceeded.
- Bound post-connect upstream silence at 120 seconds by default without
  imposing a total streaming deadline.
- Strip client provider credentials and inject only the leased account's
  credential.
- Answer root reachability probes locally and relay `/v1/oauth/token` under
  the same client-auth boundary.
- Expose status, dashboard, model catalog, settings, account, login, scheduler,
  event, switch, and shutdown control routes required by shipped clients.

### FR2 — Accounts and credentials

Support four durable credential kinds:

| Type | Source | Identity |
| --- | --- | --- |
| `oauth` | Claude PKCE browser login / Claude import | `account_uuid`, then name |
| `apikey` | Anthropic API-key entry | name |
| `codex` | ChatGPT/Codex browser OAuth or Codex auth import | `account_id`, then name |
| `grok` | xAI device-code OAuth | OIDC subject, then name |

- Store config at `~/.config/llmux.json` by default, mode `0600`.
- Reload immediately before each config mutation and replace the file
  atomically. This prevents torn files but does not serialize overlapping
  cross-process writers, which can still be last-write-wins.
- Refresh OAuth-style access tokens before expiry and once after eligible 401
  responses, persisting rotated tokens and last-refresh evidence.
- Never expose provider credentials through dashboard/status documents.
- A pre-Grok binary cannot parse a config containing Grok tagged records;
  operators must remove those records before downgrading.

### FR3 — Model routing

- Route Claude-like names to Claude accounts, `gpt-*`/Codex aliases to Codex,
  and `grok*` to Grok by default.
- Maintain group-scoped current selections plus a separate Fable-scoped Claude
  current slot.
- Allow operator rule replacement per group with prefix, substring, and exact
  tokens.
- Route unmatched/model-less requests to `routing.default_group` (`claude` by
  default).
- When a matched group has zero configured accounts, return a clean 404 by
  default; optional fallback scans Claude → Codex → Grok.
- Keep a legacy routing-disabled mode with no group filter and Codex overflow.

### FR4 — Scheduling

- Track credential health, operator pause, in-flight leases, cooldowns, quota
  windows, evidence freshness, and global/per-account ceilings.
- Pin every request to one account for its lifetime. Selection changes must not
  migrate or cancel in-flight work.
- Default eligibility ceilings: 5h `0.90`, 7d `0.99`, Fable weekly `0.98`.
- Combine Claude OAuth polling with passive response-header observation.
- Populate cold Codex/API-key windows with an optional bounded one-token idle
  probe. Never idle-probe Grok or paused accounts.
- Honor upstream 429 retry information and park the account. Explicit
  `retry-after` parks last for their stated duration; fresh capacity evidence
  can heal heuristic parks created when that signal was absent.
- Ship two modes:
  - `default`: servable headroom × weekly reset urgency, sticky unless another
    eligible account scores more than 25% better;
  - `round-robin`: stay until hard-ineligible, then advance in roster order.
- If all comparable evidence is stale, degrade to available header evidence
  rather than deadlocking every account.

### FR5 — Provider behavior

#### Claude

- Preserve the Anthropic request/response path except credential/header
  replacement and removal of the client-only `[1m]` model annotation.
- Observe rate-limit and usage evidence without translating Anthropic SSE.

#### Codex

- Convert Anthropic Messages input and tool calls to OpenAI Responses input and
  convert Responses SSE back to Anthropic Messages events.
- Pass known concrete Codex IDs through verbatim.
- Resolve `sol`/`terra`/`luna` to the current `gpt-5.6-*` generation and bare
  `gpt-5.6` to `gpt-5.6-sol`.
- Use `codex.default_model` (default `gpt-5.6-sol`) only for absent, decorated,
  or unknown requested models.
- Support priority service tier and model-clamped reasoning effort. A
  configured effort overrides the client; unset/bypass preserves it.
- Preserve text, reasoning summaries, and tool calls. Drop unsupported images
  with an observable warning.

#### Grok

- Authenticate through xAI device-code OAuth and persist the discovered token
  endpoint for refresh.
- Reuse the Responses translation core with Grok-specific headers, effort
  clamping, SSE labeling, pricing, and rate-limit parsing.
- Resolve bare `grok` to `grok.default_model` (`grok-4.5`); pass concrete
  `grok-*` names verbatim.
- Expose no Codex priority tier and no fabricated Claude-style usage poller.

### FR6 — Daemon and CLI

- `llmux run` probes, starts/reuses a local daemon, waits for readiness, and
  launches Claude Code.
- `server` renders a local TUI on a TTY or plain logs otherwise; if a daemon
  already owns the port, attach instead of rebinding.
- `dashboard` renders the same dashboard contract from an existing daemon.
- Support login/import, account/status, stop/restart, env, debug API,
  stable/preview channel, and update commands.
- Remote mode comes from global `--remote` or persistent `remote.host`.
- In remote mode, read/attach commands target the remote, host-owned lifecycle
  and credential mutations refuse, and channel/update remain local. The
  direct-upstream debug `api` command also remains local and uses local config.

### FR7 — Observability and privacy

- Publish compact status and richer dashboard documents from one event fold.
- Show account/quota health, provider settings, in-flight and completed
  activity, sessions, model/token/client usage, calendar buckets, and
  API-equivalent cost with explicit unpriced states.
- Persist append-only activity metadata and hydrate older local pages on
  demand.
- Capture raw request/delivered-response payloads best-effort when `raw_io` is
  enabled (default), prune to 90 days by default, and clip each body at 8 MiB.
- Append tagged Codex and Grok Responses traces to the shared
  `codex-trace.jsonl` when each provider flag enables it.
- Provide deterministic demo aliases and durable display-only email
  anonymization. Never include secrets or raw bodies in visual receipts.

### FR8 — Native companion

- Share a versioned Rust semantic state, reducer, HTTP client, privacy
  projection, typed actions/effects, and receipts across platforms.
- macOS uses SwiftUI/AppKit through a stable C ABI bridge and preserves the
  native notch interaction.
- Linux uses Qt 6/QML/Kirigami through CXX-Qt, LayerShellQt when supported,
  Plasma tray integration, and explicit Wayland/X11 fallbacks.
- Allow loopback HTTP; require HTTPS plus API key for remote native-client
  endpoints and deny redirects.
- Keep provider credentials daemon-owned and remote connection credentials in
  native settings, never semantic UI state.

### FR9 — Distribution and verification

- Stable tags and preview pushes build four CLI binaries:
  macOS aarch64/x86_64 and Linux aarch64/x86_64.
- Stable and preview releases package the macOS Islands app alongside CLI
  binaries and checksums.
- Homebrew exposes stable/preview CLI formulae and macOS Islands casks.
- Linux Islands is built, packaged, installed, smoke-tested, and screenshot-
  verified in clean Arch CI.
- The current Linux GUI user distribution is the repository
  `llmux-islands-git` PKGBUILD. It is not a GitHub release asset or published
  xbrew/AUR/pacman recipe.

## Acceptance

The shipped contract is accepted when:

1. Claude Code can complete streamed tool-using requests through each
   configured provider group.
2. Routing never leases an incompatible credential and request leases survive
   concurrent selection changes.
3. Scheduler gates, status, dashboard, TUI, and native clients agree on account
   health and current selection semantics.
4. OAuth refresh, 429 cooldown, stale evidence, and process restart recover
   without credential loss or mid-stream account migration.
5. Off-loopback requests fail without the daemon key and succeed with it over
   an operator-provided trusted transport.
6. Remote commands target/refuse/stay-local according to their documented
   ownership boundary.
7. Privacy modes mask identities, raw capture is explicit in config/docs, and
   durable screenshot receipts contain no secrets.
8. CI validates the Rust daemon, shared Islands core, macOS bridge/app, and
   clean-Arch Linux package/render path.

## Compliance boundary

llmux is for one human using accounts they control. Subscription-token
compatibility depends on provider policy and can change. Operators should keep
an API-key fallback for durable operation. llmux is not affiliated with
Anthropic, OpenAI, or xAI.

## Related records

- [Current architecture](02-architecture.md)
- [Scheduler perishability](09-scheduler-perishability.md)
- [Remote CLI decision](13-remote-cli.md)
- [Calendar usage/cost](14-usage-calendar-stats.md)
- [Cross-platform Islands dossier](docs/llmux-islands-linux-port/README.md)
- [Decision archive index](README.md)
