# magpie-gaps — loop

Status: not started (2026-10-20). Only open work lives here; closed rounds move to `loop.done.md`.

## Next starting point

Round 1 = wave 1 of [24-magpie-gap-roadmap.md](../24-magpie-gap-roadmap.md): items 1–3 plus D1–D3.
Start with item 2's first slice, generalizing the OpenRouter passthrough into a DeepSeek preset,
because items 3 and D2 can demo it. Live-probe the vendor's auth header and model ids first.

## Open work units

| WU | Item | Branch | Worktree | Scope | State |
|---|---|---|---|---|---|
| 2a | Direct vendor keys: generalize the OpenRouter passthrough + DeepSeek preset | `feat/vendor-key-presets` | `.worktrees/feat-vendor-key-presets` | `src/provider/openrouter.rs` (or a sibling), login CLI, catalog | open |
| 1a | Quota display: remaining/used toggle | `feat/quota-remaining-toggle` | `.worktrees/feat-quota-remaining-toggle` | `src/tui/`, `llmux-islands/` | open |
| 3a | Fallback chain to OpenRouter, opt-in, with header + badge | `feat/group-fallback-chain` | `.worktrees/feat-group-fallback-chain` | routing/scheduler, Islands badge | open |
| D1 | Positioning + comparison table | `docs/positioning` | `.worktrees/docs-positioning` | `README.md`, `docs/why-llmux.md` | open |

## Build facts

Gate commands are measured when round 1 starts (record the exact `cargo` / Islands commands and
their pass output here).

## Open gaps

None yet. Acceptance scenarios per item: [24-magpie-gap-roadmap.md](../24-magpie-gap-roadmap.md).
