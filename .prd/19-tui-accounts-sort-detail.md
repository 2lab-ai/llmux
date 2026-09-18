# 19 — Accounts table: Fbl is Claude-only, group + name/next sort, click-to-open account detail

Status: shipped (PR #166 → main 850db8b → preview-2026-09-18-0609-850db8b80519; this describes the implemented contract, not a future target)
Date: 2026-09-18
Owner: Z (icedac@gmail.com)
SSOT: `.prd/tui-accounts-sort-detail/ssot.md` · loop: `.prd/tui-accounts-sort-detail/loop.md`

## Problem (live TUI, v0.2.23)

- `.prd/18` made the `5h` cell Claude-only (`-` elsewhere) but left the `7d Fbl`
  gauge on the old rule: every Codex/Grok row reads `○ cold` under `7d Fbl`
  forever — a Fable weekly scope exists only on Claude OAuth accounts.
- The table's row order is the intervention order (exhausted → auth-broken →
  known 5h usage desc → ready → paused → cold). Groups interleave, so a
  20-account pool reads as a shuffled list that reorders as usage moves; the
  owner wants provider blocks and a stable name order, with the scheduler's
  literal next-pick order available as the second mode.
- The detail pane shows ~8 lines for one account (the current, or the cursor
  row while a Mode interaction is open). There is no way to open everything the
  daemon knows about an arbitrary account.

## Decision

| # | Rule | Where |
| --- | --- | --- |
| 1 | `7d Fbl` renders only for `BackendGroup::Claude`. Every other group renders a dim `-`, never `cold`/`stale`/a glyph (same rule as `.prd/18` rule 1 for `5h`). A Claude row with no Fable scope keeps `○ cold`. | `fable_gauge_cell` takes `group` |
| 2 | Display order = `BackendGroup` order (Claude, Codex, Grok, OpenRouter) as the primary key, in both sort modes. | `DashboardView::display_order(sort, now)` |
| 3 | Sort mode `name` (default): within a group, account name ascending (case-insensitive), config index as tiebreak. | `triage.rs` |
| 4 | Sort mode `next`: within a group, the scheduler's own order — head = `pick_scoped(.., Some(group), .., NonFable)`'s literal decision (Stay → the group's current, Switch → its target, Exhausted → none), then the eligible tail in `ranked(.., heuristic_degraded)` order (round-robin: roster rotated from the head), then ineligible in config order. Uses the same per-group `headers_only` / `heuristic_degraded` flags and `gate_scoped` as `pick`, so the head can never disagree with the daemon (trinity R1 MUST-FIX). | `select::group_selection_order` |
| 5 | `o` toggles the mode on MAIN and the accounts overlay; session-local; pane title carries `sort name` / `sort next`; footer lists `o sort`. The intervention order is retired; the `!` urgency marker (`triage::urgent`) stays. | `App.account_sort`, `Chrome`, footer |
| 6 | Left-click on an accounts row opens an account detail modal pinned to the account id (display indexes reorder). Esc/q/Enter close; ↑↓/PgUp/PgDn/Home/End + wheel scroll; other keys/clicks are swallowed; the modal closes when the account leaves the snapshot. Right-click keeps the context menu. | `App.account_modal`, `draw_account_modal` |
| 7 | The modal prints every per-account datum the view holds (identity, order, status/gate, current/pin, in-flight, token, 5h/7d windows, scoped limits, cooldowns, limits + effective ceilings, lifetime totals, poll health, usage controls). Nothing is summarized away; unknown = `—`. | `draw_account_modal` |
| 8 | The modal's gate is the selector's own: `headers_only_mode` and `heuristic_degraded_mode` computed for the account's group, then `gate_scoped(.., RequestScope::NonFable)` — never the frame-wide `ctx.headers_only` or the degraded-blind `eligibility`. A `degraded` status row explains a dropped heuristic-cooldown gate; the cooldown section keeps the park visible (trinity R1/R2 MUST-FIX). Follow-up, not this doc: the table status column and detail pane still use the frame-wide flag. | `account_modal_lines` |

## Non-goals

- `/llmux/status` JSON, the islands app, scheduling, and the detail pane's
  existing content are untouched. The sort mode is not persisted to config.

## Release

Preview only (owner: "프리릴리즈까지 배포"): merge to main → preview.yml →
tap `llmux-preview` → `brew upgrade` + daemon restart on fable-m5max.
