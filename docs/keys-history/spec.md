# Keys history and request-limit fidelity

Goal: preserve explicit token limits; make keys history durable, selectable and navigable; recover existing accounts after re-login. User explicitly requests fixes through verified preview delivery.

## Acceptance and trace

| User clause | Contract / receipt |
|---|---|
| `max_tokens=1` codex/grok `설정 그대로` (revised by the user after the 2026-09-14 receipts) | Grok forwards exactly `max_output_tokens:1`, never silently drops or retries without a cap, and preserves true usage. Codex cannot: the gateway rejects the tested field — `400 {"detail":"Unsupported parameter: max_output_tokens"}` (2026-09-14, `gpt-6-astra`) — and no supported alternative output-cap field was found in the current official Codex client or its docs (struct `ResponsesApiRequest`, openai/codex @`3abbf9f`; the open request openai/codex#36180 is about that client, not a gateway allowlist) — other field names remain untested, so the omission stands and llmux fakes no local enforcement. The user's revised contract is then documentation: the Codex caveat is written up in [`docs/provider-compatibility.md`](../provider-compatibility.md) and the root README, and every future provider/model integration audits the same axes ([`rules/documents.md`](../../rules/documents.md) checklist item 3). Test wire bytes for streaming and nonstream; an upstream rejection receipt is NOT proof of successful cap enforcement. |
| `last 1 h, last 24h, last 7d, 14d, 30d, 90d` | Exact rolling windows [now-duration,now], boundary tests, exclude future timestamps; keep All history default for backwards compatibility. |
| `위우래 키로 볼수 있게` | Up/Down and PageUp/PageDown/Home/End reach every key and model row. Full terminal frame proves tail visibility. |
| `모델들 filter로 고를수` | Multi-select observed model names, clear/all; totals, errors, tokens, cost and span reflect selected models AND window. No selection means all. |
| `name을 맨 앞` / `key 정보는 16자 정도` | Name column first, key display at most 16 display cells including ellipsis; never raw secret. |
| `sqlite` / `~/.config/llmux/` / `~/.config/llmux-preview/` | usage.sqlite3 under channel directory. Explicit config overrides derive isolated sibling directory. JSONL import idempotent; no deletion of old history; restart count equality. |
| `accounts에서 login ... 기존 계정 덮어써` | Same identity updates credentials on disk/live, clears stale authentication failure, no duplicate; unchanged credentials do not revive failed account; preserve quota, pause, in-flight state. |

## K: keys usage vertical trace
0. Client: K opens table; w cycles window; f opens multi-select models; arrows/Space choose; Enter applies; Esc closes picker; arrows outside picker scroll flattened rows. Loading/failure explicitly visible, never lifetime data relabelled filtered.
1. API: admin-authenticated `GET /llmux/keys/usage?window=<all|1h|24h|7d|14d|30d|90d>&models=<a,b,c>` — the model filter is ONE comma-separated parameter, absent/empty meaning every model (`src/proxy/server.rs:1643-1690`). Unknown window => 400; no durable store => 503; failed query => 500 (never a valid-looking empty view).
2. Inputs: window all/1h/24h/7d/14d/30d/90d, models optional exact observed names; invalid window =>400.
3. Flow: RequestFinished.tenant/group/model/tokens/status/time -> durable SQLite usage row -> timestamp+model WHERE predicates -> tenant/model sums -> TenantUsageDoc -> local/remote identical rendering. Normalize model consistently with existing normalize_model. Retain unattributed failures under all-model query; exclude when explicit models chosen.
4. Side effects: durable rows recorded; import legacy activity.jsonl using persistent offset/source identity transactionally to avoid duplicate count and migration/live overlap; file private permissions; no credentials or prompt content in new DB. Continue JSONL activity history so other existing features do not regress.
5. Errors: SQLite open/write/query errors visible in diagnostics/UI, not silently shown as valid zero; never damage serving path. Avoid holding hub/pool locks across DB I/O; use blocking pool for disk/query work; no per-frame SQL queries or full-history replay.
6. Output: a `KeysUsageDoc` (`src/key_usage.rs:1284-1302`) — `tenants` rows (named + priced server-side) plus `available_models` and the applied-filter metadata `window`/`models`/`from_ms`/`to_ms`/`rows`/`generated_ms`/`health`; filtered requests/ok/errors/token/cost/span consistent. Persistent store serves bounded indexed queries; tests can construct in-memory/temp stores without touching live config.
7. Observability: migration/writes/errors reported without credentials; screenshot/text frame and DB counts captured.
Files: src/dashboard.rs, src/proxy/server.rs, new src/key_usage.rs, src/lib.rs, src/tui/{mod,ui,view}.rs, Cargo.toml/Cargo.lock; owning docs docs/operational-reference.md, docs/configuration.md; .prd non-goals amended for keys-only SQLite.

## T: explicit limit vertical trace
1. POST /v1/messages auth -> routing -> provider build_request.
2. body.max_tokens positive integer (explicit null/zero/negative rejected locally).
3. body.max_tokens -> responses_request::convert.max_output_tokens -> build_responses_body.max_output_tokens exactly on grok; on codex the field is omitted and reported (`src/provider/responses_request.rs:255-290`), because the only cap field measured on that gateway is refused.
4. One upstream request, capped where the backend takes a cap; never a capless retry, never local truncation, never a synthesized cap on codex.
5. Upstream unsupported-field error preserved, strict policy rejects the documented semantics warning before credential refresh.
6. Real SSE/nonstream output and usage retained; `incomplete/max_output_tokens` translates to Anthropic stop_reason=max_tokens.
7. Documentation is part of the contract, not a follow-up: the codex omission and the grok semantics gap are user-visible in the README caution and the provider matrix, labelled untested vs unsupported.
Files: src/provider/{responses_request,codex,grok}.rs, tests/token_limits.rs, docs/responses-compatibility/spec.md, docs/provider-compatibility.md, README.md, rules/documents.md.

## L: re-login vertical trace
0. Accounts n -> provider login -> fresh AccountConfig; local inject or remote POST /llmux/inject-account.
1. Admin-only inject endpoint.
2. Stable provider identity dedup then name fallback (existing contract).
3. config::update_path -> Config::upsert_account replaces row -> AppState::apply_roster -> AccountPool::reload_accounts replaces credentials + invalidates old generation; changed credentials restore AuthFailed to Healthy.
4. Atomic config update, live roster refresh; unchanged credentials preserve failure, quota/pause/leases unaffected.
5. Failed persistence leaves live state unchanged; no duplicate account.
6. Existing Updated response retained; frontend status says updated rather than added.
Files: src/cli/login.rs (identity gate), src/config/schema.rs (`locate_account`/`upsert_account`, `update_oauth_tokens_if`), src/proxy/server.rs (`inject_account`/`apply_roster`, background refresh pass), src/proxy/forward.rs (fingerprint-guarded refresh/auth-failure call sites), src/scheduler/mod.rs (`reload_accounts`, `record_auth_failure_if`, `update_credential_if`), src/scheduler/usage.rs (guarded poll), tests/relogin.rs; owning doc docs/operational-reference.md (login/re-login paragraph). Concurrency amendment with the code-level hops and the B1-B7 breaks: [`relogin-trace.md`](relogin-trace.md). Frontend label change by the keys UI owner.

## Implementation / verification plan
- [x] Token worker: RED exact forwarding assertions -> minimal mapping -> scoped tests and docs (docs = `docs/provider-compatibility.md`, README caution, `rules/documents.md` provider-axes checklist).
- [ ] Login worker: RED authfailed/new token reload assertion -> restore health only on changed credentials -> retention/unchanged tests and endpoint receipt.
- [ ] Keys worker: store/query/migration tests RED -> SQLite store -> daemon integration -> UI state/filter/navigation -> full-frame render tests; update owning docs and non-goal amendments.
- [ ] Parent: just check directly; external review per natural unit; fix findings via same workers.
- [ ] Parent: isolated real TCP + TUI frame + restart/migration receipts; actual provider wire probe, do not claim unsupported upstream capability.
- [ ] Commit/push PR; CI green -> merge -> preview.yml -> release and tap verified -> brew preview upgrade -> single detached restart -> live status/TUI smoke.
