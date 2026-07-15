# Architecture

This document describes the shipped system at component and flow level. The
maintained product contract lives in [`.prd/01-spec.md`](../.prd/01-spec.md),
and the source-oriented architecture record lives in
[`02-architecture.md`](../.prd/02-architecture.md).

![llmux architecture overview](assets/architecture-overview.png)

[Open the diagram at full size](assets/architecture-overview.html).

## Runtime topology

One `llmux` process contains the HTTP proxy, scheduler state, provider
adapters, background maintenance, dashboard fold, and control API. The same
binary also exposes client commands that start, attach to, or inspect that
daemon.

| Component | Responsibility | Primary source |
| --- | --- | --- |
| CLI and daemon lifecycle | Parse commands, probe/spawn/stop, launch Claude Code, remote targeting | `src/cli/` |
| Proxy server | Bind the listener, expose `/llmux/*`, enforce client auth on every route, run background tasks | `src/proxy/server.rs` |
| Forward path | Classify requests, lease an account, refresh auth, retry, and stream | `src/proxy/forward.rs` |
| Router | Classify model strings into Claude, Codex, or Grok | `src/routing.rs` |
| Scheduler | Track windows/cooldowns and select within a group | `src/scheduler/` |
| Providers | Anthropic passthrough path; Codex and Grok Responses translation | `src/provider/` |
| Dashboard | Fold runtime events into stable status/dashboard documents | `src/dashboard.rs` |
| TUI | Render local state or an attached dashboard document | `src/tui/` |
| Native clients | Shared Rust semantics with native macOS and KDE shells | `llmux-islands-core/`, `llmux-islands*/` |

## Request flow

1. Claude Code sends an Anthropic Messages request to the configured llmux
   endpoint.
2. The router extracts the model and chooses a backend group.
3. The scheduler evaluates health, pauses, cooldowns, quota ceilings, freshness,
   and group stickiness.
4. The selected account is leased. That lease pins the credential for the
   entire request even if the group's current account changes later.
5. The forward path refreshes an expiring credential when needed and injects
   provider auth.
6. Claude traffic stays on the Anthropic passthrough path except for
   auth/header replacement and client-only `[1m]` model normalization. Codex
   and Grok requests are converted to Responses-family JSON and their SSE is
   converted back into Anthropic Messages events.
7. Usage headers, emitted usage events, completion state, cost projections,
   and receipts fold back into scheduler/dashboard state.
8. The lease is released when the request ends or the client disconnects.

## Account and scheduler state

The scheduler maintains independent current slots for Claude, Codex, and Grok,
plus a Fable-scoped Claude slot so dedicated Fable selection does not displace
ordinary Claude traffic. Account state includes credential health, in-flight
leases, cooldowns, quota windows, freshness, operator pause, and optional
account-specific ceilings.

Claude subscription windows are refreshed from the OAuth usage endpoint and
response headers. Codex windows are observed from traffic and a bounded idle
probe. Grok exposes reset-less rate-limit evidence rather than the same 5h/7d
quota surface and is never idle-probed. The scheduler normalizes these inputs
without pretending they are identical.

The pure selection functions operate on snapshots; runtime methods apply the
result only after revalidating current state. In-flight work is never migrated
between accounts.

## Background work

The daemon runs bounded maintenance beside the listener:

- Claude usage polling with backoff;
- OAuth refresh before expiry;
- scheduler re-evaluation and reset/cooldown healing;
- cold-account Codex/API-key probing when enabled;
- activity-history persistence/hydration and raw-I/O retention pruning; and
- dashboard publication for attached clients.

These jobs are why `llmux` is daemon-first rather than a wrapper that exists
only for the duration of one Claude Code process.

## Control surfaces

The local TUI and remote attach mode render the same dashboard contract.
llmux Islands uses the same daemon document and a shared Rust UI state/reducer,
but keeps platform-native shells:

- macOS: SwiftUI/AppKit through a versioned Rust C ABI bridge;
- Linux/KDE: Qt 6, QML, Kirigami, CXX-Qt, and LayerShellQt where available.

Actions such as pause, settings updates, events, login orchestration, and
verification receipts remain semantic operations. Window placement,
notifications, sound, tray behavior, and clipboard integration stay native.

## Network and trust boundary

The daemon listens on all interfaces so remote clients can reach it. Loopback
requests are exempt from the proxy control key; off-loopback requests require
`proxy.api_key`. The built-in transport is HTTP, not HTTPS. Remote deployments
therefore require a trusted encrypted overlay such as Tailscale or WireGuard.

The system is single-user. The API-key gate is an ownership check, not
multi-tenant isolation and not transport encryption.

## Durable files

| Data | Default location | Notes |
| --- | --- | --- |
| Config and credentials | `~/.config/llmux.json` | mode `0600`; reload-before-mutate and atomic replace prevent torn files, but do not serialize overlapping writers |
| Raw request/response log | `$XDG_STATE_HOME/llmux/raw-io.jsonl` | sensitive; bounded by retention/body settings |
| Responses-provider trace | `$XDG_STATE_HOME/llmux/codex-trace.jsonl` | shared by Codex and Grok records; each provider has its own enable flag |
| Islands connection/UI settings | platform-specific app config | never owns provider credentials |

## Build and distribution

GitHub release and preview workflows build four CLI binaries:
`aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu`, and `aarch64-unknown-linux-gnu`. The same release
also packages the macOS Islands app. The Linux Islands shell is verified in a
clean Arch container and distributed today through the repository `PKGBUILD`,
not as a GitHub release asset, pacman repository, AUR package, or xbrew recipe.

See [Getting started](getting-started.md) for user installation and the
[decision archive](../.prd/README.md) for the implementation history.
