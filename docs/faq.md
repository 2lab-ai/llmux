# FAQ

## I have several Claude subscriptions. How do I see what is left on each?

Run `llmux login` once per account. Islands, the macOS notch app, then shows each account's 5-hour and 7-day windows with their reset timers:

```bash
brew install --cask 2lab-ai/tap/llmux-islands
```

The TUI (`llmux dashboard`) shows the same windows. See [llmux-islands.md](llmux-islands.md).

## Can a Claude Code subagent run on GPT or Grok?

Yes. Set `model: gpt-6-astra[1m]` (or `model: grok-4.7`) in a `.claude/agents/<name>.md` file. When Claude Code runs under `llmux run`, that agent's requests are routed by the model id to your Codex or Grok account, and the result returns to the parent. Measured 2026-10-08.

Claude Code prints an `unrecognized_model` warning for non-Claude ids. The request is still sent.

See [multi-model-agents.md](multi-model-agents.md).

## Does llmux switch me to GPT when my Claude quota runs out?

No. The scheduler chooses among accounts in the same provider group: for a Claude model, that means your Claude accounts. Within that group it prefers quota that would otherwise expire unused, and it stays sticky to an account for prompt-cache locality.

Moving to another provider is always your explicit choice, through `/model` or an agent's `model:` line. See [schedulers.md](schedulers.md).

## Does llmux keep Claude Code and Codex settings, hooks or skills in sync?

No. Each client keeps its own configuration; llmux routes model traffic only. A shared cross-CLI configuration is an idea, not a shipped feature.

## Which features need the preview channel?

None as of 2026-10-08: stable v0.2.25 contains the Islands setup screens and launcher, authenticated local control between the app and the daemon, `llmux env --codex` and the Haiku 5.5 row. Stable 0.2.24 lacked all four; run `llmux update`. The rolling preview channel carries whatever landed on `main` after the last tag. See [operational-reference.md](operational-reference.md#release-availability).

## Is this a trinity / consensus feature?

llmux ships no review command. Three-model review is project configuration: two agent files (`gpt-reviewer`, `grok-reviewer`) plus a prompt that asks the parent Claude session, the third model, to collect the disagreements. See [multi-model-agents.md](multi-model-agents.md).

## Does llmux replace Claude Code?

No. `llmux run` launches Claude Code; `llmux run --codex` launches Codex CLI. llmux routes each client’s model traffic while that client retains its own tool execution, permissions and settings. It does not convert one client’s configuration into the other’s.

## Can I use llmux with only Claude accounts?

Yes, with either frontend. To use a Claude model from Codex, run `llmux run --codex -- exec -m haiku 'Explain this repository'`. Install Codex CLI on the client and Node.js 18+ plus npm on the daemon host; llmux installs the pinned Claude Agent SDK on first use. A ChatGPT account is needed for the Codex **backend**, not for choosing the Codex **client**. See [the frontend guide](codex-frontend/README.md).

## Why are some Activity rows dark gray?

Dark gray (RGB(40,40,40) in the TUI) means the request arrived through an OpenAI endpoint, even if Claude served it. Anthropic requests keep their existing background, including GPT requests from Claude Code. [Endpoint origin](codex-frontend/activity.md) is separate from backend/model.

## What happens when a Claude account rejects SDK access?

Known SDK organization, account-hold, verification and billing restrictions exclude that credential and try another eligible account before streaming. Authentication expiry still refreshes once; invalid requests do not disable accounts. See the [error mapping](provider-compatibility.md#claude-agent-sdk-account-errors).

## Is llmux for sharing accounts across a team?

No. llmux is for one human using their own accounts. It is not for credential pooling, resale, or shared subscription brokerage.

## `gpt-5.5` stops around 265k context. What should I do?

Observed 2026-08. Use a Claude 1M-context model for compaction, then switch back to `gpt-5.5[1m]`.

A practical sequence inside Claude Code:

```text
/model opus[1m]      # or /model sonnet[1m]
/compact
/model gpt-5.5[1m]
```

Why this helps:

- `gpt-5.5` routing is handled by llmux, but Claude Code still owns local context accounting and compaction behavior.
- In long sessions, Claude Code can block a `gpt-5.5` session around the mid-200k range, even when `gpt-5.5[1m]` is selected.
- Switching temporarily to a Claude model with a 1M context window gives Claude Code a known large-window model for `/compact`.
- After compaction reduces the active transcript, switching back to `gpt-5.5[1m]` continues routing through llmux to the Codex group.

The ~265k cutoff is empirical client behavior, not an llmux routing limit. `gpt-5.5` is a 272k model, and the `[1m]` suffix is display-only for it. The `[1m]` suffix improves Claude Code's context-window display, but it does not guarantee every un-compacted long transcript will be accepted unchanged.

This does not change your llmux account configuration. It is a Claude Code session-management workaround.

## Why does `[1m]` matter in `/model gpt-5.5[1m]`?

Claude Code derives its displayed context window from the model-name string. Bare `gpt-5.5` can be treated as an unknown or smaller-window model by the client. The `[1m]` suffix tells Claude Code to use a 1M context display while llmux still routes the request by the `gpt-` prefix.

llmux strips one trailing `[1m]` before resolving the codex model, so the suffix never reaches upstream and works on any codex id or alias (`gpt-5.6-sol[1m]`, `sol[1m]`). `gpt-5.6-sol[1m]` and `gpt-5.6-terra[1m]` are also catalog rows advertising a 1000000 window — probed 2026-08-21 at 910,229 input tokens accepted / ~936k rejected against the ChatGPT-account backend (OpenAI publishes 1,050,000 total for the gpt-5.6 family). `gpt-5.5` is a 272k model, so `gpt-5.5[1m]` remains a display-only workaround.

See [operational-reference.md](operational-reference.md#context-window-display-for-codex-models) for the routing details, and [models.md](models.md#the-codex-1m-rows) for the catalog rows.

## Does `gpt-5.5[1m]` still route to Codex?

Yes. The `gpt-` prefix still matches the Codex group. llmux strips the display suffix for routing and usage attribution.
