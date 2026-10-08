# llmux Islands

`llmux-islands` is the native macOS companion for llmux. It gives the same multi-account usage cockpit a glanceable floating-notch surface while keeping llmux as the only source of truth.

The app does not read `~/.config/llmux.json` or provider credentials. It asks the installed llmux CLI for an endpoint-bound local control key through a private pipe, keeps it only in the HTTP executor, and reads usage from the daemon. Remote connections use their explicitly configured control key.

![llmux Islands workspace with four accounts, 5-hour and 7-day usage bars, reset timers, and the Start coding row](../screenshots/llmux-islands-workspace.png)

*Real app UI with demo data, built from the Islands get-started source (PR #196, preview channel). Captured 2026-10-08.*

Older recording (2026-07-02, previous UI): [GIF](../screenshots/llmux-islands-demo.gif), [original MOV](../screenshots/llmux-islands-demo.mov)

## What it shows

- Per-account Claude / Codex / Grok / API-key usage from the llmux daemon.
- 5-hour and 7-day quota windows with reset timing.
- Token/auth health and degraded accounts.
- A **Start coding** row (preview channel) that opens Claude Code or Codex in a chosen project folder through llmux.
- A closed floating island label:

```text
Llmux Islands [mascot] [Claude activity] [Codex activity] [Grok activity]
```

Activity counters are hidden when the count is zero. When one or more sessions
are active, the counter animates with a rainbow loop; the mascot stays still.

**OpenRouter accounts are not modeled yet** (2026-08-21). The shared UI
contract has no openrouter provider case, so an `or:*` account surfaces here
with an `unknown` provider, and Islands cannot start an OpenRouter login — the
shared core rejects that provider. Add the account elsewhere instead
(`llmux login --openrouter`, or the TUI dashboard's `n` provider picker, which
does carry OpenRouter); the daemon, CLI, and TUI serve it normally. The
Swift shell needs tolerant provider decoding plus an icon before Islands can
carry the fourth backend group.

Recent-activity rows use a dark gray background for requests received through the OpenAI Responses endpoint, including when served by a Claude model. Anthropic endpoint requests retain their existing background, regardless of backend. This distinction is shared by macOS and Linux; historical records without endpoint metadata retain the original background.

## Native presentation boundaries

The shells share semantic state and behavior, not a cross-platform widget tree.

- macOS retains the shipped SwiftUI/AppKit presentation from before the UI
  renewal: the colored quota mosaic, rounded account tiles, Statistics surface,
  and native menu hierarchy. Receipt metadata remains available in Statistics.
- KDE uses a black canvas, white opacity tiers, square controls, and equal-width
  data grids. Buttons, fields, selectors, switches, and labels follow one 32px
  control row and the exact 4/8px alignment rhythm documented in the port's
  `design.md`.

On KDE, credential metadata, secondary quota windows, analytics detail, request
receipts, daemon configuration, events, maintenance, diagnostics, and build
metadata live in a labelled, local-only **Advanced** disclosure. Offline,
authentication, warning, failure, and destructive-confirmation states are
never hidden there. macOS intentionally keeps its original information
hierarchy rather than copying this Linux disclosure.

Privacy masking, actions, and receipts come from the shared Rust UI state on
both platforms. Opening Linux Advanced does not dispatch an action, touch the
daemon, or persist state.

## Requirements

- macOS 14 or later and llmux installed on the same computer.
- llmux CLI from the same release as the app (the cask installs it).
- Claude Code or Codex installed to launch that coding app from Islands.
- An existing Claude, ChatGPT, Grok or API-key account to connect.

No Xcode or manual daemon command is needed for the installed app. Islands starts
its configured local daemon when necessary. Local control authentication requires
an llmux build that includes the private Islands handoff; an older CLI shows an
update instruction, rather than treating an unauthorized daemon as stopped.
Node.js/npm are needed on a daemon serving Claude models to Codex through the
Agent SDK; see [Codex frontend prerequisites](codex-frontend/README.md).

## Install

```bash
brew install --cask 2lab-ai/tap/llmux-islands
```

The cask depends on the `llmux` formula, so it installs the matching CLI. The
setup screens and launcher below ship on stable since v0.2.25 (2026-10-08).
Already on 0.2.24? Run `llmux update`; it upgrades both and relaunches the app.

The rolling preview channel has its own cask, `llmux-islands-preview`, which
depends on `llmux-preview`. The two casks conflict; switch with
`llmux channel preview` or `llmux channel stable`, which mirrors the cask.

Then launch `LlmuxIslands.app` from Applications, Spotlight, or Finder.

### Stable 0.2.24

The 0.2.24 app (2026-10-07) predates these screens and cannot authenticate local
control: it adds `x-api-key` only to remote requests, while the daemon requires an
admin credential on `/llmux/*` even from loopback (confirmed from source, not
reproduced in a running app). Upgrade with `llmux update`.
Per-release status: [Release availability](operational-reference.md#release-availability).

## First launch

Open the floating notch at the top of your screen by clicking or hovering over it.
An empty or unavailable workspace opens setup once at launch. The screen separates
connecting, ready with no accounts, connection failure, and connected accounts.

1. Choose **Connect your first account**, then the account provider. The existing
   daemon-owned sign-in flow opens the provider authentication page.
2. Once the account appears, choose **Claude Code** or **Codex**, then **Choose
   project folder**. Folder selection starts no process.
3. Choose **Open in Terminal**. A new Terminal session runs `llmux run` or
   `llmux run --codex` in that folder. **Copy command** is an alternative.

After the first account appears, the tiles show each account's 5-hour and 7-day usage with reset timers — this is the view most people install Islands for.

The app reports Terminal dispatch, not a completed model request. Missing tools
have a copyable installation command. A Terminal permission refusal leaves the
copy-command route available. Project launch currently supports local connections;
remote users launch their existing CLI with their configured remote connection.
No project path is persisted by the launcher.

## Build and run from source

Developers need Xcode 15+, XcodeGen (`brew install xcodegen`), and Rust via rustup.


```bash
cd llmux-islands
xcodegen generate
xcodebuild -project LlmuxIslands.xcodeproj -scheme LlmuxIslands -configuration Debug \
  -derivedDataPath build \
  CODE_SIGN_IDENTITY="-" CODE_SIGNING_REQUIRED=NO CODE_SIGNING_ALLOWED=YES build
open build/Build/Products/Debug/LlmuxIslands.app
```

Click the notch again or click outside it to hide the island. macOS has no menu-bar gauge icon.

## Email anonymous mode

Use **Email anonymous** in the Islands menu when recording or screen-sharing real usage.

When enabled, email addresses in the Usage area are post-processed into a pixelized mosaic so the layout remains faithful but the text is unreadable. Non-email placeholders remain readable.

This is different from demo mode:

- **Email anonymous mode** preserves your real live usage state and pixelizes emails in the UI.
- **Demo mode** replaces displayed identities with stable fake addresses and keeps the notch open. It still uses the live daemon; use fixture snapshots for isolated media.

## Privacy-safe screen captures

For fixture-only production-view PNGs, run the built executable directly:

```bash
LLMUX_ISLANDS_SNAPSHOT_DIR=/tmp/islands-captures \
LLMUX_ISLANDS_SNAPSHOT_KIND=onboarding \
/path/to/LlmuxIslands.app/Contents/MacOS/LlmuxIslands -emailAnonymousEnabled YES
```

This exits before any app window, daemon, credential helper or Terminal action.
`onboarding` renders connecting, empty, account connection, offline, ready and
project-selected screens using synthetic data. `stats` includes mixed incoming
Anthropic/OpenAI Activity rows; `label` renders the closed notch. Label footage as
demo data. Snapshot mode never reads a saved project, remote host or tool inventory
for the launcher. The existing live `--demo` mode below still polls the daemon;
it is not the same privacy boundary. The old recorder scripts may quit a running
Islands app and should not be used to capture an unrelated active session.

## Demo and recording mode

For public screenshots or GIFs, launch the app with demo mode:

```bash
open -na /path/to/LlmuxIslands.app --args --demo
```

or set:

```bash
LLMUX_ISLANDS_DEMO=1 open -na /path/to/LlmuxIslands.app
```

Demo mode:

- Shows stable fake emails instead of real account names.
- Holds the island open for recording.
- Can force activity counters with `LLMUX_ISLANDS_DEMO_INFLIGHT`, for example:

```bash
LLMUX_ISLANDS_DEMO=1 LLMUX_ISLANDS_DEMO_INFLIGHT="claude=3,codex=2" open -na /path/to/LlmuxIslands.app
```

From the repository root, the recording helpers are:

```bash
demo/record-islands.sh
demo/record-all.sh
```

The app capture needs a one-time macOS **Screen Recording** grant for the terminal that runs the recorder.

## Remote daemon

Local control requests are authenticated too. The private CLI handoff returns only the control key for the matching configured proxy port; it refuses a remote CLI configuration. Both the app and the CLI must be 0.2.25 or later, from the same channel, for this handoff. The key never enters view state, saved app preferences, logs, or clipboard. HTTP 401/403 means the daemon is already running and does not trigger a spawn/restart.

A remote connection needs two things you provide:

- **An HTTPS endpoint.** This is the app's own rule: any non-loopback endpoint must use HTTPS, HTTP is allowed only for loopback, and redirects are denied. llmux does not terminate TLS; the CLI's remote mode is plain HTTP over a trusted overlay. Put an HTTPS reverse proxy in front of the remote daemon.
- **An admin-scope key.** Control reads such as dashboard and status require admin scope; keys issued by default are data-plane only and fail here.

Configure the HTTPS host/port and that control `x-api-key` in **Connection settings**. A stored remote key is bound to that endpoint: changing host/port requires an explicit replacement, and switching to local discards it.

Project launch is local only. Remote users launch their existing CLI with their configured remote connection.

Do not expose mutating llmux endpoints to an untrusted network without the API key.

## Troubleshooting

### The island cannot connect

- Still on 0.2.24? Local control needs the 0.2.25 app and CLI together; run `llmux update`.
- Use **Retry** or **Connection settings**.
- If the private local helper is missing, use **Copy update command** to update
  both llmux and Islands on the app’s release channel (preview date version or
  stable version). Source builds require rebuilding the matching CLI too.
- Keep the app port equal to `proxy.port` in the CLI configuration; a mismatch
  fails closed.
- Do not restart merely because the daemon returned 401/403.

For manual diagnosis:

```bash
llmux restart
llmux status
```

### The app build cannot find an Xcode project

Regenerate it from `project.yml`:

```bash
cd llmux-islands
xcodegen generate
```

### Screen recording is black or incomplete

Grant Screen Recording permission to the terminal app that runs `demo/record-islands.sh`, then restart that terminal and record again.
