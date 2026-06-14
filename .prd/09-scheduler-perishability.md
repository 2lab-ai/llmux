# Scheduler — perishability-first selection (2026-06-14)

Supersedes the parameter choices and the absolute-stickiness rule of
`.prd/07-scheduler-research.md`. The value-function *shape* (`servable_now ×
urgency`, pure lexicographic comparator) is kept; two things were wrong in
practice and are fixed here.

## The bug the owner hit (live fleet, 8 accounts)

Six idle Claude accounts, all far under their limits, 7d resets spread 1.3d–6.4d:

| account | 5h | 7d | 7d reset | OLD score | NEW score |
|---|---|---|---|---|---|
| ai2 (current) | 17% | 4% | 4d5h | 0.73 | 1.61 |
| dev1 | 3% | 2% | 5d16h | **0.87 (picked)** | 1.37 |
| info | 4% | 4% | 6d3h | 0.86 | 1.18 |
| notify | 5% | 3% | **1d14h** | 0.85 | 2.82 |
| ai3 | 6% | 6% | **1d6h** | 0.84 | **2.91 (picked)** |
| ai | 17% | 4% | 6d10h | 0.73 | 0.91 |

The scheduler picked **dev1** as `next` and **stayed on ai2** — both of which
have the *most* weekly runway — while **notify/ai3**, whose barely-touched
weekly budget resets within ~1 day, sat unused and were about to evaporate.
That is use-it-or-lose-it exactly backwards.

Two independent root causes:

1. **Urgency was dormant past 15h.** `urgency = clamp(3 / W, 1, 3)` with
   `W = windows_left = time_to_reset / 5h` only rises above 1.0 when `W < 3`,
   i.e. the 7d reset is **< 15 hours** away. A reset "1 day out" (W ≈ 6) got
   **zero** perishability boost, so every idle account collapsed to
   `score ≈ min(r5, r7)` and the most-5h-headroom account (dev1) won.
2. **Stickiness was absolute.** `pick` returned `Stay` whenever the current
   account was eligible, regardless of how much more perishable another account
   was — so it camped ai2 (4d runway) and never moved to burn ai3/notify.

## Fix 1 — urgency ramps across the whole weekly window

```text
r5           = max(0, five_hour_max − eff_5h_util)
r7           = max(0, seven_day_max − eff_7d_util)
servable_now = min(r5, r7)
frac_left    = clamp(time_to_7d_reset / SEVEN_DAY_PERIOD, 0, 1)   # 1=just reset, 0=resets now
urgency      = 1 + (URGENCY_MAX − 1) × (1 − frac_left)            # 1.0 if no live 7d window
score        = servable_now × urgency
```

- `SEVEN_DAY_PERIOD = 7d`, `URGENCY_MAX = 4.0`.
- Linear, not hyperbolic: smooth, no blow-up near reset, no arbitrary 15h
  cliff. Perishability is now proportional to **how far through its weekly
  window** an account is — a reset 1 day out (frac_left ≈ 0.18) gets ≈3.5×, a
  reset 6 days out (frac_left ≈ 0.86) gets ≈1.4×, a just-reset/cold account 1.0×.
- `servable_now` still gates the boost: a 5h-maxed or weekly-drained account has
  `servable_now ≈ 0`, so urgency cannot rescue an urgent-but-**unusable**
  account (it would gate immediately). "Two thin accounts beat one fat one near
  expiry" and "don't chase unsalvageable quota" both still fall out.
- A cold / just-reset account is **least** perishable → ranks last among
  usable accounts (preserve the long-runway reservoir for future bursts).

## Fix 2 — stickiness yields to clearly-more-perishable quota

`pick` stays on an eligible current **unless** some account's score exceeds the
current's by `SWITCH_MARGIN = 0.25` (25%):

```text
stay  ⇔  current eligible AND best.score ≤ current.score × (1 + SWITCH_MARGIN)
```

- Switches off ai2 (0.73 → 1.61) to ai3 (2.91) immediately — ai3 is ~1.8× ⇒ over
  the margin.
- The margin ignores pure tiebreak wins (equal score, lower id) and small score
  noise, so it does **not** flap between near-equal accounts.
- Proactive switches are driven only by the **60s `EVALUATE_TICK`** (the
  per-request path still switches only on the current going *ineligible*), so
  the tick cadence is itself a ≥60s rate-limit on hand-offs. A switch off an
  *ineligible* current (gated / cooldown / auth-fail) is always immediate.
- Stays a **pure function** of `(snapshot, params, now)` — no mutable
  `last_switch_at` state. A time-based min-dwell remains an optional future
  hardening (as in 07) if simulation ever shows thrash.

## Why this is optimal for a single bursty consumer

Spending account *i*'s quota now is "free" to the extent *i*'s 7d window resets
soon: that budget refreshes regardless, so it would otherwise be lost. Spending
a long-runway account instead depletes capacity that will **not** refresh before
a possible future burst. So, among usable accounts, **burn soonest-reset first
and preserve the long-runway reservoir** — which is what `score` now ranks.

## Verification (in `select.rs` tests)

- **Scenario tests** — hand-computed expected pick for each edge case
  (perishable-beats-cold, gated-urgent-not-chased, 5h-rate-cap, ties).
- **Property/invariant tests** — comparator is a total order; a gated account
  never outranks a usable one; more-perishable never ranks below equally-usable
  less-perishable; `pick`/`next_in_line`/`selection_order` agree.
- **Burn-down sequence** — the live fleet above, drained step by step, must
  visit accounts in soonest-reset-among-usable order (ai3 → notify → …), never a
  long-runway account while a more-perishable usable one exists.
- **Wasted-quota simulation** — a constructed staggered-reset scenario where the
  OLD policy lets a soon-resetting account's budget evaporate (load-balancing)
  and drops demand later, while the NEW policy harvests it and serves all demand.
