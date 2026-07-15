# How llmux works

llmux separates the agent harness from the account and model that execute a
request. The stable side is Claude Code and everything around it. The variable
side is the provider, model, credential, and quota window.

## The mental model

Think of llmux as one local traffic controller with four responsibilities:

1. **Accept** Anthropic Messages traffic from Claude Code.
2. **Route** the model name to a Claude, Codex, or Grok group.
3. **Schedule** one healthy account inside that group.
4. **Adapt** the request and stream when the selected upstream is not
   Anthropic-shaped.

The terminal UI and llmux Islands are control surfaces around that traffic
controller. They read the daemon's dashboard document and dispatch typed
operations; they are not separate schedulers and do not read provider
credentials directly.

![llmux architecture overview](assets/architecture-overview.png)

[Open the full-size diagram](assets/architecture-overview.html) or
continue into the [runtime architecture](architecture.md).

## Harness and model boundary

Claude Code remains responsible for conversation state, tools, permissions,
context compaction, hooks, and repository instructions. llmux does not replace
that harness. It changes what happens after Claude Code sends an HTTP request.

The integration contract is deliberately small:

```text
ANTHROPIC_BASE_URL=http://127.0.0.1:3456
```

This is why changing from a Claude model to a Codex or Grok model does not
require moving your work into another vendor CLI.

## Router versus scheduler

These are different decisions:

- The **router** answers “which provider group can serve this model name?”
- The **scheduler** answers “which account in that group should serve now?”

Default routing recognizes three disjoint families:

| Model signal | Group | Credential examples |
| --- | --- | --- |
| `claude-*`, `fable`, `opus`, `sonnet`, `haiku` | Claude | subscription OAuth, Anthropic API key |
| `gpt-*`, `codex`, `sol`, `terra`, `luna` | Codex | ChatGPT/Codex OAuth |
| `grok`, `grok-*` | Grok | xAI device-code OAuth |

Each group keeps a sticky current account; Fable-family traffic also has a
separate Claude-scoped current slot so it does not displace ordinary Claude
work. Switching the Claude account does not silently change the Codex account,
and a missing group returns a clear error by default rather than borrowing an
incompatible credential. Routing can be customized or disabled for legacy
behavior.

## Quota is perishable

A quota window has an amount and a reset time. Capacity left when the reset
arrives disappears, so equal remaining percentages are not equally valuable.
The default scheduler therefore considers both headroom and how soon the
weekly budget expires.

It remains sticky to preserve upstream prompt-cache locality, but may move when
another account has materially more perishable capacity. The alternative
`round-robin` mode stays on the current account until it becomes ineligible,
then advances in roster order. See [Scheduling](guides/scheduling.md) for the
exact gates and trade-offs.

## One daemon, many clients

The daemon owns the runtime state:

- account health, token refresh, quota windows, cooldowns, and leases;
- per-group current accounts and scheduler mode;
- activity, token, cost, and request-receipt projections;
- the control API used by CLI/TUI and Islands; and
- provider request translation and streaming.

`llmux run`, `llmux dashboard`, and Islands are clients of that daemon. Local
clients use loopback. Remote clients may target one central daemon, but the
wire transport remains HTTP and must be protected by an encrypted overlay.

## Data plane and control plane

| Plane | Examples | Rule |
| --- | --- | --- |
| Data | prompts, tool calls, SSE output, provider auth injection | pinned to one account for the request lifetime; off-loopback access also requires the daemon API key |
| Control | status, dashboard, switch, pause, settings, login orchestration | loopback-trusted; off-loopback requires the same daemon API key |
| Presentation | TUI, macOS SwiftUI/AppKit, KDE Qt/Kirigami | renders daemon-derived state; privacy masking happens before or at presentation boundaries |
| Evidence | activity ledger, usage aggregation, raw-I/O/trace files when enabled | operational data with explicit retention and privacy implications |

## Persistence and secrets

The durable config lives at `~/.config/llmux.json` by default and is written
atomically with mode `0600`. It contains account credentials, so do not commit
or share it. Islands keeps only its own connection and presentation settings;
provider credentials remain daemon-owned.

Raw request/response capture is enabled by the current config default. Treat
its state file as sensitive and review `raw_io` retention settings before using
llmux with confidential prompts. Public screenshots should use email-anonymous
or demo mode.

## Product boundary

llmux is intentionally a local, single-human system. It does not provide
multi-tenant isolation, credential brokerage, TLS termination, or a hosted
control plane. Its durable value is preserving one harness while making the
model/account boundary operationally flexible.
