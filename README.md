# llmux

**Models change every month. Your harness shouldn't.**

llmux lets you build your agent workflow once on a single canonical harness — [Claude Code](https://www.anthropic.com/claude-code) — and swap the *model* behind it freely. Wherever the next frontier model ships, you don't re-port your setup.

![llmux demo](https://github.com/2lab-ai/llmux/releases/latest/download/llmux-demo.gif)

![llmux live session — TUI dashboard with email-anonymous masking + Islands notch label](screenshots/llmux-live-session.gif)

[Original live-session recording](screenshots/llmux-live-session.mp4)

![llmux Islands demo](screenshots/llmux-islands-demo.gif)

[Original llmux Islands screen recording](screenshots/llmux-islands-demo.mov)

## The problem

Frontier models keep coming out of the big labs, and "current best" moves often. But each vendor's CLI agent **harness** — the operating layer around the model: file edits, shell execution, tool calls, context management, permissions — evolves independently and is mutually incompatible. That creates four layers of pain:

1. **Can't port.** A workflow built in Claude Code (subagents, slash commands, MCP servers, `CLAUDE.md` conventions, hooks) does not transfer to Codex CLI or Gemini CLI as-is.
2. **Can't sync.** Even after a painful port, each harness keeps improving separately — there's no way to keep two environments in the same state. The gap only widens.
3. **Model lock-in.** Moving to a better model means moving your *entire harness*. Your tooling investment holds your model choice hostage.
4. **Subscription lock-in.** Flat-rate subscriptions are bound to each vendor's first-party client — you can't drive a Claude subscription from a third-party tool.

**Root cause:** the valuable asset (your workflow) is bound to the harness, and the harness is bound to the model and vendor. llmux breaks that chain by standardizing on **one** harness and making the model a swappable part behind it.

## The thesis

What you actually want to preserve isn't a specific *model* — it's the *harness environment* you built. The model is a consumable; the harness is capital.

So llmux adopts Claude Code as **the one canonical harness** and turns the model into a part you swap behind it. Model-switch cost drops from "rebuild your harness" to "one setting." You keep your subagents, your slash commands, your `CLAUDE.md` — and point them at whichever model is best this month.

## What ships today

A local proxy that sits behind Claude Code (`ANTHROPIC_BASE_URL=http://localhost:3456` is the whole integration contract) and routes requests to a backend you control:

- **One Rust binary, `llmux`** — `server`, `run`, `stop`, `restart`, `login`, `import`, `env`, `dashboard`, `status`, `accounts`, `remove`, `api`.
- **Claude Code stays unmodified.** `llmux run` starts (or reuses) the local daemon and launches `claude` pointed at the proxy.
- **Multiple accounts, one cockpit.** Manage several Claude subscription/API-key accounts plus optional OpenAI Codex accounts, and switch between them without leaving Claude Code.
- **Perishability-aware scheduling.** Each Claude account tracks its 5-hour and 7-day quota windows from upstream headers + OAuth usage polling. Eligible accounts are scored by *usable burst now × weekly-quota perishability*, so quota that resets soon (and would otherwise evaporate unused) is burned first while long-runway accounts are preserved as a reservoir. Sticky per session, never per request — it only switches when another account is clearly worth more.
- **Model selects the backend.** Out of the box the request's `model` chooses the backend group: `claude-*`/`opus`/`sonnet` land on a Claude account, `gpt-*`/`codex` land on an OpenAI Codex account. The Codex provider translates the Anthropic Messages API into the Codex Responses backend and streams it back as Anthropic SSE — so Claude Code talks to GPT without knowing it.
- **Daemon-first + attach-mode dashboard.** A detached daemon keeps polling and refreshing tokens; `llmux dashboard` renders the live ratatui view from it.
- **llmux Islands.** A native macOS menu-bar/notch companion shows the same account usage at a glance, including animated floating-island activity and an email-anonymous mode for safe screen sharing. See [the Islands guide](docs/llmux-islands.md).

This is **Tier 1 + a working slice of Tier 2** (see below): multi-account Claude plus model→backend routing already ship; the endgame is per-subagent cross-provider routing inside one Claude Code session.

## Two tiers: where we bet, what's convenience

llmux draws a hard line between what it stakes its identity on and what is a best-effort convenience that depends on vendor policy.

| | **Tier 1 — durable (the identity)** | **Tier 2 — convenience (bonus)** |
|---|---|---|
| What | Claude Code as the single harness. Claude via subscription (through Claude Code), other models via **API key**. | Routing non-Anthropic models through *their* flat-rate subscription, where the vendor currently allows it. |
| Compliance | Fully compliant, stable. | Vendor-policy-dependent, gray, mutable. |
| Value | Solves painpoints 1–4. ~90% of the value. | Flat-rate savings. Can break without notice. |

We put the product's identity in Tier 1. Tier 2 is offered opt-in, with an explicit "works now, no guarantee" warning — so the product's lifespan isn't hostage to the next vendor policy change.

## Roadmap

```
[shipping] Model-level routing
        You pick a MODEL; llmux maps model -> subscription/key transparently.
        claude-* -> a Claude account, gpt-* -> a Codex account. On by default.
        Plus multi-account quota scheduling within each backend group.
          |
          v
[next]  Per-subagent cross-provider
        In one Claude Code session:
          main agent  = a Claude model   (Anthropic subscription, native to Claude Code)
          subagents   = gpt-5.5          (OpenAI backend, via the router)
        Wire Claude Code's subagent `model` field to a backend mapping.
        "GPT subagents inside the Claude Code harness, naturally" — the endgame.
```

Claude Code already supports in-session model switching and per-task routing, and subagents already carry a `model` field (`.claude/agents/*.md`). The endgame composes those existing mechanisms — the router just maps the model string to a different backend. (Model names move fast; `fable-5` / `gpt-5.5` are illustrative of the *shape*, replaced by whatever is current.)

## Non-goals

- **Not a new harness.** llmux attaches above/below Claude Code; it does not replace it. (Competing on harness features is a losing game — Claude Code is overwhelmingly harness code, and that's the moat.)
- **Not model laundering.** Route to a weaker model and you get that model's quality. llmux unifies the UX; it cannot raise intelligence.
- **Not a policy-circumvention product.** Vendor-policy gray zones live in Tier 2, opt-in and clearly marked — never the identity.

## Install

```bash
brew install 2lab-ai/tap/llmux
```

Native macOS Islands companion:

```bash
brew install 2lab-ai/tap/llmux-islands
```

Rolling preview channel:

```bash
brew install 2lab-ai/tap/llmux-preview
```

Or build from source:

```bash
git clone https://github.com/2lab-ai/llmux && cd llmux
just build    # cargo build --release --locked
```

## Quick start

```bash
# Add accounts — browser OAuth, one login per account
llmux login
llmux login

# Or import existing credentials from supported local stores
llmux import

# Start the proxy with the foreground TUI when attached to a TTY
llmux server

# In another terminal, run Claude Code through the proxy
llmux run
```

`llmux run` spawns `claude` with only `ANTHROPIC_BASE_URL` set and passes arguments through after `--`. If nothing is listening on the configured port, `run` auto-starts a detached daemon (stderr at `~/.local/state/llmux/server.log`, respecting `$XDG_STATE_HOME`) and waits until it is ready. A port occupied by a foreign process is an error, never spawned over.

A convenient alias, so launching Claude Code through llmux is one word:

```bash
alias lx='llmux run'
lx
```

Manual shell wiring also works:

```bash
eval "$(llmux env)"
claude
```

For the native macOS usage companion, build and launch `llmux-islands` after the daemon is running. The full setup, privacy, and recording guide is in [docs/llmux-islands.md](docs/llmux-islands.md).

## Documentation

The operational reference — the full command table & TUI keys, daemon/dashboard model, configuration reference, the scheduling policy, model routing (including the gpt-5.5 context-window workaround), and the Codex backend — lives in **[README.detail.md](README.detail.md)**.

- [llmux Islands](docs/llmux-islands.md) — macOS menu-bar/notch companion, floating activity label, email-anonymous mode, and demo recording.
- [Configuration](docs/configuration.md) — config file, proxy, scheduler knobs, model routing, Codex request shaping, and account types.
- [Contributor guide](AGENTS.md) — architecture rules and development conventions.

## Compliance & caveats

llmux is for **one human using their own accounts** — no credential pooling, no resale.

- **Tier 1 is the safe path.** Claude via subscription through Claude Code, everything else via API key. This is fully compliant and stable.
- **Tier 2 is gray.** Driving a vendor's flat-rate subscription from outside its official client depends on that vendor's current policy and can break or trigger account action without notice. The Codex backend uses ChatGPT subscription tokens outside the official client; OpenAI does not endorse it. Anthropic restricts using Claude subscription tokens outside Claude Code / Claude.ai. Use Tier 2 opt-in, at your own risk, with your own accounts only — and keep an API-key fallback configured.
- Anthropic's unified quota headers are undocumented and may change; the OAuth usage endpoint and 429 + `retry-after` are the fallback evidence chain.
- Not affiliated with Anthropic or OpenAI.

The product intent — what llmux is, what it bets on, and what it refuses — is fixed in [`.prd/`](.prd/) as the source of truth.

## Development

```bash
just check    # cargo fmt --check + cargo clippy --all-targets -- -D warnings + cargo test
just build    # cargo build --release --locked
```

Contributor conventions are in [`AGENTS.md`](AGENTS.md).

## License

MIT.
