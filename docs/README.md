# llmux docs

The product story and quick start are in the root [README](../README.md). This folder holds the guides that go further.

Agent/contributor rules: [AGENTS.md](../AGENTS.md) (SSOT). Doc ownership after
features: [rules/documents.md](../rules/documents.md).

## Start here

Pick the thing you want to do.

- **See how much of each subscription you have left** → [llmux Islands](llmux-islands.md) — install, add your first account, and read remaining usage from the macOS menu bar or notch. Privacy modes and recording are covered there too. The setup screens and launcher are on the preview channel.
- **Make models work together inside Claude Code** → [Multi-model agents in Claude Code](multi-model-agents.md) — Claude implements, GPT reviews, add Grok, and collect where they disagree. Each agent names a model id on its `model:` line, and llmux routes it.
- **Check what was actually sent to a model** → [The accidental AI debugger](ai-debugger.md) — a raw request/response viewer for every call: the four legs of the wire, copy as curl, email masking. Answers "did my field reach the model?"

## Reference

- [Operational reference](operational-reference.md) — commands, TUI keys, daemon/dashboard, scheduling, model routing, Codex frontend/backend, SDK error lifecycle, install variants.
- [Release availability](operational-reference.md#release-availability) — which features are on stable 0.2.24 vs the preview channel. The Islands setup screens and launcher are preview-only as of 2026-10-08.
- [Configuration](configuration.md) — config path, proxy/scheduler/routing keys, frontend/SDK runtime, Codex/Grok shaping, account types, email-anonymous mode.
- [Models](models.md) — catalog, aliases, `max_context`, group routing.
- [Schedulers](schedulers.md) — eligibility gates, `default` vs `round-robin`, adding a scheduler mode.
- [Fable scheduling](fable-scheduling.md) — how the Fable lane picks a subscription: gauges (poll + 7d_oi headers), gates, perishability ranking, manual pin, pause.
- [Provider compatibility](provider-compatibility.md) — what each backend group actually honors: forwarded vs dropped vs refused request fields, the Codex/Grok `max_tokens` receipts, diagnostic headers, known unknowns.
- [Codex frontend](codex-frontend/README.md) — `llmux run --codex`, Responses API, model picker, Agent SDK prerequisites and limits. [Activity origin](codex-frontend/activity.md) covers endpoint coloring.
- [Remote daemon](remote.md) — one central daemon, other computers connect to it as clients, and usage is attributed per computer. A bonus, not the default setup. Also covers the remote-mode command matrix and transport security.
- [Why llmux exists](why-llmux.md) — the harness-is-capital bet and the problems llmux removes.
- [What ships today](features.md) — the complete feature list, with dates on behavior changes.
- [FAQ](faq.md) — what is left per account, subagents on GPT or Grok, no cross-provider substitution, which features need preview, and the dated `gpt-5.5` context-window workaround.
- [System prompts (multi-model)](system-prompts/README.md) — **real captured system-prompt bodies** under [`system-prompts/samples/`](system-prompts/samples/) (CLI agent, 106k monitor, gpt SDK bot, compact, reviewer, auditor). Not a taxonomy essay.

## Design notes (not how-to)

- Responses compatibility: [spec](responses-compatibility/spec.md) and [trace](responses-compatibility/trace.md) — Codex/Grok image and tool translation, explicit semantic losses, strict policy, and count/termination validation. User-facing limits are in the [operational reference](operational-reference.md#codex--grok-compatibility-contract).

- [Grok provider STV notes](grok/) — `spec.md` / `trace.md` design artifacts for the grok backend; not a user guide.
- [OpenRouter provider STV notes](openrouter/) — `spec.md` design artifact for the openrouter backend (why it is a passthrough, the live upstream probes); not a user guide.
- Product/architecture decisions live in [`.prd/`](../.prd/):

  - [Product spec](../.prd/01-spec.md)
  - [Architecture](../.prd/02-architecture.md)
  - [Scheduler perishability](../.prd/09-scheduler-perishability.md)
  - [llmux Islands spec](../.prd/11-llmux-islands-spec.md)
  - [llmux Islands architecture](../.prd/12-llmux-islands-architecture.md)
