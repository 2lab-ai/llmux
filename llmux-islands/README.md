# llmux-islands

A native macOS notch app that shows per-account **llmux** usage at a
glance and lets you manage subscriptions — driven entirely by the llmux daemon's
HTTP API. Raw dashboard JSON is reduced by the same Rust semantic core as the
Linux shell; SwiftUI renders its versioned, privacy-safe `UiState` and executes
transient effects. With email anonymity enabled, account ids in `UiState` are
opaque handles and raw ids stay out of published/rendered state and persistence.
Raw ids may still exist in the in-memory daemon input/cache and transient
executor effects; OAuth correlation state and account-add keys likewise stay
executor-only. The configured remote control key remains owned by native
connection settings.

Cross-platform design, UI inventory, KDE mapping, shared schema, and evidence:
[`../.prd/docs/llmux-islands-linux-port/`](../.prd/docs/llmux-islands-linux-port/).
The older [spec](../.prd/11-llmux-islands-spec.md) and
[architecture](../.prd/12-llmux-islands-architecture.md) remain historical
macOS inputs.
User guide: [`../docs/llmux-islands.md`](../docs/llmux-islands.md).

## Build & run

Requires Xcode 15+, XcodeGen (`brew install xcodegen`), and the stable Rust
toolchain installed through rustup. The Xcode build phase compiles and links the
Rust bridge for each requested macOS architecture.

```sh
cd llmux-islands
xcodegen generate          # project.yml -> LlmuxIslands.xcodeproj (gitignored)
xcodebuild -project LlmuxIslands.xcodeproj -scheme LlmuxIslands -configuration Debug \
  -derivedDataPath build \
  CODE_SIGN_IDENTITY="-" CODE_SIGNING_REQUIRED=NO CODE_SIGNING_ALLOWED=YES build
open build/Build/Products/Debug/LlmuxIslands.app
```

For a loopback HTTP configuration, the shared startup effect starts an
installed `llmux` daemon on the configured port when needed. Click or hover
over the notch to open the island; click the notch again (or click outside) to
hide it.

## llmux API it consumes

| Action | Endpoint |
|---|---|
| Display accounts, analytics, activity receipts | `GET /llmux/dashboard` (`/llmux/status` is used only when dashboard explicitly returns 404/405/501; its document is normalized and request-correlated through Rust) |
| Add an Anthropic API-key account | `POST /llmux/add-account` |
| Remove an account | `POST /llmux/remove-account` |
| Add a Claude / Codex subscription (OAuth) | `POST /llmux/login/start` → `GET /llmux/login/status` (+ `POST /llmux/login/cancel`) |
| Pause/resume an account | `POST /llmux/pause-account` |
| Email anonymity and operator events | `POST /llmux/settings`, `POST /llmux/events` |

Remote daemons require HTTPS and an `x-api-key`; redirects are denied. Loopback
may use HTTP, is exempt from the control key, and never receives a configured
remote key header.

## Layout

```
llmux-islands/
  project.yml                       # XcodeGen spec
  LlmuxIslands/
    App/        LlmuxIslandsApp, AppDelegate, NotchPanel
    UI/         native SwiftUI island, menus, analytics and receipt views
    Llmux/      HTTP executor DTOs and IslandUsageModel
    SharedCore/ Rust C-ABI owner, canonical UiState mirror and projections
    Dashboard/  native tile/analytics presentation types
    Core/       notch settings and selectors
    Resources/  Info.plist, entitlements, Assets.xcassets
  scripts/build-rust-core.sh         # Xcode Rust/staticlib build helper
```

## Notes

- macOS has no `NSStatusItem` tray surface. It reports native app start,
  open/close, navigation, and window metrics to the shared reducer; the
  reducer's tray-count effect needs no separate executor because the
  closed-notch label observes the same canonical provider counts.
- Screen and notification-sound discovery remain native AppKit capabilities;
  selections still enter Rust as typed operations before their transient
  persistence effects run on macOS.
- OAuth logins run on the **daemon** — llmux opens the browser and injects the
  account; the app only polls progress and never sees the token.
