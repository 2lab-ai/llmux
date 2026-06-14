# Scheduler — current behavior (as-built, 2026-06-14)

Precise description of what `src/scheduler/select.rs` + `mod.rs` actually do today,
written before the redesign so the strategist brief and the new design have a fixed
baseline to argue against.

## Data model

Per account the pool tracks two **quota windows** (`window.rs::QuotaWindow`):

- `five_hour` — the rolling 5-hour session window.
- `seven_day` — the rolling 7-day weekly window.

Each window = `{ utilization: 0..1, resets_at, fetched_at, source }`.

- `effective_utilization(now)` = `0.0` if `resets_at <= now` (expired → carries no
  constraint), else `utilization`.
- `is_stale(now, max_age)` = `fetched_at` older than `max_age`.

Evidence sources, freshest `fetched_at` wins per window:

- **Headers** (`anthropic-ratelimit-unified-{5h,7d}-utilization`, 0..1 fractions) —
  observed live during traffic.
- **Usage poll** (`GET /api/oauth/usage`, percentages 0..100 ÷100) — every 300s for
  oauth accounts, covers idle accounts. (Codex has no poller; its `x-codex-*` headers
  are its only window source.)

## Selection inputs

`SelectParams` derived from `SchedulerConfig`:

| field | default | meaning |
|---|---|---|
| `five_hour_max` | 0.90 | 5h utilization ceiling |
| `seven_day_max` | 0.99 | 7d utilization ceiling |
| `usage_max_age` | 600s | older usage → ineligible (unless all-stale fallback) |

Plus `now`, and an optional `BackendGroup` filter (claude vs codex) when `routing.enabled`.

## Eligibility gate — `eligibility()` (first failing reason wins)

1. `!healthy` → **AuthUnhealthy**
2. `cooldown_until > now` → **CoolingDown** (set by 429: `RetryAfter` exact, or `Heuristic` 60m)
3. `five_hour.effective_utilization(now) > 0.90` → **FiveHourOverThreshold**
4. `seven_day.effective_utilization(now) > 0.99` → **SevenDayOverThreshold**
5. not headers-only AND not codex AND usage stale > 600s → **UsageStale**

A **missing** window = utilization 0 (cold account, immediately eligible). Boundary is
`<=` eligible / strictly `>` gated.

**Headers-only fallback** (`headers_only_mode`): if NO account passes the full gate but
≥1 fails ONLY on staleness, the staleness gate is dropped pool-wide (per group). Rationale:
serving a maybe-stale account beats refusing service; a real 429 is the corrective backstop.

## Selection — `pick()`

1. Compute `headers_only`.
2. `eligible` = accounts in-group that pass the gate.
3. **Stickiness**: if the group's `current` is in `eligible` → `Stay` (even if outranked).
4. Else `Switch { to: min_by(rank) }`.
5. If `eligible` empty → `Exhausted { retry_after: soonest_reset }`.

### `rank()` comparator (ascending = preferred first)

1. **(legacy/routing-off only) codex tier last** — codex is the cross-group overflow pool,
   never auto-picked over a healthy anthropic account.
2. **min `seven_day.resets_at`** — "use-it-or-lose-it": burn the account whose weekly quota
   evaporates soonest. Accounts with no live 7d window rank AFTER those with a known reset.
3. **min `five_hour.effective_utilization`** — most 5h headroom.
4. **stable id**.

`next_in_line()` and `selection_order()` reuse the same gate + comparator so the TUI/status
display can never disagree with the selector.

## Background loops (`proxy/server.rs`)

- **Usage poller** — 300s cadence + jitter, oauth only, backoff ladder 2m→5m→10m→15m on failure.
- **Token refresh** — refreshes every healthy oauth/codex account when `< refresh_ahead_secs`
  (25200s = 7h) to expiry, with zero client traffic.
- **Scheduler re-evaluation tick** — re-runs `pick` periodically so threshold crossings move
  the current slot even between requests.

## The governing philosophy today

- **Stickiness first**: never move off an eligible current. (Implicitly protects upstream
  prompt-cache locality — every switch lands on a different account = a cache miss.)
- **When forced to move**: soonest-7d-reset-first, i.e. minimize *weekly-quota expiry waste*.
- **5h is only a gate + a tiebreak**, never a first-class part of the value function.

## What the redesign must reconsider (user's critique)

> Treating the 7-day quota as a single expiry deadline is wrong. The 5h window caps the
> *rate* at which any single account's weekly quota can be drained. So:
>
> - A 7d budget that cannot physically be drained before its reset (too few 5h windows left
>   × the 5h cap) is partly **unsalvageable** — "100% of 7d left but only 5h on the clock"
>   can only ever yield one more 5h window's worth.
> - Therefore **two accounts with 10% of 7d each are worth more than one account with 20%**
>   (same expiry): each has its own independent 5h drain rate, so the pair has 2× burst.
> - Proposed direction: stop treating 7d as one bucket; **slice the 7d budget (~5 slices)**
>   and optimize against the 5h-rate constraint rather than the single 7d deadline.

The open design questions this raises — greedy-now vs paced-across-the-week, how to fold the
5h rate cap into the value function, whether "5 slices" is pacing or just the unit of
accounting — are the subject of the strategist consultation in `07-scheduler-research.md`.
