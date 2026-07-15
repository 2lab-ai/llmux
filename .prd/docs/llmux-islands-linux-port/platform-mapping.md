# macOS to KDE mapping and architecture decision

## Decision

Use a platform-neutral Rust core and a KDE-native Qt 6/QML/Kirigami shell:

    llmux HTTP API
        -> islands-core (protocol, reducer, formatting, polling/login state)
        -> stable JSON semantic UI contract
        -> CXX-Qt QObject adapter -> QML/Kirigami -> KDE adapters
        -> narrow C ABI -> Swift projection -> SwiftUI/AppKit adapters

Keep the shipped SwiftUI/AppKit shell and macOS interaction model stable while
making the Rust reducer its canonical semantic source. The shared contract is
state/actions/effects, not a generic widget tree.

This maximizes reuse of behavior and data rules while preserving platform
windowing conventions. It also makes the Rust core independently testable on
macOS and Linux.

## Why this stack

- Kirigami is KDE's Qt Quick component set, and KDE recommends keeping business
  logic outside QML:
  https://develop.kde.org/docs/getting-started/kirigami/
- CXX-Qt provides safe Rust/Qt interop, QML elements, and queued calls back to
  the Qt event loop:
  https://kdab.github.io/cxx-qt/book/
- LayerShellQt exposes the QWindow anchoring, layer, margin, desired-size, and
  keyboard-interactivity controls required for a top-edge surface:
  https://api.kde.org/legacy/plasma/layer-shell-qt/html/window_8h_source.html
- KStatusNotifierItem is KDE's native tray/status item abstraction. The port
  reaches Plasma's StatusNotifierItem backend through Qt's portable
  `SystemTrayIcon` facade so the QML shell retains a regular-desktop fallback:
  https://api.kde.org/kstatusnotifieritem.html

### Alternatives

| Alternative | Strength | Rejection reason |
|---|---|---|
| Slint + Qt backend + ksni | Highest renderer sharing and pure-Rust app code | Public positioning is unavailable on Wayland and the public backend does not expose the QWindow needed by LayerShellQt; a custom backend would erase the simplicity advantage |
| Tauri | Web UI reuse and packaging | Linux tray click events are not supported, which breaks the primary tray-to-island interaction: https://v2.tauri.app/learn/system-tray/ |
| egui/iced | Permissive Rust-native stack | Custom widgets do not map as directly to KDE controls and still require separate tray/layer-shell integration |
| C++-only Qt | Direct KDE APIs | Duplicates protocol/reducer logic and fails the Rust-sharing goal |
| UniFFI for the macOS bridge | Generated typed Swift bindings and a mature Rust FFI workflow | The boundary is already a checked, versioned JSON document plus transient effects; a seven-function length-delimited C ABI keeps the signed app's generated/runtime surface smaller while preserving the same contract tests |

UniFFI remains the documented fallback if this boundary grows into a broad
typed object API. Its Swift bindings support module maps and XCFramework
packaging, but introduce generated Swift/C headers and bindgen/build plumbing:
https://mozilla.github.io/uniffi-rs/latest/swift/overview.html and
https://mozilla.github.io/uniffi-rs/next/swift/xcode.html

Slint remains a viable fallback for a conventional window. Its Qt backend
supports Linux X11/Wayland and macOS, but its desktop license/attribution and
the layer-shell access gap must be accepted explicitly:
https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backend_qt/

## Window-system constraint

Qt documents that some systems, notably Wayland, do not support application
chosen top-level positions. The port therefore does not emulate AppKit
setFramePosition on Wayland:
https://doc.qt.io/qtforpython-6.5/PySide6/QtGui/QWindow.html

Instead:

- Plasma Wayland: LayerShellQt, top anchor, overlay layer, zero exclusion zone,
  selected QScreen, centered content within the layer surface.
- X11: frameless always-on-top tool window positioned from QScreen geometry.
- Other compositors: regular Kirigami window and Qt `SystemTrayIcon`; no false
  claim of exact top-edge placement.

## UI mapping

| macOS/AppKit or SwiftUI | KDE/Qt mapping | Parity rule |
|---|---|---|
| NSPanel nonactivating borderless window | QQuickWindow + LayerShellQt overlay | No taskbar entry; focus only for interactive content |
| Physical-notch geometry | Top-anchored centered layer surface | A display without a notch uses a small top margin |
| NSEvent global hover/click monitor | Qt `SystemTrayIcon` activation + local pointer handlers | No global interception or accessibility permission |
| Outside-click close and click repost | focus/active loss and Escape | Never synthesize/repost another application's click |
| Closed provider counters | tray tooltip/menu/warning icon plus optional compact layer surface | Same provider grouping and total; the portable tray API has no attention-status setter |
| Native Usage/Statistics/Menu routes | Checkable Qt Quick segmented actions | Same semantic routes and state retention; Linux route `menu` is labelled Settings without changing macOS navigation |
| SwiftUI colored quota-tile grid | Equal-width QML card delegates in GridLayout | Same account/current/attention state; Linux defaults to primary quota and reveals secondary detail in Advanced |
| ContextMenu | Qt Quick Controls Menu | Pause/resume/remove actions and disable rules match |
| alert/confirmationDialog/sheet | Kirigami.PromptDialog/Dialog | Destructive and channel changes require confirmation |
| ProgressView | Kirigami.LoadingPlaceholder/BusyIndicator | Busy is operation-scoped, not global |
| Linux-only contextual `Advanced` disclosure | Checkable labelled control plus conditional Qt Quick sections | Presentation-local only; macOS keeps its original hierarchy |
| SwiftUI settings rows | Explicit two-column QML forms | Labels, validation, keyboard navigation, 104px label column, and 16px control gap |
| ScreenSelector | QGuiApplication::screens/QScreen | Stable screen id and fallback |
| NSSound | freedesktop notification sound-name | Preview is cancellable and “none” is valid |
| SMAppService launch at login | XDG autostart or systemd --user | Toggle reflects actual installed state |
| NSWorkspace open URL | QDesktopServices::openUrl | Allow http/https only |
| Accessibility permission | “Not required on Plasma” info row | Port does not ask for unnecessary privilege |
| About/release/source links | Kirigami card/FormLayout with external-link actions | Version/build/channel remain visible |
| App terminate | QCoreApplication::quit | Stop tasks and unregister integrations |

## Semantic content mapping

| Current content | KDE view |
|---|---|
| Usage header/status/add/refresh | Kirigami toolbar actions and InlineMessage |
| Default account usage | Compact Kirigami card delegates with current/attention/active state and primary quota |
| Advanced account details | Secondary quota, token/credential metadata, raw status, and account mutation controls |
| OAuth/API key chooser | Kirigami dialog with provider cards and password field |
| Device verification code | Selectable Label, copy and open actions |
| Statistics default | Summary metrics plus compact account overview and actionable health |
| Statistics Advanced | Model/client/health detail and 24h/72h heat map in contextual sections |
| Recent activity/request receipts | Advanced section with repeated card/row delegates for in-flight, completed, note, and verification receipts; failures remain visible at their originating surface |
| Settings default | Screen, sound, privacy, launch-at-login, and actionable connection state |
| Settings Advanced | Connection editor, platform diagnostics, events, maintenance, receipts, and build/source metadata |
| Events editor | Modal Qt Quick Controls Dialog with inline date-time validation |

## Code-sharing boundary

Shared by both native shells:

- wire DTO decoding through llmux's DashboardDoc where possible;
- tolerant login/event/settings DTOs;
- reducer, derived semantic state, privacy, gauges, timestamps, receipt shaping;
- polling/backoff, login state machine, action validation, effect generation;
- deterministic fixtures and JSON contract snapshots;
- operation/login verification receipts and executor-effect correlation.

Linux-only:

- CXX-Qt QObject bridge, QML/Kirigami widgets;
- Qt `SystemTrayIcon` with Plasma StatusNotifierItem integration, LayerShellQt,
  notification, XDG autostart, package manager.

macOS-only:

- AppKit panel/status behavior and SwiftUI views;
- the narrow C ABI owner/lifetime wrapper and one-way native DTO projection;
- operation-scoped account-add key ownership in the Swift executor, plus the
  existing shell-owned remote control credential. Only presence markers reach
  semantic actions; neither secret enters UiState or bridge output.

## Arch dependencies

The PKGBUILD is the package dependency authority. Its direct build dependencies
are `cargo`, `clang`, `git`, and `qt6-tools`, with Arch's `base-devel` group
assumed for `makepkg`; CXX-Qt and the other Rust crates are locked by
`Cargo.lock`. Runtime dependencies are `gtk3`, `kirigami`, `layer-shell-qt`,
`libcanberra`, `libnotify`, `qqc2-desktop-style`, `qt6-base`,
`qt6-declarative`, `qt6-svg`, and `qt6-wayland`. LayerShellQt is still treated
as a session capability at runtime, with X11 placement and regular-window
fallbacks.
