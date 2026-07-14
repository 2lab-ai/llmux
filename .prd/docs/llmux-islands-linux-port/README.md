# llmux-islands Linux port dossier

This directory is the implementation contract and evidence map for the native
Arch Linux/KDE Plasma port of the shipped macOS `llmux-islands` companion.

## Architecture decision

The reusable boundary is semantic, not a cross-platform widget tree:

- `llmux-islands-core`: Rust daemon client, typed actions/effects, reducer,
  privacy-safe UI projection, polling/login state, and activity/verification
  receipts.
- macOS: the existing SwiftUI/AppKit shell stays native, but now consumes the
  same Rust state machine through a small, audited C ABI. Swift projects the
  canonical state into the shipped views and executes transient effects.
- Linux/KDE: Qt 6/QML/Kirigami through CXX-Qt, with LayerShellQt, Qt
  `SystemTrayIcon` backed by Plasma's StatusNotifierItem integration, `QScreen`,
  XDG autostart, native tray notifications, and freedesktop sound adapters.

The checked-in JSON Schema defines the shared `UiState` boundary. Rust
`Action`/`Effect` enums are the executable protocol; platform-only window,
clipboard, notification, sound, and quit commands remain in the thin shell.

## Documents

- [inventory.md](inventory.md) — shipped macOS surface and daemon protocol
  inventory, including recent request activity (the receipt surface).
- [platform-mapping.md](platform-mapping.md) — exhaustive macOS-to-KDE mapping,
  official research, alternatives, and architecture decision.
- [spec.md](spec.md) — shared semantic state/action/effect contract, invariants,
  flows, compatibility, and implementation map.
- [trace.md](trace.md) — seven-section vertical traces, test derivation,
  implementation status, and verification matrix.
- [visual-receipts/](visual-receipts/) — eight full-surface and receipt-detail
  PNGs captured from the macOS and KDE production renderers, with source-run
  provenance and SHA-256 checksums.

## Scope status

| Area | Status in this worktree |
|---|---|
| Current implementation audit | Complete |
| Port architecture and platform mapping | Complete |
| Shared Rust semantic core and UI schema | Implemented and tested |
| KDE Usage, Statistics, Menu, tray, and window shell | Implemented, Arch smoke-verified, and visually inspected |
| Settings, events, autostart, notifications, and maintenance receipts | Implemented |
| Arch packaging and CI recipe | Clean-container build, package, install, binary smoke, and screenshot verified |
| macOS adoption of the core | Implemented through the versioned JSON/C ABI boundary, Xcode-tested, and visually inspected |
| Stable release/tag | Explicitly out of scope without a user release gate |

The older `.prd/11-llmux-islands-spec.md` and
`.prd/12-llmux-islands-architecture.md` describe an earlier Claude/Codex-only
plan. They remain historical input, not an inventory of the shipped app.

The KDE production renderer owns a shared `IslandTheme` and custom QML control
set derived from the macOS SwiftUI sources: black panel, translucent dark cards,
provider/quota accents, amber segmented navigation, and monospaced activity and
verification receipts. Platform-native integration remains Qt/Kirigami; bright
system widget chrome is not part of the application surface.
