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

## T6 — Minimal default path and contextual Advanced disclosure

### 0. Client Surface

Usage opens with connection/attention state, privacy-safe account identity,
primary quota, add account, and refresh. Statistics opens with summary metrics
and account overview. Settings opens with navigation plus screen, sound,
privacy, and launch-at-login preferences. Each platform provides a labelled
Advanced disclosure in the context it affects; snapshot receipt mode opens the
relevant Statistics disclosure so receipt evidence remains inspectable.

### 1. API Entry

None. Opening or closing Advanced is shell-local presentation state and never
calls an llmux endpoint.

### 2. Input

The only new input is a user activation of a keyboard-accessible disclosure.
It carries no account id, credential, endpoint, or persisted value.

### 3. Layer Flow

`UiState.usage/statistics/settings` → platform view summary. The same canonical
state → platform Advanced content when `shell.advanced_visible` is true.
`Advanced activation` → shell-local boolean → visibility only; it does not map
to `Action`, `Effect`, HTTP, persistence, or a receipt.

### 4. Side Effects

None. Existing mutations keep their T3–T5 effects only when their explicit
control is activated. Disclosure does not refresh, write settings, or start an
operation.

### 5. Error Paths

Offline, fatal, authentication-required, warning, critical, destructive
confirmation, and operation failure states stay visible while Advanced is
closed. Missing additive diagnostic data renders as unavailable only after the
relevant disclosure opens and never becomes a misleading zero.

### 6. Output

An inverted OpenAI-reference surface with one-level progressive disclosure:
black canvas, white ink, opacity-tier hierarchy, flat square internal surfaces,
system grotesque/sans typography, and whitespace instead of decorative cards.
Provider decoration and normal-state quota/navigation/focus accents are
monochrome; rare semantic warning, error, and success color is paired with text
or iconography. Both shells expose equivalent default and Advanced groups.

### 7. Observability

Default and Advanced/receipt production-renderer screenshots exist for macOS
and KDE. Both consume the same canonical privacy-safe `UiState` fixture and
fixed clock. Each exact seven-file set includes the production shell chrome,
selected navigation, and connection state. Contract tests assert the disclosure
label, default-hidden technical groups, preserved critical-state visibility,
and the absence of a semantic dispatch when disclosure is toggled.

## Contract tests derived before implementation

| Trace | Happy path | Sad path | Side-effect | Transformation contract |
|---|---|---|---|---|
| T1 | current fixture yields exact state snapshot | malformed/old/offline preserves last good | tray/retry effects | account/activity/privacy arrows |
| T2 | Grok pending→verification→done | 409, stale state, timeout, cancel race | poll stop, refresh, one receipt | provider/state/phase arrows |
| T3 | add/pause/remove result then refresh | no key, no confirmation, stale id, HTTP fail | busy + receipt + refresh | real id/secret executor arrows |
| T4 | server/local settings and event/autostart | invalid interval/write/network fail | atomic persistence/idempotency | action fields to daemon/file |
| T5 | pacman instruction or absolute Linuxbrew delegation | unknown owner/system path/checksum mismatch | no pacman mutation; protected atomic primitive | request→owner→plan→receipt |
| T6 | common path remains complete; Advanced reveals canonical detail | hidden critical/offline state is rejected | disclosure causes no daemon/persistence effect | UiState→summary/detail visibility only |

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
| Minimal/Advanced presentation hierarchy | Implemented; visual CI receipt pending | T6 contracts cover the cross-platform default path, contextual disclosures, semantic-color restraint, critical-state exception, local-only toggle behavior, and seven deterministic states per renderer |
| Runtime parity | Local T6 gates and macOS renderer verified; clean-platform re-verification pending | Linux no-default format, warning-free clippy, and 92 tests pass with `-j 2`; the full Swift app source typechecks against macOS 14 and a linked AppKit snapshot process emits the exact seven distinct full-shell PNGs. GitHub Actions remains authoritative for Xcode/XCTest, Qt/QML, Arch packaging, smoke, and final renderer captures |

Verification receipt (2026-07-14): the shared core passes 46 contract, fixture,
reducer, and HTTP tests; the macOS bridge passes eleven ABI tests plus its native
C smoke executable; the Linux crate passes 85 tests both locally and in a fresh
Arch container. The existing workspace baseline remains green with 766 unit
tests and 38 E2E tests (one ignored by design).

T6 local verification receipt (2026-07-15): `cargo fmt --check`, warning-free
no-default `cargo clippy`, and all 92 Linux shell tests pass with `-j 2`;
`xcodegen generate` and a complete macOS 14 `swiftc -typecheck` pass, apart from
one pre-existing actor-isolation warning in `ScreenObserver`. A directly linked
AppKit snapshot process also emitted the exact seven-file macOS contract from
the canonical fixed fixture; all files were distinct and visually inspected for
shell chrome, default/Advanced hierarchy, monochrome provider treatment, and
uncropped content. The shared-core, macOS, and KDE dashboard fixture files are
byte-identical and guarded transitively by the macOS bridge and Linux shell
contracts. Re-rendering with conflicting host connection, API-key presence,
sound, and Fable defaults produced byte-identical copies of all seven macOS
PNGs. Snapshot mode also skips the live CLI channel query, so the receipt set
does not inherit host preferences or local CLI state. This host has Command
Line Tools rather than full Xcode, so it does not claim Xcode build or XCTest
evidence.

The clean Arch gate is configured to produce seven real captures: Usage and
Usage Advanced, Statistics and Statistics Advanced, receipt detail, Settings,
and Settings Advanced. The macOS gate requires the equivalent seven PNGs. Each
artifact carries `SHA256SUMS`; the final run-specific hashes and inspected PNGs
belong in `visual-receipts/` rather than being predicted in this trace.

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
