# Usage control vertical traces

Status: planned
Date: 2026-09-11
Contract: [16-codex-usage-controls](../16-codex-usage-controls.md)

## Implementation status

| Scenario | Contract test | Implementation | Observation |
| --- | --- | --- | --- |
| Refresh an externally reset account | Behavioral RED + GREEN | Implemented | Runtime100→43 and503 preserve71 |
| View and redeem account reset | Behavioral RED + GREEN | Implemented | Local/CLI fake consume3→2; real consume0 |
| Force switch with fresh usage | RED + GREEN | Implemented | Runtime71, local fresh100% manual override |
| Accounts local/attach/CLI parity | RED + GREEN | Implemented | Full160×44 and100×35 frames |

## S1 Refresh

0. Client: accounts `f` / CLI `accounts refresh [ACCOUNT]` queues an explicit refresh; TUI shows pending then per-account result and new gauges. Entry displays cache and queues refresh only if no successful observation or older than 60s; not every render. Explicit f bypasses this floor. All batch IO is sequential and off the UI input/render loop.
1. API: POST `/llmux/refresh-usage`, existing x-api-key ADMIN gate.
2. Input: `{account?: string}`; absent = all OAuth/Codex, unknown name = 404; explicitly unsupported provider = 422.
3. Flow: request.account → AccountId(name) → pool credential for that name → provider-specific GET with that credential → usage normalized to WindowReading → pool.record_usage → dashboard/status account window → DashboardView → gauge. Codex window duration 18000 → five_hour; 604800 → seven_day (never infer from primary position). Same upstream fetch yields optional available/applicable reset counts. Account metadata uses the exact same name plus credential generation. Capture identity before IO, atomically revalidate identity/generation under the pool write lock before applying observations or switching, invalidate metadata for replaced/removed identities. No IO under std state locks. Bound each upstream call with timeout; a daemon per-account try-lock returns 409 busy for competing controls rather than queueing a second consume. URL parsing preserves upstream origin, requires final /codex (trailing slash tolerated), substitutes /wham; invalid shape/userinfo/query/fragment → 422 before IO.
4. Effects: only successful observations mutate quota/control metadata. Failure retains old observations and reports error; no config/credential copies. Explicit refresh does not change pause/health/operator settings. No inference POST.
5. Errors: auth 403 before IO; unknown 404; unsupported 422; upstream non-2xx/malformed/timeout → sanitized per-account error, partial multi-account response is not silently green. Do not parse an HTML error body into valid empty state.
6. Output: `{ok, results:[{account, ok, error?, ...}]}`; success snapshot includes additive metadata in GET status/dashboard. CLI nonzero if any requested account failed.
7. Evidence: tests replace 100% cached window with live 43%-weekly-primary fixture; validate identity, headers, endpoint, normalized 0.43, timestamps, cache unchanged on error, all-account partial failure.

## S2 Reset list and consume

0. Client: `accounts resets ACCOUNT` or TUI reset selection reads entitlement; `accounts reset ACCOUNT` / TUI R-confirm names account and one reset before consume. Cancellation or unknown/zero owned inventory cannot start a NEW redemption. Fresh available_count>0 plus available codex_rate_limits credit permits an ATTEMPT; applicable_available_count=0 is shown as server-reported applicability with warning, not an invented hard gate. Unknown applicability never renders usable. TUI repeated Enter while pending cannot enqueue duplicates. Client mints one ID at confirmation (CLI can supply --request-id) and prints/preserves it before send; TUI holds account+credit+ID through uncertain retry. Closing/abandoning the dialog only dismisses client UI; daemon pending survives, new IDs stay blocked until a terminal response, and reopening recovers original ID from authenticated pending metadata. No new key is silently generated. Retrying that existing ID bypasses new-inventory gating, because the first attempt may have consumed the credit.
1. API: GET `/llmux/reset-credits?account=NAME`; POST `/llmux/reset-credits/consume`; ADMIN gate inherited.
2. Input: consume `{account, redeem_request_id, credit_id?, confirm:true}`; reject empty identity/request id/credit id and absent confirmation before network.
3. Flow: selected account name → AccountId → Codex credential only → GET WHAM reset credits / POST `{redeem_request_id,credit_id?}` with selected ChatGPT-Account-ID → typed outcome → on reset/already_redeemed re-read usage+inventory → pool+account metadata → JSON and TUI message/count. Optional credit_id omitted, not serialized null. Daemon never substitutes the client's ID. Persist pending (account identity fingerprint, credit ID, request ID) atomically to state directory before POST; persistence failure prevents POST. Startup reloads it; unresolved pending accepts only original ID+credit+identity, conflicting new key returns 409 plus pending ID to admin. Save no credentials. Stable request ID reused after uncertain transport failure; no automatic new-key retry. Terminal four-code outcome clears pending receipt. Live available_count is not applicable_available_count; expose both when present.
4. Effects: POST spends at most one reset according to upstream idempotency; local success never fabricates zero usage or decrements counts by guessing. No paid balance changes. Post-success refresh failure preserves outcome and adds warning; retrying as a fresh redemption is not suggested.
5. Errors: ADMIN 403 before ANY IO; unknown 404, non-Codex 422, invalid/confirmation missing 400; unavailable entitlement upstream no_credit distinct from network failure. nothing_to_reset distinct from reset. Unknown outcome/malformed response = uncertain error with original request ID retained. Never reflect credentials/raw HTTP request in errors.
6. Output: `{outcome, request_id, windows_reset, refresh_warning?}`; list uses upstream details plus observation metadata. Non-success outcomes do not masquerade as reset success.
7. Evidence: fake upstream captures account headers and same-key retries; two competing different IDs produce <=1 upstream POST; pending receipt survives daemon recreation; storage failure prevents POST; replacement during IO cannot mutate the successor; no POST on cancel/invalid/non-Codex/unauthorized; all four outcomes, timeout, successful consume then failed follow-up read, absence of real consumption.

## S3 Manual switch

0. Client: `s` select Enter queues same operation in local and attach; remove client cache eligibility/current shortcuts. Already-active selection still refreshes.
1. API: existing POST `/llmux/switch` (ADMIN), local calls same async service.
2. Input: `{account}` unchanged.
3. Flow: request.account → selected credential → S1 refresh if supported → pool.switch_to(target, select_params, now) → AccountSwitched event → response includes separate refresh warning → UI displays final result, then reloads doc. Pause/auth/cooldown enforcement stays in pool; quota thresholds alone never reject manual switch, even fresh100%; no scheduler-selection refactor.
4. Effects: fresh readings before switch and existing manual pin only on successful switch. Failed refresh never clears windows or unpauses account. Unsupported provider keeps existing switch semantics.
5. Errors: switch refusal 409 with reason; successful switch plus refresh error returns success+warning, not silent success or fictional fresh data.
6. Output: `{ok:true,current,refresh_warning?}`; same target allowed for refreshing.
7. Evidence: assert upstream GET observed before pool commit, stale cached overthreshold cannot be blocked by TUI preflight, active target refresh, paused still refused, unrelated group selection unchanged.

## File map and ownership

- Backend WU: `src/proxy/usage_controls.rs` (new), `src/proxy/mod.rs`, `src/proxy/server.rs`, `src/auth/codex_usage.rs` (new), `src/auth/mod.rs`, `src/dashboard.rs`, `src/scheduler/mod.rs` (atomic identity-aware observation/switch application only), backend integration tests. Metadata lives in AppState and is added to AccountDoc/status; no AccountSnapshot field explosion required.
- Client WU (after backend contract): `src/cli/mod.rs`, `src/cli/accounts.rs`, `src/tui/mod.rs`, `src/tui/view.rs`, `src/tui/ui.rs`, client tests, `docs/operational-reference.md`.
- Dispatcher: `.prd` contract/loop and direct verification. Unrelated main changes excluded.

## Trace deviations

None yet. Update contract before implementing a deviation.
