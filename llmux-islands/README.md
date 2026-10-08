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

## Get started

The setup screens and Start coding launcher described here are on the
**preview channel** as of 2026-10-08 (shipped 2026-10-07). Install
`brew install --cask 2lab-ai/tap/llmux-islands-preview`, which also installs the
matching preview CLI; existing stable users run `llmux channel preview`. The
stable 0.2.24 app predates these screens and cannot authenticate local control
against the daemon — see [release availability](../docs/operational-reference.md#release-availability).

Installed users do not need Xcode: open Islands, connect an account, then choose
Claude Code/Codex and a project folder. **Open in Terminal** launches the existing
llmux frontend only after a click. See the [user guide](../docs/llmux-islands.md#first-launch)
for prerequisites, old-CLI recovery and remote limitations.

The new macOS setup and launcher are native executor UI; shared dashboard state,
provider selection, scheduler and Linux semantics are unchanged. See the active
[get-started contract](../.prd/22-islands-get-started.md).

### Notes for operators

- Activity receipts distinguish incoming OpenAI requests with dark gray
  backgrounds, including Claude models served through the SDK; see
  [endpoint activity](../docs/codex-frontend/activity.md).
- Node.js/npm are required only on a daemon serving Claude-through-Codex, not by
  the native Islands UI itself.

## Build & run

**Build from source (developers).** Ordinary users do not need Xcode; install
the preview cask described in [Get started](#get-started) instead.

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

Remote daemons require HTTPS and an `x-api-key`; redirects are denied. llmux itself does not terminate TLS; put an HTTPS reverse proxy in front of a remote daemon. Loopback may use HTTP and authenticates through the private `llmux islands-connection --port N` handoff. The CLI reads only its owned config; the native executor receives only a matching local endpoint/control key through bounded Process pipes. It never receives provider credentials, persists the key, or sends a configured remote key to loopback. HTTP 401/403 prevents daemon spawning. Remote-key reuse is restricted to the same endpoint.

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
