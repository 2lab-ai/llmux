# llmux docs

User-facing guides that are too detailed for the repository front page.

Start at the root [README](../README.md) for the product story and quick start.
Agent/contributor rules: [AGENTS.md](../AGENTS.md) (SSOT). Doc ownership after
features: [rules/documents.md](../rules/documents.md).

## Guides

- [Fable scheduling](fable-scheduling.md) — how the Fable lane picks a subscription: gauges (poll + 7d_oi headers), gates, perishability ranking, manual pin, pause.
- [Operational reference](operational-reference.md) — commands, TUI keys, daemon/dashboard, scheduling, model routing, Codex backend, install variants.
- [Configuration](configuration.md) — config path, proxy/scheduler/routing keys, Codex shaping, account types, email-anonymous mode.
- [Models](models.md) — catalog, aliases, `max_context`, group routing.
- [FAQ](faq.md) — context-window workarounds (including `gpt-*` → Claude 1M `/compact` → back).
- [llmux Islands](llmux-islands.md) — macOS menu-bar/notch companion, privacy modes, recording.
- [System prompts (multi-model)](system-prompts/README.md) — **real captured system-prompt bodies** under [`system-prompts/samples/`](system-prompts/samples/) (CLI agent, 106k monitor, gpt SDK bot, compact, reviewer, auditor). Not a taxonomy essay.

## Design notes (not how-to)

- [Grok provider STV notes](grok/) — `spec.md` / `trace.md` design artifacts for the grok backend; not a user guide.
- Product/architecture decisions live in [`.prd/`](../.prd/):

  - [Product spec](../.prd/01-spec.md)
  - [Architecture](../.prd/02-architecture.md)
  - [Scheduler perishability](../.prd/09-scheduler-perishability.md)
  - [llmux Islands spec](../.prd/11-llmux-islands-spec.md)
  - [llmux Islands architecture](../.prd/12-llmux-islands-architecture.md)
