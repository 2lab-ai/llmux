# Vertical traces

The trace is the source of truth. Status starts Specified. Contract tests are
derived before the production path and become Verified only after code, tests,
runtime evidence, and the file-map gate agree. Feature-branch commits remain
green as required by the repository build gate.

Each Observability section below names evidence that is implemented in typed
effects, canonical state, or verification receipts. This port does not claim a
separate structured tracing backend; fields not present in those artifacts are
not emitted as runtime logs.

## T1 — Dashboard poll to semantic UI and request receipts

### 1. API Entry

GET /llmux/dashboard. Loopback HTTP is exempt from API-key auth; a production
remote endpoint requires HTTPS and uses x-api-key from executor-only settings.
The explicitly named insecure-remote constructor exists only for tests/dev.

### 2. Input

Endpoint URL must be loopback http or https, host non-empty, port 1..65535;
non-loopback http is rejected by production configuration. The response must
be a JSON DashboardDoc; additive fields may be absent.

### 3. Layer Flow

PollTick → FetchDashboard(request_id) → HTTP DashboardDoc → derive::dashboard
→ UiState.usage/statistics/window. DashboardDoc.activity.completed[*] →
receipts::from_activity → ActivityReceipt[*]. DashboardDoc.accounts[*].name +
email_anonymous → privacy::display_account → AccountTile.display_name and
ActivityReceipt.account_display.

The document is projected once through the core; the reducer retains the
authoritative action-handle map and QML receives only serialized UiState.

### 4. Side Effects

last_success_ms and tray counters change. No daemon or filesystem state changes.
A failed poll schedules bounded retry while preserving the last good snapshot.

### 5. Error Paths

Invalid URL prevents HTTP. Timeout/connect/HTTP/decode failures produce a
sanitized connection error and stale/offline state. Missing additive fields
become unavailable/empty. A stale request_id produces no state change.

### 6. Output

Ready/offline UiState JSON conforming to ui-contract.schema.json. Receipts
contain metadata only; secrets and request/response bodies are absent.

### 7. Observability

The request-correlated `FetchDashboard` effect carries `request_id`; canonical
connection state carries lifecycle, last-success time, and sanitized failure.
Headers and response bodies are never copied into UiState or receipts.

## T2 — Login, Grok device verification, and terminal receipt

### 1. API Entry

POST /llmux/login/start, GET /llmux/login/status?state=..., and
POST /llmux/login/cancel, with the same auth policy as T1.

### 2. Input

provider is claude, codex, or grok. state must match the active operation.
verification_uri must be http/https before OpenUrl. Poll deadline is five
minutes and only one login may be active.

### 3. Layer Flow

LoginStarted.provider → StartLoginRequest.provider → daemon LoginProvider →
StartLoginResponse.state → LoginState.state. LoginStatusResponse.phase +
verification_uri + user_code → reducer LoginProgress → UiState.usage.login.
Terminal LoginStatusResponse → VerificationReceipt.outcome/message →
RefreshRequested.

The API key and provider credentials never cross into state; the user code is
ephemeral and cleared on terminal state.

### 4. Side Effects

The daemon opens/runs OAuth or device-code flow and injects the account on
success. The client polls, may open/copy the verification data, cancels on user
request, refreshes dashboard on success, and appends one terminal receipt.

### 5. Error Paths

Unknown provider is rejected locally. HTTP 409 shows already-in-progress.
Unknown state is terminal failure. Transient poll errors retry until deadline.
Cancel moves the core to `cancelling`, stops polling immediately, and ignores a
late same-id success until the typed cancel acknowledgement. The five-minute
deadline is enforced again in the reducer, independent of the Linux timer.

### 6. Output

Pending progress, optional Grok URI/code, then exactly one succeeded, failed, or
cancelled VerificationReceipt. Success names the privacy-safe account.

### 7. Observability

Canonical login state carries provider and phase while active. The terminal
verification receipt carries operation id, outcome, sanitized message, and
start/finish timestamps. It excludes user_code, verification query strings,
tokens, and raw error bodies.

## T3 — Account pause/remove/add mutation and verification

### 1. API Entry

POST /llmux/add-account, POST /llmux/pause-account, or
POST /llmux/remove-account.

### 2. Input

Add requires non-empty API key and optional trimmed name. Pause requires an
existing core-issued account handle and boolean. Remove requires the same
handle plus an explicit confirmation action; the core alone resolves real ids.

### 3. Layer Flow

OperationStarted.request.account_id → core real-id lookup → daemon account key
→ live pool mutation. Linux retains AddAccount's `SecretString` only through
the executor effect; the macOS wire action carries `has_api_key` while Swift
retains the actual value by operation id. It never enters UiState. HTTP result
→ OperationFinished(operation_id) → reducer busy clear → FetchDashboard →
VerificationReceipt.

### 4. Side Effects

The daemon persists and hot-applies account config. Client disables only the
affected control, refreshes authoritative state, and appends one receipt.

### 5. Error Paths

Unknown/masked id, missing key, duplicate busy operation, or unconfirmed remove
causes no HTTP and no daemon change. HTTP conflict/not-found/unauthorized/error
restores controls and records sanitized failure. Stale results cause no change.

### 6. Output

Mutation acknowledgement followed by dashboard-derived state. Receipt outcome
does not claim the final account list until the refresh succeeds.

### 7. Observability

The terminal verification receipt carries operation id, operation kind,
privacy-safe target, outcome, sanitized message, and start/finish timestamps.
API keys, raw account ids, and request/response bodies are excluded.

## T4 — Settings/events/autostart platform mutations

### 1. API Entry

POST /llmux/settings for email_anonymous; POST /llmux/events for upsert/remove;
local platform adapter for XDG autostart and local preferences.

### 2. Input

Event id/content are non-empty; `from`/`to` are daemon-format strings
(RFC3339-with-offset or compact `YYYYMMDDHHMM`) that parse with `from < to`.
Connection port is valid. Autostart command and desktop file path are fixed by
the package, not arbitrary user input.

### 3. Layer Flow

EmailAnonymousChanged.enabled → SettingsRequest.email_anonymous → daemon live
config → next DashboardDoc.email_anonymous. EventDraft.{id,from,to,content} →
EventsRequest.{id,from,to,content} → daemon config.events row.
AutostartChanged.enabled → SetAutostart.enabled → fixed
io.twolab.LlmuxIslands.desktop file state.

### 4. Side Effects

Daemon config and live holders change for server settings/events. XDG autostart
is created/removed idempotently. Local display preferences are atomically
written with 0600 permissions. Each terminal action appends a receipt.

### 5. Error Paths

Validation prevents side effects. Network/pre-rename write failures leave the
prior state and surface a sanitized failure. Temp+sync+readback+rename prevents
a truncated settings file; a post-rename directory-sync uncertainty is not
misreported as if the old file remained. Removing an absent autostart entry is
no_change. Settings and autostart paths reject symlinks.

### 6. Output

Refreshed server state or re-read local platform state and a terminal
VerificationReceipt.

### 7. Observability

The terminal verification receipt carries operation id, the
setting/event/autostart kind, outcome, sanitized message, and timestamps. Event
content, API keys, and full local paths are excluded from the receipt.

## T5 — Arch maintenance/channel operation

### 1. API Entry

Local RunMaintenance effect. It may inspect pacman ownership. A Linuxbrew
install resolves `brew` and `llmux` through verified absolute prefixes; it
never PATH-resolves `llmux`.

### 2. Input

Requested operation is update or channel stable/preview. Channel change
requires ChannelChangeConfirmed. Download source must be an allowed official
release URL and checksum/signature must match.

### 3. Layer Flow

UpdateRequested/Confirmed.channel → MaintenanceCommand.kind/channel →
InstallOwner detection → PackagePlan. pacman-owned path → Instruction result;
Linuxbrew path → absolute CLI delegation; self-managed path → signed-manifest
required/no-change. Result → VerificationReceipt.message.

### 4. Side Effects

pacman-owned: no binary mutation. Linuxbrew: delegate only to the absolute
verified formula CLI. Self-managed: no production mutation until an official
signed manifest protocol/key exists. Channel preference is persisted only
after successful delegation.

### 5. Error Paths

Unknown owner, root/system target, symlink path, wrong uid, missing checksum,
mismatch, network failure, or pre-rename failure leaves binaries/channel
unchanged and returns failure.
Stable publication remains an external user gate.

### 6. Output

Instruction, succeeded update, no_change, or sanitized failure plus a terminal
VerificationReceipt. The UI remains usable.

### 7. Observability

The terminal verification receipt carries operation id, maintenance kind,
privacy-safe target, outcome, sanitized message, and timestamps. Package-owner
and channel guidance may appear only in the sanitized message; auth headers and
credential-bearing download URLs are excluded.

## Contract tests derived before implementation

| Trace | Happy path | Sad path | Side-effect | Transformation contract |
|---|---|---|---|---|
| T1 | current fixture yields exact state snapshot | malformed/old/offline preserves last good | tray/retry effects | account/activity/privacy arrows |
| T2 | Grok pending→verification→done | 409, stale state, timeout, cancel race | poll stop, refresh, one receipt | provider/state/phase arrows |
| T3 | add/pause/remove result then refresh | no key, no confirmation, stale id, HTTP fail | busy + receipt + refresh | real id/secret executor arrows |
| T4 | server/local settings and event/autostart | invalid interval/write/network fail | atomic persistence/idempotency | action fields to daemon/file |
| T5 | pacman instruction or absolute Linuxbrew delegation | unknown owner/system path/checksum mismatch | no pacman mutation; protected atomic primitive | request→owner→plan→receipt |

RED is an iteration-stage observation, not a deliberately broken branch commit.
Tests are not weakened to obtain GREEN, and every saved commit must pass the
repository build gate.

## Implementation status

| Unit | Status | Verification |
|---|---|---|
| Audit and platform decision | Specified | Source inventory + official docs |
| Machine-readable UI schema | Verified for core state | `tests/contract.rs` validates representative serialized state against the checked-in schema |
| Rust protocol/reducer core | Verified through T4 HTTP/effect boundary | 46 contract/fixture/reducer/HTTP tests cover state, routes, HTTPS/auth, redirects, privacy handles and free-text masking, window lifecycle, cancellation/deadline races, bounded retry, refresh ordering, receipts, and typed effects |
| T4 settings/events/autostart | Verified at adapter boundary | Daemon settings/events plus private atomic settings, symlink-safe idempotent XDG autostart, validation, and terminal receipts |
| KDE Qt/QML shell | Verified in clean Arch container | Usage, Statistics, Menu, tray, canonical controller, semantic dispatch, static QML resource registration, and bounded offscreen smoke all execute |
| macOS C ABI | Verified at native caller boundary | Eleven Rust ABI tests cover every action/effect family, request correlation, opaque ids, secrets, platform-state persistence, invalid buffers, panic containment, and ownership; a target-14 C caller links and runs with linker warnings fatal, and its load command reports `minos 14.0` |
| macOS SwiftUI/AppKit shell | Shared-core runtime integrated | Strict schema decoder, C ABI ownership wrapper, canonical projections, executor effects, hardened endpoint policy, and Xcode build integration; macOS CI is the authoritative full build because this development host has Command Line Tools but not Xcode |
| Tray/layer-shell/notifications | Verified at adapter/build boundary with platform fallbacks | Qt `SystemTrayIcon` with Plasma StatusNotifierItem backend and native message activation, LayerShellQt Wayland, positioned X11, regular fallback, freedesktop sound |
| Arch maintenance/package | Verified fail-closed | Pacman instruction-only, absolute Linuxbrew delegation, protected checksum primitive, and an unprivileged `makepkg` release build followed by package installation and installed-binary smoke in the clean Arch image |
| Runtime parity | Automated gates verified | Linux tests plus clean Arch CXX-Qt compile, metadata validation, QML lint, warning-free offscreen smoke, and three distinct 960x760 UI snapshots uploaded with a SHA-256 manifest; manual Plasma Wayland/X11 acceptance remains a release checklist |

Verification receipt (2026-07-14): the shared core passes 46 contract, fixture,
reducer, and HTTP tests; the macOS bridge passes eleven ABI tests plus its native
C smoke executable; the Linux crate passes 85 tests both locally and in a fresh
Arch container. The existing workspace baseline remains green with 766 unit
tests and 38 E2E tests (one ignored by design).

The clean Arch image produces real Usage, Statistics, and Menu captures at
960x760. CI exports all three PNGs plus `SHA256SUMS` as the
`llmux-islands-kde-snapshots` artifact, so the evidence and its run-specific
hashes remain reproducible instead of relying on stale values copied into this
document.

## Required file map

Expected changed surfaces:

- .prd/docs/llmux-islands-linux-port/*
- llmux-islands-core/**
- llmux-islands-linux/**
- llmux-islands-macos-bridge/**
- llmux-islands/LlmuxIslands/SharedCore/**
- llmux-islands/scripts/build-rust-core.sh
- macOS model/view/project and contract-security tests required to adopt the core
- .github/workflows/linux-islands.yml
- .dockerignore (keeps nested Cargo targets out of the root Arch build context)
- README.md and release docs only when installation is actually available

Any additional production file must first be added here with a trace reason.

## Trace deviations

- macOS retains its production SwiftUI/AppKit presentation but uses the shared
  Rust reducer at runtime. Native windowing and platform executors intentionally
  remain shell-owned; they are not duplicated widget semantics.
- Self-managed Linux binary replacement remains unavailable in the production
  controller until an official signed release-manifest protocol and public key
  are defined. Pacman and Linuxbrew ownership paths are functional now.
