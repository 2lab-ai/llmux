# Schedulers

Both client frontends use the same account pool, leases and quota accounting.
The Claude Agent SDK additionally supplies typed account-restriction errors to
this lifecycle; see [SDK error mapping](provider-compatibility.md#claude-agent-sdk-account-errors).

## What you experience

- **Selection stays within a provider group.** A Claude model is served by one of your Claude accounts, a `gpt-*` model by one of your Codex accounts. llmux never substitutes a different provider when one is exhausted; crossing providers is your explicit choice (`/model`, or a per-agent `model:` — see [multi-model-agents.md](multi-model-agents.md)).
- **Quota is finite and perishable.** Each account has a 5-hour and a 7-day window that reset on fixed timestamps; quota left unspent at a reset is gone.
- **`default` spends perishable quota first.** It prefers an eligible account whose soon-to-reset quota would otherwise expire unused, but stays on the current account unless another scores clearly higher (>25%), so prompt caches survive. This is a scoring preference, not a promise to always use the soonest-expiring account.
- **`round-robin` never switches proactively.** Fewest switches and best cache locality, at the cost of letting other accounts' quota expire.
- **You can pin or pause an account manually** (`p` in the TUI switcher; context menu in llmux-islands). Paused accounts are never selected.
- **You can save next week's quota.** Save mode holds every account whose weekly quota would still be there after a time you choose, so only quota that would be lost by then is spent. See [Save mode](#save-mode).

## How selection works

Which account serves the next request is decided by the scheduler. Two algorithms ship; switch live with `S` in the TUI (persisted to `scheduler.mode`), or `POST /llmux/scheduler-mode {"mode": "default" | "round-robin"}`.

**Why switching matters:** the upstream prompt cache is scoped per account — every account switch invalidates it, and the next request re-reads the full conversation context uncached (token cost + latency). Both schedulers are therefore sticky on the current account; they differ in *when* they move and *who* is next.

## Eligibility (both modes)

An account can be picked only when ALL of these hold — the same pure gate drives selection, the TUI status column, and `/llmux/status`, so they can never disagree:

- auth healthy, not operator-**paused** (`p` in the TUI switcher, context menu in llmux-islands)
- not held by [save mode](#save-mode) (weekly reset after the save-mode cutoff)
- not cooling down (429 `retry-after` park)
- 5h utilization ≤ `scheduler.five_hour_max` (default **0.90**)
- 7d utilization ≤ `scheduler.seven_day_max` (default **0.99**)
- usage data fresh (≤ `usage_max_age_secs`; if ALL accounts are stale, the gate degrades to headers-only mode instead of stalling)

Per-account overrides: `account_limits` in the config (TUI: `L` in the switcher, `"90,98,98"` = 5h,7d,fbl percents) replace any of the three ceilings for that account.

**Fable scope:** Fable-family requests are additionally refused an account whose Fable weekly bucket is constraining (≥ `scheduler.fable_weekly_max`, default **0.98**, reset-aware) or Fable-cooling. Non-Fable traffic ignores Fable state entirely — a Fable-exhausted account still serves everything else. Full Fable lane mechanics: [fable-scheduling.md](fable-scheduling.md).

## `default` — quota-maximizing

Ranks eligible accounts by `score = servable_now × urgency`: `servable_now` = min(5h, 7d headroom below the ceilings) — the binding limit wins; `urgency` = 1–4× as the 7d reset approaches — soon-to-reset budget is perishable, so it burns first, while long-runway accounts are preserved as reservoirs. Sticky on the current account, but proactively switches when another account scores >25% higher (`SWITCH_MARGIN`) — it trades some cache locality for not letting quota expire unused.

## `round-robin` — sequential exhaust (fewest switches)

Stays on the current account until it is **hard ineligible** (ceiling hit, cooldown, auth, pause) — never a proactive switch — then moves to the **next account in roster order**, wrapping. Deterministic, score-free, and the minimum possible number of switches, at the cost of letting other accounts' soon-to-reset quota expire unused. Pick this when prompt-cache locality (long agent sessions) matters more than squeezing every window.

## Save mode

Save mode spends only the weekly quota that would be lost before a time you choose, and keeps the rest for after it. Example: on Friday you set the cutoff to Monday 15:00. Until then, an account whose 7-day window resets on Saturday serves; an account whose window resets on Tuesday is held, so its quota is still there when the week's work starts.

- **Turn it on or off:** `POST /llmux/save-mode {"until": <epoch seconds>}` turns it on, `{"until": null}` turns it off. The change applies at once and is saved to `scheduler.save_until`. The cutoff must be in the future and within 30 days. The soma-work dashboard has a top-bar switch for admins that calls this endpoint.
- **Which accounts it holds:** a subscription account (Claude, Codex, Grok, Gemini) whose 7-day window resets after the cutoff. A reset exactly at the cutoff serves. An account with no 7-day reading, or one whose window already reset, is treated as resetting a full week from now, because the next request opens a fresh window — so it is held unless the cutoff is more than a week away. Pay-as-you-go accounts (API keys, openrouter) have no weekly quota and are never held.
- **How strong the hold is:** the same as a pause. A held account is not selected, a manual switch to it is refused, and a manual pin on it ends. When every account that could serve is spent, requests get 429 until the cutoff — save mode does not fall back to held accounts.
- **When it ends:** at the cutoff, on its own. Held accounts serve again with no further step.
- **Where you see it:** a held account's status reads `saved · 7d resets in 3d4h`. `/llmux/status` carries `save_mode: {until, active, held}`. The TUI config tab shows `save mode` under scheduler.

## Adding a scheduler

The selection logic is pure and lives in `src/scheduler/select.rs` (`pick_scoped`, `rank`, `round_robin_next`) — deterministic functions of `(PoolSnapshot, SelectParams, now)`, unit-tested without IO. To add a mode: extend `SchedulerMode` in `src/config/schema.rs`, branch in `pick_scoped`/`next_in_line`/`selection_order`, and document it here.

The full derivation, edge cases, and the wasted-quota simulation behind the `default` policy live in [`.prd/09-scheduler-perishability.md`](../.prd/09-scheduler-perishability.md).
