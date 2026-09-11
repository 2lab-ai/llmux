# Codex usage controls

Status: in-progress
Date: 2026-09-11

## Problem and target

External Codex resets do not invalidate llmux's observed quota windows. The OAuth poller excludes Codex; idle probes are throttled inference requests. Manual switching does not refresh usage, and TUI prechecks can refuse the action using cached limits.

Accounts must expose upstream reset entitlements and let the operator refresh usage or deliberately redeem one reset. This is an integration into existing CLI/TUI, not a replica of Codex branding or its full UX.

## Research and evidence

Primary: [openai/codex backend client](https://github.com/openai/codex/blob/fc948f8c473e5d11e780ffcf1fd7f812a2020932/codex-rs/backend-client/src/client/rate_limit_resets.rs), [types](https://github.com/openai/codex/blob/fc948f8c473e5d11e780ffcf1fd7f812a2020932/codex-rs/backend-client/src/types.rs), [TUI](https://github.com/openai/codex/blob/fc948f8c473e5d11e780ffcf1fd7f812a2020932/codex-rs/tui/src/chatwidget/usage.rs).

- GET `/backend-api/wham/usage`: `rate_limit.{primary_window,secondary_window}` with `used_percent`, `limit_window_seconds`, `reset_at`; optional `rate_limit_reset_credits.available_count`.
- GET `/backend-api/wham/rate-limit-reset-credits`: `available_count`, `credits[]` (`id`, `reset_type`, `status`, `granted_at`, `expires_at`, `title`, `description`). The supported filter literals `reset_type == "codex_rate_limits"` and `status == "available"` are evidenced by the official [backend contract fixture, lines 103–153](https://github.com/openai/codex/blob/fc948f8c473e5d11e780ffcf1fd7f812a2020932/codex-rs/backend-client/src/client/rate_limit_resets_tests.rs#L103-L153) and independently by all three entries in the live sanitized GET captured 2026-09-11. These are supported known values, not a claim that the string fields are a closed enum.
- POST `/backend-api/wham/rate-limit-reset-credits/consume`: `{redeem_request_id, credit_id?}` → `{code, windows_reset}`. Codes: reset / already_redeemed / nothing_to_reset / no_credit.
- Auth: Bearer account access token + ChatGPT-Account-ID; JSON POST; no redirect forwarding of credentials.
- The same idempotency key must survive uncertain retry; never automatically start a second redemption with a fresh key after a timeout.
- No paid credits purchasing and no automatic redemption.
- [LIVE] Read-only GETs on one configured account returned HTTP 200 for both endpoints, remaining reset count 3. Live usage had **primary_window duration 604800 (weekly)** and secondary null; position alone does not determine 5h versus 7d.
- [LIVE] Usage also returned `applicable_available_count: 0` alongside available_count 3. Applicability semantics are undocumented: expose if supplied, do not claim all owned resets are usable now. The consume response remains authoritative.
- [LIVE] Three credit descriptions explicitly said one free rate limit reset was granted. A fixed weekly grant is NOT established; do not promise one.
- No real consume was executed. Local captured evidence omits credentials/account/profile identifiers.

## Architecture decision

Use direct HTTP from the existing daemon rather than spawning per-account Codex app-servers. llmux already owns account credentials and HTTP clients; subprocesses would add lifecycle/auth-copy complexity. Parse the configured Codex upstream as an HTTP(S) URL, trim trailing slashes, require a final `/codex` path segment, then replace it with `/wham`. Reject unsupported shapes, userinfo, query or fragment with 422 before IO; never guess or fall back to production. Non-default upstream origins stay unchanged (mock-safe).

Add one daemon usage-control service shared by HTTP handlers and local TUI. It fetches Codex usage via GET, OAuth usage via the existing GET helper; unsupported providers report unsupported rather than spend inference quota. Explicit refresh bypasses scheduler eligibility and idle-probe cooldown but preserves pause/auth/operator policy. It never clears cached windows before a failed request. Fresh success updates the pool and fetch timestamp, and preserves upstream cooldown policy.

Account control metadata (optional reset counts/details, last successful refresh, sanitized error) is daemon-owned and serialized additively to dashboard/status; unknown is not zero. Do not persist quota metadata into credentials. Existing periodic mechanisms remain unchanged. Accounts entry displays cached observations and requests a refresh only when last successful refresh is absent or older than 60 seconds; concurrent account requests are not queued. Manual `f`/CLI refresh bypasses that entry-time floor. Batch refresh is sequential with bounded per-account timeout, and TUI performs it off the input/render loop so cancellation/navigation remains responsive.

## User contract

- `llmux accounts refresh [ACCOUNT]`: target account, or all supported subscription accounts when omitted. Per-account failures remain visible and CLI exits nonzero on partial failure.
- `llmux accounts resets ACCOUNT`: fresh reset entitlement list, never mutation.
- `llmux accounts reset ACCOUNT [--credit-id ID] [--request-id ID] --yes`: deliberate redemption. Without --yes, TTY confirmation names account and one reset; non-TTY refuses. Retry error prints the same request ID for safe explicit retry. No paid credit operations.
- `llmux accounts` retains offline roster fallback; when daemon is available it can expose live reset count/usage. `--json` remains machine-readable.
- Accounts overlay: `f` refresh, `R` select a Codex account and confirm one reset; `r` remains remove. Show per-account resets with unknown/not-applicable distinction. Local and attach modes share service semantics and refresh returned state.
- Reset action gate: obtain a successful fresh credit list before enabling a new redemption. `available_count == 0`, unknown list, or no `status == "available"` / `reset_type == "codex_rate_limits"` entry disables a new redemption. Positive available count plus an available matching credit permits **attempting** redemption even when `applicable_available_count == 0`; show “owned N · currently applicable 0 (server-reported)” and explicitly warn that upstream may return nothing_to_reset/no_credit. Applicability absent = unknown, never “usable”. The undocumented applicability field is display information, not an authoritative permission gate. Choose an available credit by explicit ID or earliest expiration; upstream still decides the result.
- Client owns the idempotency ID: CLI/TUI mint once per confirmed action unless --request-id supplied; daemon never substitutes/mints one. TUI holds account+credit+ID in pending/uncertain state and offers retry with **the same ID** or closing the dialog. Closing/abandoning the dialog is client-local only: it does not clear daemon pending state or authorize a new redemption. Reopening recovers the original pending ID from the authenticated server response; new IDs remain blocked until a terminal outcome resolves pending state. CLI prints the ID before sending and in every result/error so a restarted command can reuse it. A retry with an existing request ID bypasses fresh-inventory availability gating because the first attempt may already have consumed it; it must keep the original account and credit. Transport failure, unknown/malformed response or response loss are uncertain, not a clean failure.
- Per-account exclusion: concurrent control operations use a daemon try-lock; a competing operation returns 409/busy rather than queueing another consume. Tests send two different keys concurrently and assert upstream POST count <=1. After uncertain consume, daemon retains the account+credit+request ID and rejects new IDs; only the original ID can retry. A terminal four-code outcome releases the pending identity. No generic automatic retry.
- Crash boundary: before a consume POST, save a minimal non-secret pending receipt (account identity fingerprint, credit ID, request ID) atomically under the daemon state directory. Failure to save prevents POST. Startup reloads pending receipts and requires the same ID before another reset; terminal result clears the pending receipt. No entitlement data or credentials are copied. Receipt validation includes account identity; replacement cannot redeem using a predecessor's pending identity. Client receipt supplies the original ID for reconnection; server conflict exposes the pending ID to an authenticated operator. This is narrowly scoped retry safety, not a job queue.
- Account identity/generation safety: capture the account credential identity and roster generation before IO; after IO revalidate under the pool lock before applying observations or committing a switch. Removed/replaced credentials cannot receive old results. Metadata is invalidated on replacement/removal. Serialize per-account control calls without holding std locks across IO. A completed consume for a removed/replaced account reports its outcome without mutating the new account; never retry it with new credentials.
- Manual switch refreshes selected OAuth/Codex account before committing (including already active target). Remove stale client-side eligibility rejection; server remains authority for pause/auth/cooldown. Quota thresholds alone never refuse a manual switch, even if the just-refreshed utilization is 100% (preserves the existing operator override). If refresh fails, retain the existing switch ability but display a warning separately from switch result. Unsupported refresh does not block switching other providers.
- On reset/already_redeemed, re-read usage and reset inventory. A post-reset refresh failure is a **successful redemption with stale-read warning**, not a retryable redemption failure.

## Non-goals and acceptance

No quota persistence redesign, scheduler score change, paid billing integration, automatic credit use, or production credential cloning.

See [workstream SSOT](codex-usage-controls/ssot.md), [vertical traces](codex-usage-controls/trace.md), and [verification driver](codex-usage-controls/loop.md). Required proof: malformed/missing fields do not become fake zeros; live-shaped primary weekly maps to weekly; refresh replaces exhausted usage; failed refresh preserves prior state; account isolation/auth/confirmation/idempotency; local + attach full accounts frames; forced switch before/after; partial failures; docs and complete project gate.
