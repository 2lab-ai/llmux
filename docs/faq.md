# FAQ

## `gpt-5.5` stops around 265k context. What should I do?

Use a Claude 1M-context model for compaction, then switch back to `gpt-5.5[1m]`.

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

The ~265k cutoff is empirical client behavior, not an llmux routing limit. The `[1m]` suffix improves Claude Code's context-window display, but it does not guarantee every un-compacted long transcript will be accepted unchanged.

This does not change your llmux account configuration. It is a Claude Code session-management workaround.

## Why does `[1m]` matter in `/model gpt-5.5[1m]`?

Claude Code derives its displayed context window from the model-name string. Bare `gpt-5.5` can be treated as an unknown or smaller-window model by the client. The `[1m]` suffix tells Claude Code to use a 1M context display while llmux still routes the request by the `gpt-` prefix.

See [operational-reference.md](operational-reference.md#context-window-display-for-gpt-55) for the routing details.

## Does `gpt-5.5[1m]` still route to Codex?

Yes. The `gpt-` prefix still matches the Codex group. llmux strips the display suffix for routing and usage attribution.

## Does llmux replace Claude Code?

No. llmux intentionally keeps Claude Code as the harness. It sits behind Claude Code as a local Anthropic-compatible proxy so the account/model layer can move while your harness stays fixed.

## Can I use llmux with only Claude accounts?

Yes. The durable core is multi-account Claude scheduling and Claude Code integration. Codex routing is optional.

## Is llmux for sharing accounts across a team?

No. llmux is for one human using their own accounts. It is not for credential pooling, resale, or shared subscription brokerage.
