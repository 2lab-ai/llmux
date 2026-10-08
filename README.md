# llmux

**Models change every month. Your harness shouldn't.**

<p align="center">
  <a href="#install">install</a> · <a href="#quick-start">quick start</a> · <a href="#switching-models">models</a> · <a href="docs/README.md">docs</a> · <a href="docs/remote.md">remote daemon</a> · <a href="docs/llmux-islands.md">islands</a>
</p>

---

![llmux demo](https://github.com/2lab-ai/llmux/releases/latest/download/llmux-demo.gif)

**Keep your harness, change your model.** llmux is a local proxy for Claude Code and Codex CLI, with Anthropic Messages and OpenAI Responses endpoints. The selected client talks to `http://localhost:3456`; llmux chooses the account/backend. Your client keeps its own tools, permissions and project conventions while `/model fable`, `/model gpt-5.6-sol` and `/model grok-4.7` select the backend. llmux does not synchronize settings between the two clients.

- **Claude Code or Codex frontend** — `llmux run --codex` launches Codex against the OpenAI Responses endpoint, with a catalog picker and Claude Agent SDK routing ([Codex guide →](docs/codex-frontend/README.md)). Claude-through-Codex requires Node.js/npm on the daemon; SDK controls differ from native Messages.

- **one Rust binary** — daemon, live TUI dashboard, login/import, updater, and launchers for Claude Code (`llmux run`) and Codex (`llmux run --codex`)
- **four backend groups in one pool** — Claude (subscription + API key), Codex (`gpt-*` / ChatGPT), Grok (`grok-*` / xAI), OpenRouter (`or-*` / free models on an OpenRouter key), routed by model name ([models →](docs/models.md))
- **multi-account scheduling** — quota-aware perishability scoring or sticky round-robin, 429 cooldown parking, Fable weekly ceilings ([schedulers →](docs/schedulers.md))
- **DevTools for your agent's model traffic** — live per-request receipts, a raw request/response viewer with explicit SDK transport boundaries, copy-as-curl ([the accidental AI debugger →](docs/ai-debugger.md))
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
llmux login --openrouter  # optional: OpenRouter (browser PKCE; --paste for an existing key)
llmux import          # or import supported local credential stores
```

Already looking at the dashboard? `n` opens a provider picker for the same four browser logins (Claude / Codex / Grok / OpenRouter) — the flow runs in the client and the credential is injected into the daemon, so it works attached to a remote one too.

Run Claude Code through llmux:

```bash
llmux run             # starts/reuses the daemon, then launches claude
alias lx='llmux run'  # a convenient alias; args after -- pass through to claude
```

Inside that session `/model` lists the llmux [catalog](docs/models.md#claude-code-model-picker) — every codex/grok/openrouter id too, not just the built-in Claude rows. The same launch exports `ANTHROPIC_DEFAULT_{OPUS,FABLE,SONNET,HAIKU}_MODEL` from the catalog's alias owners, so `/model opus` — which Claude Code resolves natively, before llmux ever sees it — lands on `claude-opus-5-5[1m]` and its 1M window instead of the client's 200k default ([alias exports](docs/models.md#alias-exports); a var you already export is left alone). `--no-model-picker` opts out of both.

Or run Codex CLI through the same daemon (install Codex on the client):

```bash
llmux run --codex
llmux run --codex -- exec -m haiku 'Run the tests and explain failures'
```

Claude models through Codex require Node.js 18+ and npm on the **daemon host**; the first request installs the pinned SDK automatically. `login --codex` adds a ChatGPT account; `run --codex` chooses the client and also works with only Claude accounts. See the [Codex frontend guide](docs/codex-frontend/README.md).

Want the foreground TUI dashboard instead:

```bash
llmux server
```

Manual shell wiring also works: `eval "$(llmux env)"`, then `claude`.
For Codex, `llmux env --codex` prints OpenAI-compatible exports and the explicit
provider command required after `eval`; see the [manual setup](docs/operational-reference.md#running-codex-through-llmux).

## switching models

The incoming model name selects the backend in either client. For example, inside Claude Code:

```text
/model fable
/model opus[1m]
/model gpt-5.6-sol[1m]
/model grok-4.7
/model or-ox-alpha
```

| Name pattern | Backend group |
| --- | --- |
| Claude-like (`fable`, `opus`, `sonnet`, `haiku`, `claude-*`) | Claude accounts |
| `gpt-*` / `codex` / aliases (`sol`, `terra`, `luna`) | Codex accounts |
| `grok` / `grok-*` | Grok accounts |
| `or` / `or-*` / `openrouter/*` | OpenRouter accounts |

Curated catalog (ids, aliases, efforts, context windows): `GET /models` and [docs/models.md](docs/models.md). Routing config: [docs/configuration.md](docs/configuration.md).

> **Same request, different backend — read [provider compatibility](docs/provider-compatibility.md) before you trust a field.** The following caveats describe **incoming Anthropic Messages**: Claude and OpenRouter use native Messages, while Codex and Grok require translation. Incoming **OpenAI Responses** uses native Codex Responses or the Claude Agent SDK, with different controls; see the [frontend transport matrix](docs/provider-compatibility.md#frontend-transport-matrix).
>
> - **`gpt-*` (Codex): no output-limit guarantee — your `max_tokens` is not sent upstream at all.** The gateway answered `400 Unsupported parameter: max_output_tokens` (live probe 2026-09-14), and no supported alternative cap field **was found** in the current official Codex client or its docs (read 2026-09-14), so llmux omits the cap rather than faking one. That is a search result, not an allowlist: other field names are untested, not proven absent.
> - **`grok-*`: the cap is forwarded, but it is not the budget you asked for.** A `max_output_tokens: 1` probe (2026-09-14) came back `incomplete` with one visible token and 168 reported output tokens, 167 of them reasoning. What it bounds in general — and what it costs — is unmeasured.
> - **Both:** non-null `temperature` / `top_p` / `top_k` and **non-empty** `stop_sequences` are refused with a local 400 (except [scoped monitor/title adapters](docs/provider-compatibility.md#claude-code-internal-tasks)), prior `thinking` blocks are dropped, and there is no reasoning continuity across turns.
> - **Claude Code auto mode:** after an observed GPT main turn, the same session's safety monitor defaults to `luna` with medium effort. Both severity stages are supported; unknown sessions keep normal routing. This changes the reviewer model, not the requested safety policy.
> - **Claude Code internal models:** in GPT sessions, implicit Opus/Sonnet/Haiku requests use configurable `sol`/`terra`/`luna` aliases. Titles preserve their JSON schema; initial titles and quota use proven launch context before the first main turn. Quota checks Codex availability without inventing Claude quota percentages. Explicit main Claude switches remain authoritative. [Configuration](docs/configuration.md#claude-code-internal-models-claude_code).
>
> A translated response that lost something names it in `X-Llmux-Omitted-Fields` / `X-Llmux-Compatibility-Warnings` (a faithful one carries neither header); send `X-Llmux-Compatibility: strict` to turn any such loss into a 400 instead. Full matrix, receipts and known unknowns: [docs/provider-compatibility.md](docs/provider-compatibility.md).

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
- [Codex frontend](docs/codex-frontend/README.md) — launch, SDK requirements, tools, HTTP and endpoint activity
- [configuration](docs/configuration.md) — config keys, proxy/scheduler/routing, account types
- [models](docs/models.md) — catalog, aliases, context windows, group routing
- [provider compatibility](docs/provider-compatibility.md) — per-backend difference matrix: dropped/refused request fields, `max_tokens` on Codex/Grok, diagnostic headers
- [FAQ](docs/faq.md) — context-window workarounds (`gpt-*` → Claude 1M `/compact` → back)
- [llmux Islands](docs/llmux-islands.md) — macOS menu-bar/notch companion
- [system prompts (multi-model)](docs/system-prompts/README.md) — real captured wire system prompts

## compliance & caveats

llmux is for **one human using their own accounts** — no credential pooling, no resale.

- **Durable path:** keep your chosen client harness; Claude through Claude Code/subscription or Anthropic API keys; other models through supported API keys.
- **Convenience path:** routing third-party flat-rate subscription tokens through a different client depends on that vendor's current policy and can change without notice. Use it opt-in, with your own accounts only, and keep an API-key fallback configured.
- Anthropic quota headers and vendor subscription-token behavior may change.
- llmux is not affiliated with Anthropic, OpenAI, xAI, or OpenRouter.

Product intent — what llmux is, what it bets on, and what it refuses — is fixed in [`.prd/`](.prd/).

## agent instructions

If you are an AI agent working on this repository, read [`AGENTS.md`](AGENTS.md) before making changes.

## license

MIT.
