# PRD/decision record — calendar usage & cost tab (2026-07-15)

Status: **shipped** — PR [#108](https://github.com/2lab-ai/llmux/pull/108) (feature) +
PR [#111](https://github.com/2lab-ai/llmux/pull/111) (cost-column ledger polish),
preview `2026-07-15-0502-abd68fb671b5`. Operator how-to lives in
[docs/operational-reference.md](../docs/operational-reference.md) ("Usage tab");
this file records the design decisions and their reasoning.

## Problem

The dashboard answered "which model is consuming quota" cumulatively
(`.prd/10-model-usage-dashboard.md`) and over trailing 24h/72h windows, but not
the operator's billing-shaped question: **what did each model cost per hour /
day / month, read like a ledger** (the Anthropic Console usage page, in the
TUI). The data already existed — `activity.jsonl` keeps a best-effort record
of finished requests (ts, group, model, four token classes) with no retention
limit; the append runs on the CONSUMER side of the lossy `try_send` activity
channel (`DashboardHub::apply_event`), so a dropped event is absent from both
the live fold and the file.

## Decisions

1. **Calendar buckets are a fold over the SAME event stream** (`ActivityLog`,
   the dashboard consumer) — not a new metering path. Startup replay passes
   each record's persisted timestamp, so history lands on its original
   buckets; replay == live is pinned by store-equality tests. Consequence
   (accepted): the fold sits behind the lossy `try_send` activity channel,
   like every other dashboard stat — it is an estimate surface, not a ledger
   of record (`DataQualityDoc` wording applies).
2. **Three granularities, three retention policies**: epoch-hour (UTC keys,
   trailing 72h), local-civil-day (180d), local-civil-month (unbounded — 12
   keys/year). Arbitrary-past hourly drill-down is deferred to the paged
   store (#107).
3. **Timezone: day/month buckets follow the daemon's local calendar**, keyed
   with the offset in force at each event's timestamp (`local_offset_secs`,
   DST-correct per event). Hour buckets stay UTC-keyed and are labeled in
   local wall clock. Known tension: the older tokens-per-day chart (UI-3 U14)
   still buckets days in UTC — unification queued in #110.
4. **Retention is watermark-anchored, serving is read-anchored.** Fold-time
   pruning follows monotonic high-water marks CLAMPED to the wall clock
   (an out-of-order replayed event can't resurrect an expired bucket; a
   future-dated/corrupt record can't drag the window forward and wipe real
   data), and `usage_stats(now)` re-anchors the served hourly/daily windows
   to read time with an upper bound (an idle daemon shows an honest trailing
   window; future-dated garbage never renders). Bucket time ≠ retention time
   ≠ serve time — three separate quantities on purpose.
5. **The document carries everything the client renders** (additive
   `DashboardDoc.usage_stats`): server-rendered calendar labels (the daemon's
   civil calendar is the SSOT for what "a day" means), server-priced
   `cost_usd` (config `pricing` overrides live daemon-side), and a `priced`
   flag so "no rate known" can never render as a credible $0. Old daemon ↔
   new client degrades to an empty tab; new daemon ↔ old client ignores the
   field.
6. **Row identity = the NORMALIZED served model per group**, matching
   `record_model` — a raw wire variant (`…[1m]`) must not split a row the
   model table merges.
7. **Cost cells render ledger-style** (#111): decimal point anchored to one
   column (integer right-aligned width 7, thousands separators — aligned up
   to $999,999/bucket), integer digits over a dimmer fraction, `$` quietest;
   per-model detail rows a tier below the bold bucket totals. Amount
   validity is checked **per component before aggregation**
   (`usage_cost_valid`: priced ∧ finite ∧ ≥0) — an invalid amount renders
   the honesty dash and qualifies every containing total (`+?`,
   `(+unpriced)`) instead of silently reducing it.

## Review history (what the gates caught)

- gpt-5.6 dual-persona R1 → retention watermark missing, scroll-offset
  overscroll debt, unpriced-$0 dishonesty (all fixed in-PR).
- trinity (grok-4.5 / gpt-5.6-sol / fable) R1 2:1 → gpt-5.6 escalated
  future-timestamp watermark poisoning from "nice-to-have" to blocker
  (permanent hourly/daily wipe from ONE corrupt record); wall-clock clamp +
  read-side upper bound + poison-reproducing test; R2 unanimous.
- 8-angle code review → normalized row identity, granularity-aware empty
  hint, wheel scroll, `priced_cost` single lookup, `window_floor` /
  `local_civil_day` dedupe, visible-area render bound.
- #111 3-round → negative-rate components must not net into totals
  (per-component validation); style tiers pinned at the buffer level.

## Deferred (tracked in #110)

Per-frame rebuild cost + wire gating (usage rows ride every doc build/poll),
U14 chart UTC/local unification, single-bucket-taller-than-terminal scroll,
past-garbage month rows, hour-label idiom home (#98 family), Islands panel
(doc rows already served).
