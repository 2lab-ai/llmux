# Operational reference

This page is the command-and-cockpit reference. Product concepts live in
[How llmux works](concepts.md); durable settings live in
[Configuration](configuration.md); scheduler policy and remote setup each have
their own task guide.

## CLI

Global option:

| Option | Meaning |
| --- | --- |
| `--remote HOST[:PORT]` | Target a remote daemon for this invocation. Overrides `remote.host`; the port falls back to `remote.port`, then `3456`. |

Commands:

| Command | Description |
| --- | --- |
| `server [--port N] [--no-tui] [--log-to DIR]` | Start the proxy. On a TTY it renders the TUI unless `--no-tui`; `--log-to` writes credential-masked request files. If an llmux daemon already owns the port, attach instead. |
| `run [--force] [-- ARGS…]` | Ensure a local daemon is ready, then launch `claude`; `--force` restarts even a same-version daemon. In remote mode, never starts locally. |
| `stop` | Gracefully stop the local daemon through `/llmux/shutdown`. |
| `restart` | Cooperatively drain the local daemon, then start this installed binary. |
| `login [--api\|--codex\|--grok]` | Add Claude OAuth, Anthropic API-key, ChatGPT/Codex OAuth, or xAI/Grok device-code credentials. |
| `import [--from PATH\|--json JSON]` | Import supported Claude/teamclaude/Codex credential stores or inline JSON. |
| `env` | Print shell exports that point Claude Code at the resolved local or remote endpoint. |
| `dashboard` | Attach to a daemon and render its dashboard document without binding the proxy port. |
| `status [--json]` | Show daemon/update/account state; exit 1 if the target is unavailable. |
| `accounts [-v\|--verbose] [--json]` | List config accounts; verbose adds quota detail, JSON requires a running daemon and emits the live account dashboard. |
| `remove NAME [--yes]` | Remove a local account; `--yes` is required when stdin is not a TTY. |
| `api PATH` | Debug a GET to an upstream path using the current account. |
| `channel [stable\|preview]` | Print or switch the local Homebrew channel, mirroring the compatible macOS Islands cask. |
| `update` | Upgrade the current local Homebrew channel and restart/relaunch only when versions changed. |

Remote behavior is intentionally asymmetric: `run`, `server`, `dashboard`,
`status`, `env`, and `accounts` target the remote; `stop`, `restart`, `remove`,
`login`, and `import` refuse; `channel` and `update` stay local. See
[Remote daemon](guides/remote-daemon.md). The low-level `api` command also
stays local today: it uses a credential from this machine's config to call an
upstream path directly and ignores remote targeting.

## Lifecycle

`llmux run` is the normal entry point. It probes `/llmux/status`, starts a
detached `server --no-tui` when needed, waits for readiness, exports the base
URL to the child, and starts Claude Code. Arguments after `--` pass through.

```bash
llmux run -- --continue
```

Manual wiring:

```bash
eval "$(llmux env)"
claude
```

Daemon stderr goes to `$XDG_STATE_HOME/llmux/server.log` (normally
`~/.local/state/llmux/server.log`). Only one process can own the selected port.
An existing llmux daemon causes `server` to attach; an unrelated listener is a
clean error and is never killed or overwritten.

## TUI navigation

Main view:

| Key | Action |
| --- | --- |
| `a` | Open accounts and account operations. |
| `g` | Open model/statistics view. |
| `U` | Open calendar usage and cost. |
| `s` | Open sessions. |
| `l` | Open logs. |
| `c` | Open effective configuration. |
| `?` | Open help/miscellaneous information. |
| `R` | Reload local configuration. |
| `f` | Toggle Codex priority service tier. |
| `m` | Cycle the configured Codex model pin. |
| `e` | Cycle the configured Codex reasoning effort/bypass. |
| `u` | Toggle quota bar fill between remaining and used. |
| `t` | Toggle reset-time presentation. |
| `S` | Toggle scheduler mode. |
| `j`/`k`, arrows, mouse wheel | Navigate or scroll the active surface. |
| `q` | Quit the client view; it does not stop a detached daemon. |

In the accounts surface, `s` enters Select mode; there, `p` pauses/resumes and
`L` edits per-account limits. From the accounts surface, `n` starts a new
login, `a` adds, and `r` removes. Mutation that belongs to the daemon host is
disabled or refused for a cross-host attach.

## Calendar usage and cost

Press `U` or click the Usage tab. `g` cycles hourly, daily, and monthly
granularity. Navigation uses `j`/`k`, arrows, wheel, `PgUp`/`PgDn`, and
`Home`/`End`.

Each bucket shows request count, input/output/cache-read/cache-write tokens,
and API-equivalent USD by model. Hourly history covers 72 hours, daily history
180 days, and monthly history all retained activity. Boundaries follow the
daemon's local calendar.

Costs are estimates from built-in prices plus `pricing` overrides, not a bill.
Unknown prices render `—`; affected totals carry `+?` and the title shows
`(+unpriced)` rather than misrepresenting missing prices as zero. Attached
clients render the same `usage_stats` payload from `/llmux/dashboard`.

## Activity feed

One row represents one completed request:

```text
▸ HH:MM:SS kind [model effort] account… → 200 3.1s 269tok $0.0079 «session» "input excerpt"
```

- `kind` is classified once from the buffered input (`user`, `count`,
  `compact`, `summary`, `title`, `audit`, `subagent`, `sdk`, `quota`, and
  other harness families). Wire fingerprints are documented in
  [System-prompt families](system-prompts/families.md).
- The model/effort badge is group-colored. Vendor prefixes are shortened for
  display without changing attribution.
- Clicking a row expands method/path, client, account, tokens, cost, and timing.
- Consecutive `count` probes fold into one group; individual members remain
  inspectable.
- Scrolling past memory pages older local `activity.jsonl` history. A
  cross-host attach does not read a file from the client machine because that
  file belongs to a different daemon.

`HEAD|GET /` reachability probes are answered locally. A one-token `quota`
probe receives one upstream attempt and does not sweep every account, keeping
the activity feed honest.

## Routing and upstream models

The incoming model string first selects Claude, Codex, or Grok. The scheduler
then selects an account inside that group. See [Models](models.md) for the
curated catalog and [Configuration](configuration.md#model-routing) for rule
syntax.

### Codex

The account group and upstream model are related but not identical:

- Known accepted IDs such as `gpt-5.5`, `gpt-5.6-sol`,
  `gpt-5.6-terra`, and `gpt-5.6-luna` pass through verbatim.
- `sol`, `terra`, and `luna` resolve to the current `gpt-5.6-*` generation.
- Bare `gpt-5.6` resolves to `gpt-5.6-sol`.
- Unknown, decorated, or absent model strings fall back to
  `codex.default_model`, currently `gpt-5.6-sol`.

This means a display decoration such as `gpt-5.5[1m]` still routes to the
Codex group, but it is not a known passthrough ID and therefore uses the
configured pin upstream. Use bare `gpt-5.5` when exact pass-through is the
goal. See the [FAQ](faq.md#what-does-1m-do-on-a-codex-model-name) for the
client-context trade-off.

Configured `codex.reasoning_effort` overrides the client's
`output_config.effort`. Leave it unset or select bypass in the TUI to preserve
the client value. `codex.fast = true` sends `service_tier: "priority"`.

The adapter converts Anthropic Messages input to OpenAI Responses and converts
Responses SSE back to Anthropic events. Text, reasoning summaries, and tool
calls are supported. Images are currently dropped with a warning.

### Grok

Bare `grok` resolves to `grok.default_model`, currently `grok-4.5`. A concrete
`grok-*` request is forwarded verbatim. The Grok adapter shares the
Responses-family conversion machinery with Codex but uses xAI device-code
credentials, model-specific effort clamping, and its own rate-limit evidence.

Configured `grok.reasoning_effort` likewise overrides the client value; unset
means bypass/backend default. Grok has no Codex priority service tier and is
not included in idle quota probing.

## Scheduling

Two modes ship: perishability-scored `default` and switch-minimizing
`round-robin`. Both apply credential health, pause, cooldown, quota, freshness,
and scoped Fable gates and never move an in-flight request. The full operator
guide is [Scheduling accounts](guides/scheduling.md); exact keys are in
[Configuration](configuration.md#scheduler).

## Privacy and recording

`LLMUX_DEMO_MODE=1` replaces account names with stable fake identities in the
TUI, status, and logs and suppresses config writes. Durable
`email_anonymous = true` masks display surfaces while preserving live state.

The repository contains the current demo media under [`screenshots/`](../screenshots/)
and recording helpers under [`demo/`](../demo/). Releases do not currently
publish those GIFs as assets. macOS recording needs Screen Recording permission
for the terminal that runs the helper.

The Islands-specific privacy and evidence workflow is in
[llmux Islands](llmux-islands.md#privacy).

## Configuration

Config defaults to `~/.config/llmux.json`, respects `$XDG_CONFIG_HOME`, and can
be overridden with `$LLMUX_CONFIG`. It is mode `0600`; updates reload before
mutation and replace atomically. That prevents torn files, but overlapping
cross-process writers can still be last-write-wins. It contains secrets.

The single canonical key/default reference is
[Configuration](configuration.md); this page intentionally does not duplicate
the JSON schema.

## Development

Run repository gates with bounded cargo parallelism:

```bash
CARGO_BUILD_JOBS=2 just check
CARGO_BUILD_JOBS=2 just build
```

Contributor conventions are in [AGENTS.md](../AGENTS.md), and current system
decisions are indexed in the [decision archive](../.prd/README.md).
