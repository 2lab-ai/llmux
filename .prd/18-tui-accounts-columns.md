# 18 — Accounts table: 5h is a Claude-only stub, Fbl never gives way, status shrinks first

Status: shipped — v0.2.23 (PR #164 fcd78ce; stable v0.2.23 + preview-2026-09-18-0311-7ef84b1c5d56)
Date: 2026-09-18
Owner: Z (icedac@gmail.com)
SSOT: `.prd/tui-accounts-columns/ssot.md` · loop: `.prd/tui-accounts-columns/loop.md`

## Problem (as observed in the live TUI, preview-2026-09-17-0944)

The accounts table (`src/tui/ui.rs`, `draw_accounts_table`) gives the `5h`
window the same stretchy gauge column as `7d`, for every provider:

- Codex rows read `○ cold` under `5h` forever — Codex has no 5h window source
  at all (live `/llmux/status` 2026-09-18: all 4 codex accounts
  `five_hour: null`). Grok's cell does populate from its burst headers (see
  §Tension), but the owner's rule is that the 5h limit is a Claude-only
  concept: on every non-Claude row the cell is noise, not a gauge.
- The 5h gauge burns a full `bar_width + 1 + 5` column (17 minimum, up to 38)
  on the one window that carries the least scheduling signal.
- At a narrow width the layout sacrifices the *wrong* columns: the `7d Fbl`
  gauge degrades to a 7-cell `F 22h!` marker (`fable_gauge_cell`, narrow
  branch) and then disappears entirely (`ui.rs:4286-4310`), while `status`
  keeps a fixed 20 cells and `5h` keeps a full gauge.

## Decision

| # | Rule | Where |
| --- | --- | --- |
| 1 | `5h` is rendered only for `BackendGroup::Claude`. Every other group renders a dim `-` (n/a), never `cold`/`stale`/a glyph. | `window_gauge_cell` caller for the 5h slot |
| 2 | The `5h` column is a fixed compact cell, width 8, never stretched. Content: `68%`, `68%!` (over/parked), `◑ 68%` (stale), `! 68%` (poll-degraded), `○ cold` (no window yet). No countdown bar. | new `five_hour_cell` + `FIVE_H_COL_WIDTH` |
| 3 | The stretchy gauges are `7d` and (when the toggle is on) `7d Fbl` — in both the wide and the narrow set. The narrow compact `F …` marker is deleted. When the row does not fit, `Fbl` never gives way; the give-way order is status → 5h. | `bar_width` math, header/constraints, `fable_gauge_cell` |
| 4 | `status` is `Length(20)` only when the row fits at 20. Otherwise it shrinks first, down to a floor of 8, before any other column is touched. Only after status is at 8 and the row still overflows does the `5h` column drop. | `status_width` computed before the constraints |
| 5 | After status is at 8 and `5h` is gone, the next column to give way is `account` (20 → floor 7, the header word). Below that minimum the frame clips as before; that width is the documented minimum. Added after review R1 (astra): at 80 cols with a 20-char name, Fbl and rst on, the row still summed to 87 and ratatui shaved every column (`grou` header, live capture 2026-09-18). | `name_width` clamp after `show_five_h` |

Give-way order at decreasing width, in full: status 20→8 → (if the wide set no longer fits) req/tok drop and status re-expands, then shrinks again → `5h` drops → `account` 20→7 → clip. The req/tok step is pre-existing wide/narrow behaviour, not a new rule. `7d Fbl` never gives way.

Leftover width (after the fixed columns) is still poured into the stretchy
gauges' bars (7d, Fbl), capped at `GAUGE_BAR_MAX`.

## Non-goals

- The detail pane, the `/llmux/status` JSON, and the islands app are untouched:
  the 5h window is still recorded and served; only the table cell changes.
- No change to scheduling, ceilings (`5h,7d,fbl` limits editor), or the
  poller. `WindowDisplayState` keeps its labels for the detail pane.

## Tension (recorded, not resolved here)

Grok's 5h cell today reads a real `x-ratelimit-*` burst window (live: `0.0`);
`src/auth/grok_usage.rs:344` keeps that header path feeding the 5h gauge. The
owner's instruction is that codex and grok have no 5h limit, so the table
shows `-` for grok too; the window itself is still recorded and visible in the
detail pane.

## Release

This ships as a stable release (`v0.2.23`) followed by a preview build of the
same commit, per the owner's standing rule (zbrain `rules/DEV.md` §6).
