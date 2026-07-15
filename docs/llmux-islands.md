# llmux Islands

`llmux-islands` is the native macOS companion for llmux. It gives the same multi-account usage cockpit a glanceable menu-bar/notch surface while keeping llmux as the only source of truth.

The app does not read `~/.config/llmux.json`, does not touch provider credentials, and does not run separate usage scripts. It talks to the running llmux daemon over HTTP.

## What it shows

- Per-account Claude / Codex / API-key usage from the llmux daemon.
- 5-hour and 7-day quota windows with reset timing.
- Token/auth health and degraded accounts.
- A closed floating island label:

```text
Llmux Islands [mascot] [Claude activity] [Codex activity]
```

Activity counters are hidden when the count is zero. When one or more sessions
are active, the indicator animates with a rainbow loop; the mascot makes a
small jump whose speed scales with activity up to the capped high-activity
state.

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

- macOS with Xcode 15+.
- XcodeGen: `brew install xcodegen`.
- A running llmux daemon on `http://127.0.0.1:3456`.

Start the daemon with either:

```bash
llmux run
```

or, if you only want the daemon/TUI:

```bash
llmux server
```

## Install

```bash
brew install 2lab-ai/tap/llmux-islands
```

Then launch `LlmuxIslands.app` from Applications, Spotlight, or Finder.

## Build and run from source

```bash
cd llmux-islands
xcodegen generate
xcodebuild -project LlmuxIslands.xcodeproj -scheme LlmuxIslands -configuration Debug \
  -derivedDataPath build \
  CODE_SIGN_IDENTITY="-" CODE_SIGNING_REQUIRED=NO CODE_SIGNING_ALLOWED=YES build
open build/Build/Products/Debug/LlmuxIslands.app
```

Click the menu-bar gauge icon to open or hide the island.

## Email anonymous mode

Use **Email anonymous** in the Islands menu when recording or screen-sharing real usage.

When enabled, email addresses in the Usage area are post-processed into a pixelized mosaic so the layout remains faithful but the text is unreadable. Non-email placeholders remain readable.

This is different from demo mode:

- **Email anonymous mode** preserves your real live usage state and pixelizes emails in the UI.
- **Demo mode** replaces identities with stable fake addresses and suppresses config writes for public demos.

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

Loopback access is unauthenticated by default. For a remote daemon, configure the app with the daemon host/port and the llmux `x-api-key` from your llmux config.

Do not expose mutating llmux endpoints to an untrusted network without the API key.

## Troubleshooting

### The island is blank or says llmux is not running

Start or restart the daemon:

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
