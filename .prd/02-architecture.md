# llmux — Architecture

Status: **maintained, shipped**. Rust 2021, one daemon/CLI binary plus a shared
Rust native-UI core and platform-native macOS/KDE shells. This record describes
the source tree and runtime at the 2026-07-15 documentation audit.

The product-level contract is [01-spec.md](01-spec.md). The reader-oriented
overview and rendered diagram are in [docs/architecture.md](../docs/architecture.md).

## Design spine

- Claude Code speaks Anthropic Messages to one endpoint.
- Routing classifies the requested model into Claude, Codex, or Grok.
- One `AccountPool` owns all accounts, with group-scoped selection state and a
  separate Fable-scoped Claude slot.
- Pure selection functions consume snapshots; runtime code revalidates and
  leases the chosen account.
- Provider-specific conversion is isolated from the Anthropic passthrough path.
- One dashboard fold produces stable documents for local TUI, attach, and
  native clients.
- Native shells share semantic Rust state/actions/effects, not UI widgets.

## Repository layout

```text
src/
  main.rs, lib.rs            process entry and crate exports
  cli/                       commands, local/remote endpoint resolution,
                             daemon lifecycle, brew channel/update
  config/                    v1 schema, migration, private atomic persistence
  auth/                      Claude PKCE, Codex OAuth/import, Grok device code,
                             profile and credential imports
  routing.rs                 model → BackendGroup classifier
  scheduler/                 account state, windows, header/usage evidence,
                             idle-probe policy, pure selection
  provider/
    anthropic.rs             Anthropic passthrough + client annotation cleanup
    responses.rs             shared Messages ↔ Responses conversion/SSE core
    codex.rs                 Codex model/effort/auth adapter
    grok.rs                  Grok model/effort/auth adapter
    stubs.rs                 non-shipping design stubs
  proxy/
    server.rs                listener, auth layer, routes, background tasks
    forward.rs               classify, lease, refresh, dispatch, retry, record
    sse.rs                   streaming relay and transform boundary
    idle_probe.rs            bounded cold-account request construction
    raw_io.rs                best-effort payload capture and pruning
    codex_trace.rs           shared tagged Responses trace writer
    classify.rs              activity/request-family classification
    logging.rs               credential-masked debug logs
    login.rs                 daemon-side interactive-login coordination
  dashboard.rs               DashboardHub and stable status/dashboard DTOs
  catalog.rs                 curated model document + dynamic Grok pin row
  pricing.rs                 built-in/override API-equivalent costs
  session.rs                 session projection
  event.rs                   operator banner parsing/selection
  demo.rs                    stable privacy-safe aliases
  tui/                       event loop, views, overlays, activity/history UI
  build_info.rs, logging.rs  build identity and process logging

llmux-islands-core/          versioned semantic UI state, daemon client,
                             reducer, privacy, derived state, receipts
llmux-islands-macos-bridge/  C ABI owner for Swift
llmux-islands/               SwiftUI/AppKit macOS shell
llmux-islands-linux/         CXX-Qt + QML/Kirigami KDE shell and Arch package
tests/                       daemon/provider end-to-end acceptance
```

## Runtime topology

```text
Claude Code / SDK client
          │ Anthropic Messages HTTP + SSE
          ▼
┌──────────────────────────── llmux daemon ────────────────────────────┐
│ client-auth layer: loopback exempt; off-loopback x-api-key required │
│                                                                     │
│ request classify ─► model router ─► scheduler snapshot/lease        │
│                                      │                              │
│                     ┌────────────────┼────────────────┐             │
│                     ▼                ▼                ▼             │
│              Anthropic path    Codex adapter     Grok adapter       │
│                     │          Responses bridge  Responses bridge   │
│                     └────────────────┼────────────────┘             │
│                                      ▼                              │
│                     upstream response / transformed SSE             │
│                                      │                              │
│        headers + usage + outcome ─► pool + dashboard + history      │
│                                                                     │
│ background: usage poll · refresh · idle probe · reset/cooldown heal │
│ control: /llmux/* · /models · login orchestration · shutdown        │
└─────────────────────────────────────────────────────────────────────┘
          ▲ dashboard/status/actions
          │
 CLI/TUI · remote attach · macOS Islands · KDE Islands
```

The listener binds `0.0.0.0:<port>`. `127.0.0.1:3456` is the normal client
address, not an exclusive bind. A shared router layer applies client auth to
explicit control/catalog routes and the fallback proxy data path.

## Process roles

The same `llmux` executable has server and client roles:

- `server` owns the listener and background work. On a TTY it also renders the
  in-process TUI.
- `run` probes a target, starts a detached local server when appropriate, and
  launches Claude Code.
- `dashboard` polls `/llmux/dashboard` and renders the same view model without
  binding.
- `status`, `accounts`, and `env` are short-lived clients.
- Remote mode resolves a central endpoint. Read/attach commands target it;
  lifecycle/account mutation refuses; channel/update and the direct-upstream
  debug `api` command remain local to the client machine.

Detached server stderr goes to `$XDG_STATE_HOME/llmux/server.log`. Readiness is
proven by the HTTP status endpoint, not only by process existence.

## Client authentication

`client_auth` runs after routing so it protects every route, including the
fallback data path.

- Loopback IPv4/IPv6 peers are trusted and may omit the key.
- A non-loopback peer must present the configured `proxy.api_key` as
  `x-api-key`.
- Missing peer metadata is not treated as loopback.
- The gate is constant-time enough for the local ownership role but is not a
  multi-tenant authentication system.
- The transport is HTTP. Encryption belongs to Tailscale/WireGuard/TLS outside
  llmux.

## Request flow

1. The server admits and buffers the request body up to
   `proxy.max_request_bytes`, because a retry may need to replay it.
2. Activity classification and model extraction happen once at forward entry.
3. `routing::Classifier` maps the model to `BackendGroup`.
4. Scheduler code creates a group/scoped snapshot, evaluates eligibility and
   ranking, and chooses an account.
5. Runtime code acquires an `AccountLease`, increments in-flight state, and
   clones the credential. The lease pins the account through completion or
   disconnect.
6. Near-expiry refresh occurs before dispatch. One eligible 401 path may force
   a refresh and retry.
7. Auth/header rewrite and provider conversion produce the upstream request.
8. The upstream response is classified for retry, cooldown, or direct relay.
9. SSE is relayed with backpressure. Codex/Grok use the shared Responses
   converter; Claude remains Anthropic-shaped except auth/header replacement
   and `[1m]` model annotation normalization.
10. Header windows, emitted usage, activity outcome, token/cost data, trace/raw
    capture, and dashboard state are recorded best-effort around the response.
11. Dropping the lease decrements in-flight state. A concurrent current-account
    change never moves the request.

## Routing

`BackendGroup` has a stable order `Claude < Codex < Grok`. Builtin classifier
families are intentionally disjoint; evaluation precedence is Codex, Claude,
then Grok.

Each group's empty config list retains its builtins. A non-empty list replaces
that group's builtins and parses tokens as prefix, `~substring`, or `=exact`.
Unmatched/model-less traffic uses `default_group`.

When `on_empty_group = "fallback"`, only a group with at least one configured
account qualifies; the scan order is Claude → Codex → Grok. Parked or limited
accounts still count as configured, so fallback never silently changes provider
semantics merely because a group is temporarily exhausted.

## Scheduler state and selection

`AccountPool` is backed by synchronized `PoolState`. It owns accounts, window
evidence, cooldowns, in-flight counts, operator pauses/limits, and current
selection:

- one current slot per backend group for ordinary traffic;
- one Fable-scoped Claude current slot;
- legacy scalar projection retained only for backward-compatible documents.

`scheduler/select.rs` contains deterministic eligibility, rank, and next-in-line
functions. Runtime state mutations revalidate assumptions rather than applying
an obsolete snapshot blindly.

### Evidence sources

- Claude: OAuth usage polling plus Anthropic unified response headers.
- Codex: `x-codex-*` traffic headers plus bounded idle probes when enabled.
- Anthropic API-key: response headers plus eligible idle probes.
- Grok: reset-less `x-ratelimit-*` observations with a provider-specific
  fallback horizon; no fabricated OAuth usage endpoint and no idle probing.

Freshest compatible evidence wins. Passed reset times read as empty. A 429
records an explicit `retry-after` park when present; without one, the runtime
uses a short heuristic, a Fable-scoped cooldown, or the Grok free-tier
estimate as appropriate. Fresh capacity evidence can heal heuristic parks.

### Modes

- `default`: ranks by `servable_now × urgency`, remains sticky, and proactively
  moves only when another account exceeds the current score by 25%.
- `round-robin`: remains until hard-ineligible, then advances in stable roster
  order with no score-based proactive move.

Fable eligibility adds the scoped weekly gate without poisoning non-Fable
traffic on the same account.

## Provider boundary

### Anthropic

The fast path preserves Anthropic wire semantics. It strips client auth,
injects OAuth/API-key auth, removes the client-only `[1m]` suffix when present,
and otherwise avoids Responses conversion.

### Shared Responses core

`provider/responses.rs` owns the common Anthropic Messages ↔ Responses
translation and streaming event converter. Provider adapters supply endpoint,
identity headers, model resolution, effort rules, service tier, and tracing
labels.

### Codex

Known concrete IDs pass through. Variant aliases and bare `gpt-5.6` resolve to
the current generation; other strings use the configured pin
`gpt-5.6-sol`. Configured effort overrides the client and is clamped per model.
`fast` maps to `service_tier: "priority"`.

### Grok

Concrete `grok-*` IDs pass through; bare `grok` uses the live pin
`grok-4.5`. The adapter uses xAI OAuth/device identity and rate-limit rules,
has no priority tier, and shares the tagged trace file with Codex.

## Background work

Tasks start beside the ready listener and remain bounded:

- Claude OAuth usage poll with provider/backoff handling;
- access-token refresh before `refresh_ahead_secs`;
- scheduler reset/cooldown re-evaluation;
- on-demand and periodic cold-account probing, sharing one cooldown gate;
- raw-I/O startup pruning;
- activity-history startup hydration and ongoing append;
- dashboard publication and login orchestration/state.

Cold probing is not a second scheduler. Its response headers feed the existing
window evidence path.

## Dashboard and persistence

`DashboardHub` folds request, scheduler, settings, login, and history events
into stable documents. Local TUI and attach render `DashboardView`, preventing
separate layout semantics for in-process and HTTP clients. Islands consumes the
same daemon dashboard and normalizes it through the shared UI core.

Durable stores:

| Store | Behavior |
| --- | --- |
| `~/.config/llmux.json` | private atomic config and credentials |
| `server.log` | daemon stderr/log stream |
| `activity.jsonl` | append-only request metadata; hydrated and paged locally |
| `raw-io.jsonl` | optional payload capture; startup retention pruning |
| `codex-trace.jsonl` | tagged Codex and Grok Responses traces |

Raw capture and traces are best-effort and may contain sensitive content. Their
write failure must not change the bytes delivered to the client.

## Concurrency invariants

- Request leases pin account/credential clones and maintain exact in-flight
  counters through `Drop`.
- Selection uses snapshots; state-changing application rechecks live
  preconditions.
- Claude OAuth refresh calls for one account coalesce; other OAuth adapters
  follow their provider refresh path without leaking tokens into UI state.
- Config mutation reloads before applying intent and atomically replaces the
  file. No cross-process lock/CAS serializes writers, so overlapping mutations
  can still lose an update.
- Activity/dashboard publication never blocks the network stream on durable
  evidence IO.
- Login orchestration permits only the concurrency supported by fixed callback
  resources and publishes explicit progress/cancel state.

## Native-client boundary

`llmux-islands-core` owns:

- typed daemon client operations and DTO normalization;
- versioned privacy-safe `UiState`;
- reducer, typed actions/effects, derived state, and receipts;
- account-handle anonymization and secret-exclusion invariants.

The macOS bridge owns the audited C ABI. Swift executes native effects and
renders the established notch/usage/statistics/menu hierarchy. Linux connects
the same Rust state machine through CXX-Qt and renders QML/Kirigami; platform
modules implement tray, window, screen, autostart, notification, sound, and
maintenance behavior.

Remote native clients accept loopback HTTP but require remote HTTPS plus key
and deny redirects. This is intentionally stricter than the CLI overlay path.

## Configuration boundary

`config/schema.rs` is the on-disk contract. Major groups are:

- `proxy`, including body/timeout/idle-probe safety;
- `scheduler` and per-account pause/limits;
- `routing` for three provider groups;
- `codex` and `grok` request shaping;
- `remote` client resolution;
- pricing, raw-I/O, events, and display/privacy flags;
- tagged account credentials.

Additive fields use serde defaults. Breaking tagged-enum additions require an
explicit downgrade note. User-facing defaults are centralized in
[docs/configuration.md](../docs/configuration.md).

## Failure semantics

| Signal | Result |
| --- | --- |
| Request body over cap | 413 before upstream dispatch |
| Off-loopback key missing/wrong | client-auth rejection on any route |
| 429 with retry information | park leased account; retry/switch according to scope |
| Refreshable 401 | one forced refresh path; repeated failure degrades auth and selection |
| Connect/5xx/idle timeout | provider-shaped transient failure; request-safe retry rules only |
| Matched group with no configured account | 404 by default; optional fixed-order fallback |
| All eligible accounts exhausted | 429 with nearest meaningful retry horizon |
| Malformed Responses stream | Anthropic-shaped error/termination, never raw vendor JSON |
| Native remote HTTP/redirect | rejected before credential-bearing follow-up |

## Build and verification topology

- Root `ci.yml`: Rust format, lint, tests, and platform jobs.
- `release.yml`: tagged stable four-architecture CLI build plus macOS Islands
  zip and checksums.
- `preview.yml`: the same CLI matrix and macOS app under timestamped prerelease
  identity, then tap dispatch.
- `linux-islands.yml`: shared core gates, clean-Arch package/install/snapshot
  evidence, macOS bridge ABI smoke, Xcode tests, and native screenshot evidence.

Linux Islands is not a release asset. The clean-Arch Docker build is CI
evidence; the user installation surface is the repository PKGBUILD.

## Porting pitfalls codified

- Do not describe the normal address `127.0.0.1` as an exclusive bind.
- Client auth protects the fallback proxy path as well as control routes.
- Router choice and provider upstream-model resolution are separate.
- `[1m]` is a client annotation, not an upstream context guarantee.
- Fable has a scoped selection slot/gate; it is not merely another Claude alias.
- Grok rate-limit evidence is not a Claude 5h/7d poller.
- Activity history is append-only; only raw-I/O has retention pruning.
- Codex and Grok share the trace filename with provider tags.
- macOS/KDE share semantics, not visual parity.
- Docker parity builds are CI/maintainer tools, not the Linux install path.

## Related records

- [Product specification](01-spec.md)
- [Decision archive](README.md)
- [Scheduler perishability](09-scheduler-perishability.md)
- [Grok spec and trace](../docs/grok/)
- [Cross-platform Islands dossier](docs/llmux-islands-linux-port/README.md)
