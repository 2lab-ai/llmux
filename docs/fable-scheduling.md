# Fable scheduling

How llmux picks the subscription that serves a **Fable-family request**
(`model` matching the `fable` family, vendor prefixes included — e.g.
`claude-fable-5`, `claude-fable-5[1m]`). Everything else is a *non-Fable*
request and uses the main lane unchanged.

Code: `src/scheduler/select.rs` (`pick_scoped`, `fable_score`,
`fable_ranked`, `gate_scoped`), `src/scheduler/mod.rs` (`fable_current`,
`manual_pin`, `lease_for_scoped`), `src/routing.rs` (`is_fable_model`).

## Why a separate lane

Anthropic subscriptions carry a model-scoped **Fable weekly bucket** on top
of the account-wide 5h/7d windows. An account can be Fable-exhausted while
perfectly serviceable for every other model, so Fable traffic keeps its own
sticky slot (`fable_current`) and its own gates — benching an account for
Fable never benches it for the rest.

## The Fable weekly gauge

Two feeds, freshest-wins, same slot:

1. **Usage poll** — `limits[]` rows with `kind == "weekly_scoped"`,
   labelled by the model display name ("Fable"). Carries upstream's own
   severity / is_active labels.
2. **Response headers** — `anthropic-ratelimit-unified-7d_oi-{utilization,reset}`
   ride every response through the account, so the gauge stays live between
   polls and a **cold gauge heals on the first request** through that
   account. (Severity is derived from utilization for display only.)

A window past its `resets_at` reads as utilization 0 (no constraint). A
never-observed bucket is **cold** — unknown, not zero and not exhausted.

## Selection, in gate order

1. **Hard gates** (always): auth-failed, operator **pause** (absolute —
   also refused at lease time), account-wide cooldown, and for Fable
   requests a **Fable-scoped cooldown** (set by a 429 on a Fable request)
   or a **preemptively constraining bucket** (utilization ≥ the
   `fable_weekly_max` ceiling, default 0.98 — severity labels never bench
   an account, only utilization does).
2. **Manual pin** (operator override, 5 minutes): a manual switch pins BOTH
   lanes to the chosen account. Thresholds, staleness and the preemptive
   Fable exclusion do **not** break the pin — "send it until it actually
   errors." A recorded 429 / auth failure clears it immediately; otherwise
   it lapses on its own.
3. **Perishability ranking** — `fable_score = servable_now × urgency`:
   - `servable_now = min(5h headroom, 7d headroom, Fable headroom)` — a
     Fable request burns all three budgets, the binding one wins.
   - `urgency = week / time-to-Fable-reset` (hyperbolic, clamped to 100×):
     the *required burn rate*. A **full bucket resetting in 3h (≈56×) is
     burned before a 93% bucket resetting in 1d14h (≈4.4×)** —
     use-it-or-lose-it. The main lane's linear ramp is deliberately NOT
     reused here: 38h vs 3h differ by only ×1.19 on it, inside the
     stickiness margin, which is exactly the live defect this replaced.
   - A **cold** bucket has no known perishability → urgency 1.0×. It is
     held by anchoring (below), never hunted.
4. **Anchoring + stickiness**: the lane anchors on `fable_current`, seeded
   from the **account-wide current** when the slot is empty — so with no
   clearly better candidate (score must exceed the anchor's by
   `SWITCH_MARGIN`, 25%) Fable traffic stays on the account the operator
   sees as current, cold bucket included, preserving prompt-cache locality.
   Round-robin mode keeps its absolute-stickiness contract.
5. **Healing**: the periodic evaluation tick re-evaluates the Fable slot
   every cycle (claude group), so a paused / hard-blocked fable current is
   abandoned within one tick — it used to stay pinned forever.

## 429 handling (scope-aware)

A 429 on a Fable request without `retry-after` parks the **Fable scope
only** (the account keeps serving non-Fable traffic); it escalates to an
account-wide park only when the 5h/7d windows themselves corroborate. A
`retry-after` 429 is an upstream mandate and always parks account-wide.
Any 429 on a manually pinned account also releases the pin.

## Operator levers

- **Manual switch** (TUI switcher / `POST /llmux/switch`) — 5-minute pin,
  both lanes, overrides thresholds.
- **Pause** (`p` / `POST /llmux/pause-account`) — absolute bench, both
  lanes, until resumed.
- **`llmux reset-usage`** (`POST /llmux/reset-usage`) — force every gauge
  back to cold after a provider-side quota reset; feeds repopulate from
  headers/polls.
- **Ceilings** — `scheduler.fable_weekly_max` (global) and per-account
  `account_limits` set where the preemptive exclusion engages.
