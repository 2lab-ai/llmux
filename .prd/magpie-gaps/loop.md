# magpie-gaps — loop

Status: not started (2026-10-20). Only open work lives here; closed rounds move to `loop.done.md`.

## Next starting point

Round 1 = roadmap item 1, "Any API key behind Claude Code and Codex". Start with the
Anthropic-compatible passthrough slice (DeepSeek preset). The OpenAI Chat Completions translator is
the second slice.

## Open work units

| WU | Item | Branch | Worktree | Scope | State |
|---|---|---|---|---|---|
| 1a | API key — Anthropic-compatible passthrough + presets | `feat/api-key-anthropic-compat` | `.worktrees/feat-api-key-anthropic-compat` | `src/provider/`, login CLI, catalog, `01-spec.md:59` amendment | open |
| 1b | API key — OpenAI Chat Completions translator | `feat/api-key-openai-chat` | `.worktrees/feat-api-key-openai-chat` | `src/provider/`, translator tests | open (after 1a) |
| 2a | Quota display — remaining/used toggle | `feat/quota-remaining-toggle` | `.worktrees/feat-quota-remaining-toggle` | `src/tui/`, `llmux-islands/` | open |
| D1 | Positioning + comparison table | `docs/positioning` | `.worktrees/docs-positioning` | `README.md`, `docs/why-llmux.md` | open |

## Build facts

Gate commands are measured when round 1 starts (record the exact `cargo` / Islands commands and
their pass output here).

## Open gaps

None yet. Acceptance scenarios per item: [24-magpie-gap-roadmap.md](../24-magpie-gap-roadmap.md).
