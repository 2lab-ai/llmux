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
- [design.md](design.md) — macOS pre-renewal preservation boundary and exact
  Linux/KDE alignment tokens, geometry rules, and visual acceptance criteria.
- [spec.md](spec.md) — shared semantic state/action/effect contract, invariants,
  flows, compatibility, and implementation map.
- [trace.md](trace.md) — seven-section vertical traces, test derivation,
  implementation status, and verification matrix.
- [visual-receipts/](visual-receipts/) — durable production-renderer PNG
  evidence with source-run provenance and SHA-256 checksums. The T6
  default/Advanced set is recorded there only after its authoritative CI run.

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

The presentation boundary is intentionally asymmetric. macOS preserves the
pre-renewal SwiftUI/AppKit island UI at `57df760`; KDE keeps the current
OpenAI-reference inversion and uses the exact alignment tokens in `design.md`.
Both shells still consume the same Rust semantic state, privacy rules, actions,
and receipts. Linux `Advanced` disclosure is presentation-local and does not
imply a matching macOS redesign.
