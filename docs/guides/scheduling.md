# Scheduling accounts

Routing selects a provider group; scheduling selects one account inside that
group. Both shipped scheduler modes are sticky because switching accounts
invalidates provider-scoped prompt caches and makes the next request re-read
conversation context.

## Eligibility

An account must pass every relevant gate before selection:

- credential health is usable;
- the operator has not paused it;
- no active 429 cooldown blocks it;
- 5-hour utilization is at or below `scheduler.five_hour_max` (default `0.90`);
- 7-day utilization is at or below `scheduler.seven_day_max` (default `0.99`);
- usage evidence is fresh enough for providers that expose polled windows; and
- for Fable-family traffic, the Fable weekly bucket is below
  `scheduler.fable_weekly_max` (default `0.98`).

When every comparable account is stale, the scheduler degrades to available
header evidence instead of deadlocking the group. Per-account limits override
the global ceilings for that account.

## `default`: preserve capacity before it expires

The default mode ranks eligible accounts by:

```text
score = servable_now × urgency
```

`servable_now` is the tighter of the 5h and 7d headroom below configured
ceilings. `urgency` rises as the weekly reset approaches, capped at 4×. The
current account remains selected unless another eligible account's score is
more than 25% better.

Use this mode when consuming otherwise-expiring subscription quota is more
important than maximizing prompt-cache locality.

## `round-robin`: minimize account switches

Round-robin never proactively switches. It stays on the current account until
that account becomes hard-ineligible, then advances to the next account in
stable roster order and wraps.

Use this mode for long context-heavy sessions where avoiding cache invalidation
matters more than burning every soon-to-reset quota window.

## Change mode

Press `S` in the TUI, set the durable config, or call the control endpoint:

```json
{
  "scheduler": {
    "mode": "round-robin"
  }
}
```

```bash
curl -sS -X POST http://127.0.0.1:3456/llmux/scheduler-mode \
  -H 'content-type: application/json' \
  -d '{"mode":"default"}'
```

## Operator controls

- Pause/resume removes or restores an account without deleting credentials.
- Manual switch selects an eligible account but never moves existing leases.
- Per-account 5h/7d/Fable ceilings override the global values.
- A 429 uses explicit upstream retry timing when present. Without it, llmux
  applies the provider/scope-appropriate short heuristic, Fable cooldown, or
  Grok free-tier estimate; fresh capacity evidence can heal heuristic parks.
- Group settings are independent, so a Claude switch does not change Codex or
  Grok selection.

The [Configuration reference](../configuration.md) enumerates every key. For
the derivation and historical trade-offs, read the
[scheduler perishability decision](../../.prd/09-scheduler-perishability.md).
