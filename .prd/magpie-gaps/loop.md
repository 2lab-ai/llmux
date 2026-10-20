# magpie-gaps — loop

Status: not started (2026-10-20). Only open work lives here; closed rounds move to `loop.done.md`.

## Next starting point

Round 1 = wave 1 of [24-magpie-gap-roadmap.md](../24-magpie-gap-roadmap.md): items 1–3 plus D1–D3.
Item 1 (dashboard UI) is the owner's priority: start with its accounts & quota page on the daemon's
existing `GET /llmux/dashboard` document. In parallel, item 2's first slice generalizes the
OpenRouter passthrough into a DeepSeek preset; it is the highest-scoring slice that is still missing. Items 3 and D2 do not wait for it; both can
use the existing OpenRouter backend. Live-probe the vendor's auth header and model ids first.

## Open work units

| WU | Item | Branch | Worktree | Scope | State |
|---|---|---|---|---|---|
| 2a | Direct vendor keys: generalize the OpenRouter passthrough + DeepSeek preset | `feat/vendor-key-presets` | `.worktrees/feat-vendor-key-presets` | `src/provider/openrouter.rs` (or a sibling), login CLI, catalog | open |
| 1a | Dashboard UI: web shell + accounts & quota page (remaining/used toggle) + `01-spec.md:50-53` amendment | `feat/web-dashboard` | `.worktrees/feat-web-dashboard` | daemon HTTP (`/llmux/dashboard`), new web assets, `.prd/01-spec.md` | open |
| 3a | Fallback chain to OpenRouter, opt-in, with header + badge | `feat/group-fallback-chain` | `.worktrees/feat-group-fallback-chain` | routing/scheduler, Islands badge | open |
| D1 | Positioning + comparison table | `docs/positioning` | `.worktrees/docs-positioning` | `README.md`, `docs/why-llmux.md` | open |
| D2 | README hero GIF (Islands + switch to an `or-` model), badges, KO/ZH READMEs | `docs/readme-demo` | `.worktrees/docs-readme-demo` | `README.md`, `screenshots/` | open (after D1) |
| D3 | Landing page + 30 s video | — | — | dosi.dev page | open (after D2) |

## Build facts

Gate commands are measured when round 1 starts (record the exact `cargo` / Islands commands and
their pass output here).

## Open gaps

None yet. Acceptance scenarios per item: [24-magpie-gap-roadmap.md](../24-magpie-gap-roadmap.md).
