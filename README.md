# llmux

**Models change every month. Your harness shouldn't.**

<p align="center">
  <a href="#install">install</a> · <a href="#quick-start">quick start</a> · <a href="#switching-models">models</a> · <a href="docs/README.md">docs</a> · <a href="docs/remote.md">remote daemon</a> · <a href="docs/llmux-islands.md">islands</a>
</p>

---

![llmux demo](https://github.com/2lab-ai/llmux/releases/latest/download/llmux-demo.gif)

**One agent harness, every model.** llmux is a local Anthropic-compatible proxy for [Claude Code](https://www.anthropic.com/claude-code): `claude` talks to `http://localhost:3456`, llmux decides which account/backend serves the request. Your subagents, slash commands, MCP servers, hooks, and `CLAUDE.md` conventions stay put while frontier models and subscription limits keep moving — `/model fable`, `/model gpt-5.6-sol`, `/model grok-4.6` are routing signals, not migrations.

- **one Rust binary** — daemon, live TUI dashboard, login/import, updater, and a Claude Code launcher (`llmux run`)
- **three backend groups in one pool** — Claude (subscription + API key), Codex (`gpt-*` / ChatGPT), Grok (`grok-*` / xAI), routed by model name ([models →](docs/models.md))
- **multi-account scheduling** — quota-aware perishability scoring or sticky round-robin, 429 cooldown parking, Fable weekly ceilings ([schedulers →](docs/schedulers.md))
- **DevTools for your agent's model traffic** — live per-request receipts, a raw request/response viewer over all four wire legs, copy-as-curl ([the accidental AI debugger →](docs/ai-debugger.md))
- **remote-first** — one central daemon, every other machine a pure client, with per-machine multi-tenant keys ([remote daemon →](docs/remote.md))
- **llmux Islands** — native macOS menu-bar/notch companion, plus a KDE/Qt port ([islands →](docs/llmux-islands.md))

The bet behind it — the model is a consumable, the harness is capital — is in [why llmux exists](docs/why-llmux.md). The complete feature list lives in [what ships today](docs/features.md).

## install

```bash
brew install 2lab-ai/tap/llmux
```

Rolling preview channel:

```bash
brew install 2lab-ai/tap/llmux-preview
```

Optional native macOS companion (the KDE port is a [source build](llmux-islands-linux/README.md)):

```bash
brew install 2lab-ai/tap/llmux-islands
```

Build from source:

```bash
git clone https://github.com/2lab-ai/llmux && cd llmux
just build    # cargo build --release --locked
```

## quick start

Add accounts:

```bash
llmux login           # Claude subscription OAuth; repeat once per account
llmux login --api     # optional: Anthropic API key
llmux login --codex   # optional: Codex / ChatGPT subscription
llmux login --grok    # optional: Grok / xAI (device-code flow)
llmux import          # or import supported local credential stores
```

Run Claude Code through llmux:

```bash
llmux run             # starts/reuses the daemon, then launches claude
alias lx='llmux run'  # a convenient alias; args after -- pass through to claude
```

Want the foreground TUI dashboard instead:

```bash
llmux server
```

Manual shell wiring also works: `eval "$(llmux env)"`, then `claude`.

## switching models

Claude Code's model name becomes the routing signal:

```text
/model fable
/model opus[1m]
/model gpt-5.6-sol[1m]
/model grok-4.6
```

| Name pattern | Backend group |
| --- | --- |
| Claude-like (`fable`, `opus`, `sonnet`, `haiku`, `claude-*`) | Claude accounts |
| `gpt-*` / `codex` / aliases (`sol`, `terra`, `luna`) | Codex accounts |
| `grok` / `grok-*` | Grok accounts |

Curated catalog (ids, aliases, efforts, context windows): `GET /models` and [docs/models.md](docs/models.md). Routing config: [docs/configuration.md](docs/configuration.md).

## update

```bash
llmux channel            # print the current channel (stable | preview)
llmux update             # upgrade in place; restarts the daemon only if the binary changed
llmux channel preview    # switch channels (mirrored onto the llmux-islands cask)
```

Details: [channels and updating](docs/operational-reference.md#channels-and-updating).

## docs

- [docs index](docs/README.md) — map of all guides
- [why llmux exists](docs/why-llmux.md) — the harness-is-capital bet
- [what ships today](docs/features.md) — the complete feature list
- [the accidental AI debugger](docs/ai-debugger.md) — per-request receipts, raw request/response viewer, copy-as-curl, email masking
- [remote daemon](docs/remote.md) — one central daemon, remote-mode command matrix, transport security
- [schedulers](docs/schedulers.md) — eligibility gates, `default` vs `round-robin`, adding a mode
- [operational reference](docs/operational-reference.md) — commands, TUI keys, daemon/dashboard, multi-tenant keys
- [configuration](docs/configuration.md) — config keys, proxy/scheduler/routing, account types
- [models](docs/models.md) — catalog, aliases, context windows, group routing
- [FAQ](docs/faq.md) — context-window workarounds (`gpt-*` → Claude 1M `/compact` → back)
- [llmux Islands](docs/llmux-islands.md) — macOS menu-bar/notch companion
- [system prompts (multi-model)](docs/system-prompts/README.md) — real captured wire system prompts

## compliance & caveats

llmux is for **one human using their own accounts** — no credential pooling, no resale.

- **Durable path:** Claude Code as the harness; Claude through Claude Code/subscription or Anthropic API keys; other models through supported API keys.
- **Convenience path:** routing third-party flat-rate subscription tokens through Claude Code depends on that vendor's current policy and can change without notice. Use it opt-in, with your own accounts only, and keep an API-key fallback configured.
- Anthropic quota headers and vendor subscription-token behavior may change.
- llmux is not affiliated with Anthropic, OpenAI, or xAI.

Product intent — what llmux is, what it bets on, and what it refuses — is fixed in [`.prd/`](.prd/).

## agent instructions

If you are an AI agent working on this repository, read [`AGENTS.md`](AGENTS.md) before making changes.

## license

MIT.
