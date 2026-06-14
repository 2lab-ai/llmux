# Scheduler redesign — research + decision (2026-06-14)

Consulted three independent strategists (OR/optimization, pragmatic systems, steelman of the
owner's "5-slice" idea). This records where they converged, the one design decision, and the
policy we ship. Baseline being replaced: `.prd/06-scheduler-current.md`.

## The owner's insight (restated)

The 7-day quota is not a single expiry deadline. The **5h window is a rate cap on how fast any
one account's weekly budget can be drained**. So weekly budget that cannot be drained before its
reset (too few 5h windows left) is *unsalvageable*, and "soonest-7d-reset-first" can be exactly
backwards — it chases the account whose leftover quota is *least* salvageable.

## Strategist convergence (3/3)

1. **The insight is correct.** Today's primary key (min 7d `resets_at`) ignores the 5h rate cap.
2. **Do NOT build pacing** ("reserve weekly budget across the week in 5 phased sub-budgets").
   For a single, bursty, use-it-or-lose-it consumer, pacing manufactures guaranteed waste to
   hedge an uncertain future burst. Greedy-now dominates. (All three rejected it independently.)
3. **Do NOT build a 5-slice allocator.** The "5" is `1/c` (weekly budget ÷ per-5h-window drain),
   an *accounting unit*, not a tunable and not a schedule. The load-bearing quantity is
   `W = windows_left = (t7d_reset − now) / 5h` and `realizable = min(r7, W·c)`.
4. **Keep the lexicographic, pure comparator.** Deterministic + unit-testable beats an opaque
   weighted score with unknowable weights. Keep eligibility gate, stickiness-as-default,
   exhaustion, and the headers-only fallback unchanged.
5. **"Two 10% > one 20%" is conditional**: true only when the fat account is flow-limited before
   its reset (`W·c < 0.20`); a tie otherwise. For a *sequential* single user the benefit is
   **continuity** (don't go Exhausted when one account's 5h window empties), not parallel burst.

## The one design decision

Strategists 1 & 3 proposed a continuous *value* (`min(r5,r7)·urgency` resp. `min(r7,W·c)`);
strategist 2 proposed reordering the existing lexicographic keys (5h-headroom primary,
7d-reset demoted) plus a binary urgent-expiry flag. **Decision: a single value function that
provably subsumes all three** — and needs no estimate of `c` (it uses `W` directly):

```text
r5            = max(0, five_hour_max − eff_5h_util)      # burst available right now
r7            = max(0, seven_day_max − eff_7d_util)      # weekly budget remaining
servable_now  = min(r5, r7)                              # work before next stall on EITHER limit
W             = max(1, (t7d_reset − now) / 5h)           # fresh 5h windows left before 7d reset
                 (∞ when no live 7d window → cold/expired)
urgency       = clamp(URGENCY_REF_WINDOWS / W, 1.0, URGENCY_MAX)   (1.0 when W = ∞)
score         = servable_now × urgency                   # higher = preferred
```

Why this is the synthesis:

- **Comfortable regime** (7d ample, `W` large → urgency 1): `score ≈ r5` ⇒ "most 5h headroom
  first" — strategist 2's primary key, and the account you can actually burst from now.
- **Near-expiry with salvageable, usable quota** (`W` small → urgency↑): the perishable account
  is burned first — use-it-or-lose-it, but only when it's both drainable *and* usable now
  (`servable_now` collapses to ~0 if the 5h window is already gated, so an urgent-but-unusable
  account does NOT win — strategist 1's laxity argument, strategist 2's guarded flag).
- **Never over-values unsalvageable 7d** — strategist 3's `min(r7, W·c)`; here the `W·c` cap is
  expressed through the urgency multiplier instead of a hard-to-estimate `c` constant.
- **"Two 10% > one 20%"** falls out: near expiry each thin account gets its own urgency boost and
  contributes its own salvageable slice; far from expiry both tie (correct).

### Knobs

No new config fields (the codebase minimizes config surface; scheduler stays a pure function).
Two documented constants in `select.rs`:

- `FIVE_HOUR_WINDOW = 5h` — session-window length, for counting `W`.
- `URGENCY_REF_WINDOWS = 3.0`, `URGENCY_MAX = 3.0` — below ~3 windows of weekly runway the
  perishability boost ramps in, capped at 3×.

`five_hour_max` / `seven_day_max` reuse the existing `SchedulerConfig` thresholds.

## What changes vs. baseline

- `rank()` ranks eligible accounts by **`account_score` descending** (was: min 7d `resets_at`).
  Tiebreaks: lower 5h util, then soonest 7d reset (demoted to deep tiebreak), then stable id.
  Codex-tier-last and the group filter are unchanged.
- `eligibility`, `pick` stickiness, `headers_only_mode`, `soonest_reset`, `next_in_line`,
  `selection_order`, and all blocking-reason output are **unchanged** — they reuse the new
  comparator, so the TUI/status can never disagree with the selector.

## Explicitly NOT done (with reason)

- **Pacing / weekly sub-budget reservation** — manufactures waste for a bursty single user.
- **5-slice allocator / per-slice accounting** — "5" is `1/c`, not a control structure.
- **Continuous multi-factor weighted score** — kept the value function single + monotone so the
  comparator stays transitive and unit-testable.
- **Gate hysteresis / min-dwell anti-thrash** — a real robustness win (strategist 2), but it
  touches the eligibility gate + adds mutable `last_switch_at` state; deferred as a follow-up so
  this change stays a contained pure-function edit. Stickiness already prevents steady-state
  flapping (the comparator only chooses a target when the current is already ineligible).
