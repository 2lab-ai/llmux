# Cross-platform Islands semantic UI specification

Status: implementation contract for the Arch/KDE port.

## Boundary

The shared core accepts typed data/mutation Actions and asynchronous Results,
reduces them into UiState, and emits Effects. Platform shells render UiState,
execute those Effects, and own only platform commands such as window toggling,
clipboard, notification/sound, and quit. Neither QML nor Swift may reimplement
protocol parsing, health classification, privacy masking, receipt shaping,
dashboard single-flight rules, or login transitions.

The canonical machine-readable state contract is ui-contract.schema.json.
The Rust `Action` and `Effect` enums are its executable transition contract.
JSON is used at the shell boundary so the same fixtures are consumed by
QML/CXX-Qt and Swift through a narrow, length-delimited C ABI.

## Top-level state

    UiState {
      schema_version: 1,
      revision: u64,
      lifecycle: "starting" | "ready" | "offline" | "fatal",
      window: WindowState,
      navigation: "usage" | "statistics" | "menu",
      connection: ConnectionState,
      usage: UsageState,
      statistics: StatisticsState,
      settings: SettingsState,
      operation: OperationState?,
      notices: Notice[],
      verification_receipts: VerificationReceipt[]
    }

WindowState contains open, open_reason, selected_screen_id, presentation
(layer_shell, positioned_x11, regular), width, content_height, and compact
provider counters.

ConnectionState contains endpoint_display, remote, authenticated, daemon
version, last_success_ms, retry_at_ms, and a sanitized error. It never contains
the API key.

UsageState contains ordered AccountTile values, current_by_group, aggregate
in-flight counters, the add-dialog/login state, and empty/offline labels.

StatisticsState contains overview, model/client/health rows, 24h/72h heatmap
data, data-quality labels, and ActivityReceipt values.

SettingsState contains display preferences, connection fields with
api_key_configured rather than the key, screen/sound choices, event drafts,
autostart state, build/channel/update state, and platform capabilities.
`sound_id` is nullable until the platform adapter loads its persisted
selection; screen selection is carried by `window.selected_screen_id`.

## Semantic records

### AccountTile

All absolute semantic UI timestamps, including quota `resets_at`, use Unix
epoch milliseconds even when the daemon source document uses epoch seconds.

    id                 action handle; opaque while privacy is enabled
    display_name       privacy-safe display label
    provider           claude | codex | grok | api | unknown
    current
    paused
    healthy
    status
    blocked_reason?
    in_flight
    token_expiry       state + timestamp/countdown text
    gauges[]           five_hour | seven_day | fable_weekly
    warning_level      normal | warning | critical
    busy_action?

Gauge values contain used_fraction clamped to 0..1, remaining_fraction, reset
timestamp/countdown, availability, and constraining. The renderer chooses only
layout and color tokens.

### ActivityReceipt

    receipt_id
    kind               in_flight | request | note
    occurred_at_ms
    status?
    method?
    path?
    account_display?
    provider?
    model?
    effort?
    fast
    tokens?
    cache?
    cost_usd?
    duration_ms?
    elapsed_ms?
    message?
    error

No raw request/response body, provider token, API key, authorization header, or
unmasked account email is permitted while anonymity is enabled. The core owns
the handle-to-real-id map and rejects unknown or stale handles before HTTP.

### VerificationReceipt

    id
    operation          login | add_account | remove_account | pause_account |
                       settings | event | maintenance | autostart
    target_display?
    started_at_ms
    finished_at_ms
    outcome            succeeded | failed | cancelled | no_change
    message

Verification receipts are bounded local UI confirmations and are not persisted
by the Linux shell. They are sanitized through the same secret redactor as
errors.

## Actions

The executable upper vocabulary is the Rust `Action` enum. Its exact variants
are:

    AppStarted
    TrayActivated
    OpenRequested { reason }
    CloseRequested
    NavigationSelected { navigation }
    WindowMetricsChanged { width, content_height }
    RefreshRequested { source }
    DashboardReceived { request_id, document, received_at_ms }
    DashboardFailed { request_id, error, failed_at_ms }
    LoginStarted { operation_id, provider, started_at_ms }
    LoginStatusReceived { operation_id, status, at_ms }
    LoginCancelRequested { operation_id }
    SettingsChanged { id, email_anonymous, started_at_ms }
    EventUpsertRequested { id, event, started_at_ms }
    EventRemoveRequested { id, event_id, started_at_ms }
    AutostartChanged { id, enabled, started_at_ms }
    MaintenanceRequested { id, command, started_at_ms }
    OperationStarted { id, request, target_display?, started_at_ms }
    OperationFinished { id, outcome, message, finished_at_ms }

`DashboardReceived` is constructed only after a shell has matched the response
to the core-issued request id. The macOS bridge exposes this as the dedicated
`apply_dashboard` ABI call so an arbitrary JSON action cannot bypass request
correlation.

`OperationStarted.request` is one of:

    AddAccount { name?, api_key }
    PauseAccount { account_id, paused }
    RemoveAccount { account_id, confirmed }
    UpdateSettings { email_anonymous }
    UpsertEvent { event }
    RemoveEvent { event_id }
    PersistLocalSettings { change }
    SetAutostart { enabled }
    RunMaintenance { command }

The Linux controller may use the purpose-specific action variants or the
generic operation wrapper; both reduce through the same validation and receipt
path. The macOS JSON ABI uses `operation_started` for mutations and represents
an add-account secret only as `has_api_key`; Swift retains the actual API key
until it executes the correlated effect. `llmux-islands-macos-bridge/ABI.md`
is the exhaustive serialized action table.

Shell gestures that have no shared state transition remain native aliases:
Escape maps to `CloseRequested`; screen, sound, Fable, and connection changes
map to `PersistLocalSettings`; previewing sound, copying text, opening an
approved URL, showing a notification, and quitting remain platform commands.
Every asynchronous mutation closes with `OperationFinished`. Stale dashboard
and operation ids cannot update canonical state.

## Effects

The Rust core emits only these executor instructions:

    EnsureLocalDaemon
    FetchDashboard { request_id }
    ScheduleDashboardRetry { retry_at_ms }
    CancelDashboardRetry
    StartLogin { operation_id, provider }
    PollLogin { operation_id, state }
    CancelLogin { operation_id, state }
    StopLoginPoll { operation_id }
    RunOperation { operation_id, request }
    UpdateSettings { operation_id, email_anonymous }
    UpsertEvent { operation_id, event }
    RemoveEvent { operation_id, event_id }
    PersistSettings { operation_id, change }
    SetAutostart { operation_id, enabled }
    RunMaintenance { operation_id, command }
    UpdateTray { provider_in_flight }

Daemon effects may carry core-resolved raw account ids or OAuth state only as
short-lived executor inputs. API keys remain in the platform executor and are
correlated by operation id; the macOS semantic action carries only whether a
key is present. Debug output uses redacted implementations. Open URL, copy,
notification, sound preview, native window configuration, and quit are
shell-owned commands rather than falsely portable core effects.

## Presentation hierarchy and progressive disclosure

Both native shells render the same two-level information hierarchy without
adding state to the shared semantic contract:

1. The default path shows current connection/attention state, account identity,
   primary quota or usage, compact navigation, and the action needed most often
   on that surface.
2. A clearly labelled, keyboard-accessible `Advanced` disclosure reveals
   diagnostic and infrequent controls in the context they affect. This includes
   credential/token metadata, in-flight internals, model/client/health detail,
   heatmaps, request and verification receipts, daemon endpoint credentials,
   platform capability diagnostics, events, release/channel maintenance, and
   build/source metadata.
3. Warning, critical, offline, fatal, authentication-required, and destructive
   confirmation states remain visible at the default level. Progressive
   disclosure must never conceal the reason a primary action is unavailable.
4. Screen, sound, privacy, launch-at-login, add account, refresh, and navigation
   remain directly reachable. Contextual account mutation actions may live in
   the account's Advanced disclosure, with removal still requiring confirmation.
5. Disclosure state is presentation-local, ephemeral, and excluded from
   `UiState`, persistence, actions, effects, receipts, and daemon traffic.
6. Renderer evidence uses one canonical privacy-safe `UiState` fixture and
   fixed clock on both platforms. Each artifact set contains exactly seven
   distinct PNGs—Usage/default+Advanced, Statistics/default+Advanced, receipt
   detail, and Settings/default+Advanced—and includes the real shell brand,
   navigation, selected route, and connection state.

The named `openai` UI/UX reference is the visual north star. Because Islands is
an inverted menu-bar/notch surface, it uses the reference's full-inversion mode:
pure or near-black canvas, white ink, and 100%/60%/44% opacity tiers. Emphasis
comes from white-fill/black-text inversion rather than a chromatic accent.
Internal cards, controls, and disclosures are flat and square (`0px` radius)
where platform primitives permit; they have no shadow, gradient, glow, or
decorative material blur. The outer island/window silhouette may retain its
platform-required rounding.

Provider identity is text or iconography rather than decorative color. Color is
reserved for rare semantic warning, error, success, and indispensable
quantitative distinction, always paired with text or iconography; normal quota
bars, focus, and navigation are monochrome. Platform system grotesque/sans
typography stands in for OpenAI Sans, with monospaced text limited to identifiers,
timestamps, endpoints, and tabular numeric telemetry. Layout follows a 4/8-point
spacing rhythm with generous whitespace. Frequent navigation and keyboard
actions do not animate; any occasional disclosure feedback is short,
interruptible, and reduced-motion safe.

## Required flows

### Startup and polling

1. AppStarted loads settings, registers tray, and emits EnsureLocalDaemon only
   for a loopback connection.
2. FetchDashboard begins immediately.
3. Success replaces the authoritative document and derives all views.
4. A poll occurs every 10 seconds. Only one dashboard request is active.
5. Failure retains the last good snapshot, marks it stale/offline, and applies
   bounded backoff. Manual refresh bypasses the wait but not the single-flight
   rule.

### Login and device verification

1. LoginStarted validates provider and emits POST /llmux/login/start.
2. The returned state id is the only poll key.
3. Claude/Codex show browser-waiting progress.
4. Grok additionally shows verification_uri and user_code with open/copy.
5. Pending polls are tolerant of transient errors until the five-minute
   deadline; terminal error, cancel, or success stops polling.
6. Terminal outcome adds a VerificationReceipt and refreshes dashboard on
   success. Codes/URLs are cleared from state after termination.

### Account mutation

1. One mutation per account is allowed at a time.
2. Destructive removal requires a platform confirmation.
3. Mutation success is not treated as final view state: it emits a dashboard
   refresh and then a verification receipt.
4. Failure restores controls and adds a sanitized failed receipt.

### Request receipts

1. Map dashboard activity without reordering.
2. In-flight rows update elapsed time locally without mutating source data.
3. Completed request/note rows are immutable.
4. Privacy is applied before they reach UiState.
5. Older daemon documents missing additive fields show unavailable, never a
   misleading zero.

### Maintenance

1. Detect install owner: pacman, self-managed release, Homebrew, or unknown.
2. On Arch/pacman, show the package command/instructions and never overwrite
   pacman-owned files.
3. Self-managed installations stay fail-closed until an official signed
   release manifest is available. The checksum/atomic-replace primitive is not
   reachable from the production controller by itself.
4. Channel changes require confirmation and apply to both daemon and Islands
   package policy.
5. Every attempt emits a verification receipt with old/new version/channel
   when available.

## Invariants

1. DashboardDoc is the single source of account, analytics, event, and request
   receipt truth.
2. Account ids used for actions are never reconstructed from masked labels;
   privacy-on state exposes only core-issued opaque handles.
3. Secrets are absent from UiState serialization, logs, snapshots, notices, and
   receipts.
4. A result may update state only when its request/operation id is current.
5. Login has at most one pending job and always terminates or times out.
6. Network/daemon failures never erase the last good dashboard.
7. Tray and notification availability come from Qt's live session capability
   report; before detection they are unavailable/unknown rather than assumed.
8. Connection credentials use explicit keep, replace, or clear intent. A
   cleared remote credential may be persisted, but the connection remains
   visibly unauthenticated and no request client is created until replacement.
9. Wayland placement capability is reported honestly; unsupported placement
   falls back to a normal window.
10. All user-visible mutations produce a terminal verification receipt.
11. Additive daemon fields default to unavailable/empty without parse failure.
12. QML is a renderer: business branching belongs in the reducer.

## Compatibility

- schema_version is required. Additive optional fields do not increment it.
- Renames/removals or meaning changes require a new version and a compatibility
  decoder.
- Contract fixtures and reducer/client tests cover current, minimum-supported,
  additive defaults, offline/error retention, malformed input rejection,
  privacy-on serialization, and deterministic demo projection.
- Swift rejects unknown schema versions and decodes the same initial and
  fixture-backed semantic states as the Linux adapter. Bridge tests cover
  every action/effect shape, stale ids, buffer ownership, and secret absence.

## Implementation file map

    llmux-islands-core/
      Cargo.toml
      src/{lib,contract,client,reducer,derive,privacy,receipts}.rs
      tests/{contract,reducer,fixtures,http_client}.rs
      contract/ui-contract.schema.json
      fixtures/*.json

    llmux-islands-linux/
      Cargo.toml
      build.rs
      src/{main,controller,lib,desktop,maintenance,platform,settings,qt_runtime}.*
      qml/{Main,Usage,Statistics,Menu}.qml
      resources/{icons,io.twolab.LlmuxIslands.desktop,...}
      packaging/arch/PKGBUILD

    llmux-islands-macos-bridge/
      Cargo.toml
      include/llmux_islands_macos_bridge.h
      src/lib.rs

    llmux-islands/
      scripts/build-rust-core.sh
      LlmuxIslands/SharedCore/*.swift
      LlmuxIslandsTests/{SharedUiStateTests,SharedCoreEffectTests,LlmuxClientSecurityTests}.swift

    .github/workflows/
      linux-islands.yml

The root llmux package remains the daemon and wire DTO authority. The core may
depend on the root library by path; the root must not depend on the GUI crates.
