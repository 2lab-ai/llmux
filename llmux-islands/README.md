# llmux-islands

A native macOS menu-bar / notch app that shows per-account **llmux** usage at a
glance and lets you add / remove subscriptions — driven entirely by the llmux
daemon's HTTP API. The app never reads `~/.config/llmux.json` or touches provider
credentials; llmux is the single source of truth.

Design: [`.prd/11-llmux-islands-spec.md`](../.prd/11-llmux-islands-spec.md) (what),
[`.prd/12-llmux-islands-architecture.md`](../.prd/12-llmux-islands-architecture.md) (how).

## Build & run

Requires Xcode 15+ and XcodeGen (`brew install xcodegen`).

```sh
cd llmux-islands
xcodegen generate          # project.yml -> LlmuxIslands.xcodeproj (gitignored)
xcodebuild -project LlmuxIslands.xcodeproj -scheme LlmuxIslands -configuration Debug \
  -derivedDataPath build \
  CODE_SIGN_IDENTITY="-" CODE_SIGNING_REQUIRED=NO CODE_SIGNING_ALLOWED=YES build
open build/Build/Products/Debug/LlmuxIslands.app
```

Needs a running llmux daemon on `http://127.0.0.1:3456` (`llmux run`). Click the
gauge icon in the menu bar to open the island; click again to hide.

## llmux API it consumes

| Action | Endpoint |
|---|---|
| Display accounts + 5h/7d usage | `GET /llmux/status` |
| Add an Anthropic API-key account | `POST /llmux/add-account` |
| Remove an account | `POST /llmux/remove-account` |
| Add a Claude / Codex subscription (OAuth) | `POST /llmux/login/start` → `GET /llmux/login/status` (+ `POST /llmux/login/cancel`) |

For a remote daemon, set a host/port and the `x-api-key` (loopback is exempt).

## Layout

```
llmux-islands/
  project.yml                       # XcodeGen spec
  LlmuxIslands/
    App/        LlmuxIslandsApp, AppDelegate, NotchPanel
    UI/         RootView, AccountTile, AddAccountView, ProviderIcon
    Models/     LlmuxModels          # Codable mirror of /llmux/status
    Services/   LlmuxClient          # URLSession client over the llmux API
    ViewModels/ AccountsViewModel    # polling + add/remove/login actions
    Support/    notch + usage visual primitives lifted from agent-island
    Resources/  Info.plist, entitlements, Assets.xcassets
```

## Notes

- v1 opens via the menu-bar item (agent-island's notch-hover gesture machinery
  is coupled to its session monitor and was not lifted).
- OAuth logins run on the **daemon** — llmux opens the browser and injects the
  account; the app only polls progress and never sees the token.
