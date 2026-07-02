# llmux docs

This directory holds user-facing guides that are too detailed for the repository front page.

Start at the root [README](../README.md) for the high-level product story and quick start. Use these docs when you need exact commands, config keys, backend behavior, or app-specific setup.

## Guides

- [Operational reference](operational-reference.md) — command table, TUI keys, daemon/dashboard behavior, install variants, scheduling policy, model routing details, Codex backend behavior, and development commands.
- [Configuration](configuration.md) — config file location, proxy settings, scheduler knobs, model routing options, Codex request shaping, account types, and email-anonymous mode.
- [FAQ](faq.md) — context-window workarounds and common usage questions, including the `gpt-5.5` → Claude 1M compact → `gpt-5.5[1m]` flow.
- [llmux Islands](llmux-islands.md) — native macOS menu-bar/notch app for glanceable account usage, floating island activity, demo capture, and email-anonymous mode.

## Source-of-truth design notes

Product and architecture decisions live in `.prd/`:

- [Product spec](../.prd/01-spec.md)
- [Architecture](../.prd/02-architecture.md)
- [Scheduler perishability model](../.prd/09-scheduler-perishability.md)
- [llmux Islands spec](../.prd/11-llmux-islands-spec.md)
- [llmux Islands architecture](../.prd/12-llmux-islands-architecture.md)
