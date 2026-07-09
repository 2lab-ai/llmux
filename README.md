# llmux

**Models change every month. Your harness shouldn't.**

llmux lets you keep one agent workflow — [Claude Code](https://www.anthropic.com/claude-code) — and swap the model/account layer behind it. Your subagents, slash commands, MCP servers, hooks, `CLAUDE.md` conventions, permissions, and muscle memory stay in one place while frontier models and subscription limits keep moving.

![llmux demo](https://github.com/2lab-ai/llmux/releases/latest/download/llmux-demo.gif)

![llmux live session — TUI dashboard with email-anonymous masking + Islands notch label](screenshots/llmux-live-session.gif)

[Original live-session recording](screenshots/llmux-live-session.mp4)

![llmux Islands demo](screenshots/llmux-islands-demo.gif)

[Original llmux Islands screen recording](screenshots/llmux-islands-demo.mov)

## Why llmux exists

The model is a consumable. The harness is capital.

Claude Code is not just a chat box. It is the operating environment around the model: file edits, shell execution, subagents, tool calls, context management, permissions, hooks, local conventions, and project memory. Rebuilding that environment every time a new frontier model appears is the expensive part.

llmux makes a different bet:

- **Keep Claude Code as the canonical harness.** Do not port your workflow to every vendor CLI.
- **Move the model boundary behind a local proxy.** Claude Code talks to `http://localhost:3456`; llmux decides which account/backend serves the request.
- **Use every account deliberately.** Multiple Claude subscription/API-key accounts and optional Codex accounts live in one cockpit, with quota-aware routing instead of manual juggling.
- **Treat model choice as a setting, not a migration.** `opus`, `sonnet`, `gpt-5.5`, and future model names become routing signals, not reasons to rebuild your agent stack.

The result: your workflow stays still while the model market moves.

## The problem llmux removes

1. **Harness lock-in.** A Claude Code workflow does not transfer cleanly to Codex CLI or Gemini CLI.
2. **Sync drift.** Even if you port once, every harness evolves separately. Keeping them equivalent becomes its own job.
3. **Model lock-in.** Trying a better model often means abandoning the harness you already invested in.
4. **Subscription friction.** Flat-rate accounts are useful only if you can route work to the right account before the quota window disappears.

llmux breaks the chain by standardizing on one harness and making the account/model layer swappable behind it.

## What ships today

- **Local Anthropic-compatible proxy** for Claude Code: `ANTHROPIC_BASE_URL=http://localhost:3456` is the integration contract.
- **One Rust binary, `llmux`**, with daemon, login/import, dashboard, status, account management, and Claude Code launch commands.
- **Multi-account Claude scheduling** across subscription and API-key accounts.
- **Model-to-backend routing**: Claude-like model names route to Claude accounts; `gpt-*` / `codex` model names can route to Codex accounts.
- **Codex backend adapter** that translates Claude Code Messages requests into the Codex Responses backend and streams Anthropic-style SSE back to Claude Code.
- **Detached daemon + live TUI dashboard** for quota windows, account health, routing, and manual switching.
- **llmux Islands**, a native macOS menu-bar/notch companion for glanceable usage and screen-share-safe email masking. See [docs/llmux-islands.md](docs/llmux-islands.md).

## Install

```bash
brew install 2lab-ai/tap/llmux
```

Optional native macOS companion:

```bash
brew install 2lab-ai/tap/llmux-islands
```

Rolling preview channel:

```bash
brew install 2lab-ai/tap/llmux-preview
```

Build from source:

```bash
git clone https://github.com/2lab-ai/llmux && cd llmux
just build    # cargo build --release --locked
```

## Quick start

Add accounts:

```bash
# Claude subscription OAuth; repeat once per account
llmux login
llmux login

# Optional: Anthropic API key
llmux login --api

# Optional: Codex / ChatGPT subscription account
llmux login --codex

# Or import supported local credential stores
llmux import
```

Start the dashboard explicitly if you want the foreground TUI:

```bash
llmux server
```

Or run Claude Code directly through llmux:

```bash
llmux run
```

`llmux run` starts or reuses the daemon, then launches `claude` with `ANTHROPIC_BASE_URL` pointed at the local proxy. Arguments after `--` are passed through to Claude Code.

A convenient alias:

```bash
alias lx='llmux run'
lx
```

Manual shell wiring also works:

```bash
eval "$(llmux env)"
claude
```

## Staying current

llmux is distributed through the [Homebrew tap](https://github.com/2lab-ai/homebrew-tap) on two channels: `stable` (formula `llmux`) and `preview` (formula `llmux-preview`). The active channel is derived from what brew has installed — there is no config field.

```bash
# Print the current channel
llmux channel

# Update in place on the current channel (brew upgrade), restarting the
# daemon only if the binary actually changed
llmux update

# Switch channels now (brew uninstall old + install new, mirrored onto the
# llmux-islands cask; restarts a running daemon)
llmux channel preview
llmux channel stable
```

## Switching models

Claude Code's model name becomes the routing signal:

```text
/model opus
/model sonnet
/model gpt-5.5
/model gpt-5.5[1m]
```

With default routing, Claude-like names use the Claude account group and `gpt-*` / `codex` names use the Codex group. The full routing rules, config keys, and override syntax are in [docs/configuration.md](docs/configuration.md) and [docs/operational-reference.md](docs/operational-reference.md).

## Schedulers

Which account serves the next request is decided by the scheduler. Two algorithms ship; switch live with `S` in the TUI (persisted to `scheduler.mode`), or `POST /llmux/scheduler-mode {"mode": "default" | "round-robin"}`.

**Why switching matters:** the upstream prompt cache is scoped per account — every account switch invalidates it, and the next request re-reads the full conversation context uncached (token cost + latency). Both schedulers are therefore sticky on the current account; they differ in *when* they move and *who* is next.

### Eligibility (both modes)

An account can be picked only when ALL of these hold — the same pure gate drives selection, the TUI status column, and `/llmux/status`, so they can never disagree:

- auth healthy, not operator-**paused** (`p` in the TUI switcher, context menu in llmux-islands)
- not cooling down (429 `retry-after` park)
- 5h utilization ≤ `scheduler.five_hour_max` (default **0.90**)
- 7d utilization ≤ `scheduler.seven_day_max` (default **0.99**)
- usage data fresh (≤ `usage_max_age_secs`; if ALL accounts are stale, the gate degrades to headers-only mode instead of stalling)

Per-account overrides: `account_limits` in the config (TUI: `L` in the switcher, `"90,98,98"` = 5h,7d,fbl percents) replace any of the three ceilings for that account.

**Fable scope:** Fable-family requests are additionally refused an account whose Fable weekly bucket is constraining (≥ `scheduler.fable_weekly_max`, default **0.98**, reset-aware) or Fable-cooling. Non-Fable traffic ignores Fable state entirely — a Fable-exhausted account still serves everything else.

### `default` — quota-maximizing

Ranks eligible accounts by `score = servable_now × urgency`: `servable_now` = min(5h, 7d headroom below the ceilings) — the binding limit wins; `urgency` = 1–4× as the 7d reset approaches — soon-to-reset budget is perishable, so it burns first, while long-runway accounts are preserved as reservoirs. Sticky on the current account, but proactively switches when another account scores >25% higher (`SWITCH_MARGIN`) — it trades some cache locality for not letting quota expire unused.

### `round-robin` — sequential exhaust (fewest switches)

Stays on the current account until it is **hard ineligible** (ceiling hit, cooldown, auth, pause) — never a proactive switch — then moves to the **next account in roster order**, wrapping. Deterministic, score-free, and the minimum possible number of switches, at the cost of letting other accounts' soon-to-reset quota expire unused. Pick this when prompt-cache locality (long agent sessions) matters more than squeezing every window.

### Adding a scheduler

The selection logic is pure and lives in `src/scheduler/select.rs` (`pick_scoped`, `rank`, `round_robin_next`) — deterministic functions of `(PoolSnapshot, SelectParams, now)`, unit-tested without IO. To add a mode: extend `SchedulerMode` in `src/config/schema.rs`, branch in `pick_scoped`/`next_in_line`/`selection_order`, and document it here.

## FAQ

### `gpt-5.5` stops around 265k context. What should I do?

If Claude Code blocks a `gpt-5.5` session around ~265k tokens even after selecting `gpt-5.5[1m]`, switch temporarily to a Claude model with a 1M context window, compact there, then switch back:

```text
/model opus[1m]      # or /model sonnet[1m]
/compact
/model gpt-5.5[1m]
```

This is a practical Claude Code context-management workaround: use the 1M Claude model for the compaction step, then continue routing work to `gpt-5.5[1m]` through llmux. More context-window notes live in [docs/faq.md](docs/faq.md).

## Documentation

- [Docs index](docs/README.md) — the map for detailed guides.
- [Operational reference](docs/operational-reference.md) — commands, TUI keys, daemon/dashboard behavior, scheduling policy, model routing details, and Codex backend behavior.
- [Configuration](docs/configuration.md) — config file location, proxy keys, scheduler knobs, routing options, Codex settings, and account types.
- [FAQ](docs/faq.md) — context-window workarounds and common usage questions.
- [llmux Islands](docs/llmux-islands.md) — macOS menu-bar/notch companion, privacy modes, and recording setup.
- [Contributor guide](AGENTS.md) — architecture rules and development conventions.

## Compliance & caveats

llmux is for **one human using their own accounts** — no credential pooling, no resale.

- **Durable path:** Claude Code as the harness; Claude through Claude Code/subscription or Anthropic API keys; other models through supported API keys.
- **Convenience path:** routing third-party flat-rate subscription tokens through Claude Code depends on that vendor's current policy and can change without notice. Use it opt-in, with your own accounts only, and keep an API-key fallback configured.
- Anthropic quota headers and vendor subscription-token behavior may change.
- llmux is not affiliated with Anthropic or OpenAI.

The product intent — what llmux is, what it bets on, and what it refuses — is fixed in [`.prd/`](.prd/) as the source of truth.

## License

MIT.
