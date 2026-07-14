# Raw usage data sources — the two carriers (2026-07-03)

Status: **evidence captured, verbatim**. This doc records what actually arrives on each of the two
usage-data paths llmux has, what llmux currently parses vs drops, and the implications for
per-model (Fable) usage display and model-scoped cooldown. Evidence collected live on 2026-07-03
against api.anthropic.com with real account tokens from `~/.config/llmux.json`.

High-level: llmux receives usage two ways — **(1) polled: `GET /api/oauth/usage`** per account, and
**(2) attached to model turn-end responses** (rate-limit headers + body `usage` token counts).
As of 2026-07-03, the **Fable-specific weekly bucket exists ONLY on carrier (1)**, inside a
`limits[]` array that llmux currently ignores. Carrier (2) is aggregate-only, and a Fable-limit
429 carries **no scope information at all** — which is why a Fable exhaustion currently looks
identical to whole-account exhaustion and triggers a whole-account cooldown.

## Carrier 1 — OAuth usage poll: `GET {base}/api/oauth/usage`

Requested by `UsagePoller` at `src/scheduler/usage.rs:98-104` (Bearer + `Accept: application/json`).

### What llmux parses today

`usage.rs:60-62` reads exactly two keys — `five_hour` and `seven_day` (each
`{utilization, resets_at}`) → `UsageSnapshot { five_hour, seven_day }` (`usage.rs:22`) →
`QuotaWindow` slots on the account (`src/scheduler/window.rs:24`, `WindowSource::UsagePoll`).
**Everything else in the response is dropped.**

### What actually arrives (verbatim sample, `claude:acct-e@example.com`, 2026-07-03)

```json
{
 "five_hour":  { "utilization": 0.0,  "resets_at": "2026-07-03T07:29:59.682460+00:00",
                 "limit_dollars": null, "used_dollars": null, "remaining_dollars": null },
 "seven_day":  { "utilization": 58.0, "resets_at": "2026-07-03T21:59:59.682491+00:00",
                 "limit_dollars": null, "used_dollars": null, "remaining_dollars": null },
 "seven_day_oauth_apps": null, "seven_day_opus": null, "seven_day_sonnet": null,
 "seven_day_cowork": null, "seven_day_omelette": null,
 "tangelo": null, "iguana_necktie": null, "omelette_promotional": null,
 "nimbus_quill": null, "cinder_cove": null, "amber_ladder": null,
 "extra_usage": { "is_enabled": false, "monthly_limit": null, "used_credits": null,
                  "utilization": null, "currency": null, "decimal_places": null,
                  "disabled_reason": null, "daily": null, "weekly": null },
 "limits": [
  { "kind": "session",       "group": "session", "percent": 0,   "severity": "normal",
    "resets_at": "2026-07-03T07:29:59.682460+00:00", "scope": null, "is_active": false },
  { "kind": "weekly_all",    "group": "weekly",  "percent": 58,  "severity": "normal",
    "resets_at": "2026-07-03T21:59:59.682491+00:00", "scope": null, "is_active": false },
  { "kind": "weekly_scoped", "group": "weekly",  "percent": 100, "severity": "critical",
    "resets_at": "2026-07-03T21:59:59.682835+00:00",
    "scope": { "model": { "id": null, "display_name": "Fable" }, "surface": null },
    "is_active": true }
 ],
 "spend": { "used": {"amount_minor": 0, "currency": "USD", "exponent": 2}, "limit": null,
            "percent": 0, "severity": "normal", "enabled": false, "disabled_reason": null,
            "cap": null, "balance": null, "auto_reload": null,
            "disclaimer": "Usage credits cover you when you hit your plan limits. …",
            "can_purchase_credits": false, "can_toggle": false },
 "member_dashboard_available": false
}
```

Field semantics (observed across all 7 Claude accounts):

- **`limits[]` is the canonical structure** and matches the claude.ai usage UI 1:1:
  `kind:"session"` = the 5h gauge, `kind:"weekly_all"` = "All models" weekly,
  `kind:"weekly_scoped"` + `scope.model.display_name:"Fable"` = the separate "Fable" weekly gauge.
- `percent` is 0–100 int; `severity` ∈ `normal|warning|critical`; **`is_active: true` means that
  limit is currently engaged** (requests governed by it are being rejected).
- Top-level `five_hour`/`seven_day` duplicate the `session`/`weekly_all` rows (float 0–100) —
  legacy convenience fields. The per-model legacy fields (`seven_day_opus`, `seven_day_sonnet`,
  `seven_day_oauth_apps`, `seven_day_cowork`, `seven_day_omelette`) and the codename fields
  (`tangelo`, `iguana_necktie`, `omelette_promotional`, `nimbus_quill`, `cinder_cove`,
  `amber_ladder`) were **null on every account** — the Fable number does NOT use them; it only
  appears via `limits[].scope`.
- `extra_usage`/`spend`: usage-credit (overage) state; on `claude:acct-g@example.com` it showed
  `monthly_limit: 5000, disabled_reason: "out_of_credits"`.

### Fleet snapshot at capture time (why this matters operationally)

| account | 5h | 7d all | 7d **Fable** | fable is_active |
|---|---|---|---|---|
| acct-a@example.com | 0 | 59 | **100 critical** | **true** |
| acct-b@example.com | 100 (active) | 39 | 74 | false |
| acct-c@example.com | 64 | 54 | **95 critical** | **true** |
| claude:acct-d@example.com | 79 | 69 | **100 critical** | **true** |
| claude:acct-e@example.com | 0 | 58 | **100 critical** | **true** |
| claude:acct-f@example.com | 42 | 60 | **100 critical** | **true** |
| claude:acct-g@example.com | 68 | 36 | 60 | false |

5 of 7 accounts were Fable-exhausted (≥95, active) while their all-models weekly sat at 39–69%
and several 5h windows were at 0% — i.e. **plenty of non-Fable capacity that a whole-account
cooldown throws away.**

## Carrier 2 — attached to model turn-end responses

Two sub-carriers, parsed at `src/scheduler/headers.rs:117` (headers) and
`src/proxy/sse.rs:102` / `src/session.rs:115` (body usage).

### 2a. Rate-limit response headers (success path — verbatim, haiku 200 on dev1, 2026-07-03)

```
anthropic-ratelimit-unified-status: allowed
anthropic-ratelimit-unified-5h-status: allowed
anthropic-ratelimit-unified-5h-reset: 1783063800
anthropic-ratelimit-unified-5h-utilization: 0.01
anthropic-ratelimit-unified-7d-status: allowed
anthropic-ratelimit-unified-7d-reset: 1783116000
anthropic-ratelimit-unified-7d-utilization: 0.57
anthropic-ratelimit-unified-representative-claim: five_hour
anthropic-ratelimit-unified-fallback-percentage: 0.5
anthropic-ratelimit-unified-reset: 1783063800
anthropic-ratelimit-unified-overage-disabled-reason: org_level_disabled
anthropic-ratelimit-unified-overage-status: rejected
```

- llmux parses: `unified-5h/7d-utilization`, `unified-5h/7d-reset`, `unified-status`
  (`headers.rs:122,185-202`) plus the documented `requests/tokens` bucket headers (`:213-231`).
- llmux does NOT parse (new since parser was written): `unified-5h-status`, `unified-7d-status`,
  `unified-representative-claim`, `unified-fallback-percentage`, `unified-reset`,
  `unified-overage-*`.
- **No per-model/Fable header exists on non-Fable turns.** The `7d-utilization: 0.57` above matches
  weekly_all (58%), NOT the account's Fable bucket (100%) — headers track the aggregate windows.
- Whether a *successful Fable* turn carries a scoped-utilization header is **unverified** (both
  probe attempts at a Fable success 429'd; see 2c). llmux's parser would drop it today regardless.

### 2b. Body `usage` (token counts)

- SSE: `message_start` → `input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`;
  `message_delta` → `output_tokens` (`sse.rs:110-131`), accumulated in `StreamUsage`.
- Non-stream: top-level `usage.input_tokens/output_tokens` (`session.rs:123-127`).
- Model id is recorded alongside on the activity record and joined at aggregation time keyed by
  `(group, normalize_model(model))` (`src/tui/activity.rs:792,897`) → per-model token totals
  already exist (`ModelUsageDoc`, `src/dashboard.rs:556`). **Token counts only — no quota %.**

### 2c. The Fable-limit 429 (verbatim, fable-5 on Fable-exhausted dev1, 2026-07-03)

```
HTTP 429
x-should-retry: true
request-id: req_011CceMbbJP7RaU8xaN7whoj
anthropic-organization-id: 37d3d2cf-...

{"type":"error","error":{"type":"rate_limit_error","message":"Error"},"request_id":"req_011CceMbbJP7RaU8xaN7whoj"}
```

- **Zero scope information**: no `anthropic-ratelimit-unified-*` headers at all, no `Retry-After`,
  generic `rate_limit_error` body. Indistinguishable from a whole-account 429 by inspection.
- Same request with `claude-haiku-4-5-20251001` on the same account → **HTTP 200** (2a above).
  The account was NOT exhausted — only its Fable bucket was.
- Consequence in llmux today: 429 → `src/scheduler/mod.rs:290-291` — no `Retry-After` →
  `DEFAULT_HEURISTIC_COOLDOWN`, **whole-account**, model-blind. A Fable-only exhaustion therefore
  benches the entire account (the exact misbehavior reported 2026-07-03).
- Anomaly kept honest: a fable-5 probe on `claude:acct-g@example.com` (Fable 60%, not active) ALSO
  returned the same scope-less 429. Cause unknown — hypotheses: org/entitlement gating of direct
  API fable access, or request-shape (probe lacked a Claude-Code-shaped system prompt). Not
  explained by the usage snapshot. Design must therefore treat "fable 429 while snapshot says
  fable OK" as possible → fall back to conservative (account-wide) handling in that case.

## Implications (feeds `.prd/fable-usage/` design)

1. **The Fable weekly number is only available from Carrier 1's `limits[]`** — parse it
   (`kind == "weekly_scoped"`, keyed by `scope.model.display_name`; keep it generic: the scoped
   list is model-extensible, Fable is just today's occupant).
2. Headers (Carrier 2a) cannot feed a Fable gauge today; body usage (2b) already gives per-model
   token counts but not quota %.
3. **Cooldown separation (U8)** cannot be decided from the 429 itself. Design (strategist-
   converged, see `.prd/fable-usage/loop.md` W0): requested model is Fable-family → Fable-scoped
   cooldown FIRST, always — even when the snapshot disagrees (same-account haiku 200 proves the
   account isn't dead) — with a recorded classification reason + immediate poll refresh on
   mismatch; escalate to account-wide only on corroboration (non-Fable 429 / session or
   weekly_all active-critical / model-agnostic repeats).
4. Raw-io note: `raw-io.jsonl` does not capture the usage poll or response headers, and readable
   logs contain no usage-window JSON — live capture (this doc) is the only raw record.
