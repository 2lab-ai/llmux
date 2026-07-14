# Grok provider — vertical traces

> The Trace is the Source of Truth (STV). Code behaves as written here; divergence found
> during implementation updates THIS file first (Delta log at bottom). Line refs to
> existing code are master @ 7f1dbec; new code is named by target file.

## T1 — CLI registration: `llmux login --grok`

1. **Entry** — CLI `llmux login --grok` (`src/cli/login.rs::run`, new `--grok` arm
   beside `--codex` at login.rs:12-20). No daemon required.
2. **Input** — none (interactive). Env: network to `auth.x.ai`.
3. **Layer flow** —
   `LoginArgs.grok=true` → `login_grok()` →
   `auth::grok::discover(client)` GET `https://auth.x.ai/.well-known/openid-configuration`
   → `Discovery{device_authorization_endpoint, token_endpoint}` (https + host ∈ x.ai
   enforced) →
   `auth::grok::request_device_code(client, discovery)` POST form
   `client_id=b1a00492-…&scope=openid profile email offline_access grok-cli:access api:access`
   → `DeviceCode{device_code, user_code, verification_uri_complete, expires_in, interval}` →
   print URL+code, best-effort `open` →
   `auth::grok::poll_token(client, token_endpoint, device_code)` POST form
   `grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code={…}&client_id={ClientID}`
   every `interval`s (authorization_pending → continue; slow_down → interval+5s;
   expired_token/access_denied → terminal error) →
   `TokenResponse{access_token, refresh_token, id_token, expires_in}` →
   `email = jwt_claims(id_token).email` (fallback `sub`, then epoch-ms) →
   `AccountConfig{ name: "grok:{email}", credential: AccountCredential::Grok{
     access_token, refresh_token, expires_at_ms: now+expires_in*1000,
     token_endpoint, last_refresh_ms } }` →
   `config::update_path(path, upsert)` (same upsert-by-name the codex flow uses,
   login.rs:170-185).
4. **Side effects** — `~/.config/llmux.json` gains/updates one account; running daemon
   unaffected until restart or `/llmux/inject-account` (unchanged behavior, parity with
   `--codex`).
5. **Errors** — discovery non-200 / non-x.ai host → CliError printed; poll timeout
   (min(30min, expires_in)) → "device code expired"; access_denied → "authorization
   denied". No partial config writes (upsert is read-merge-write).
6. **Output** — stdout `Added grok account "grok:{email}"` (or `Updated …`), exit 0.

## T2 — Islands registration: daemon-run device flow

1. **Entry** — `POST /llmux/login/start` `{"provider":"grok"}` (server.rs:775 route,
   loopback/api-key gated).
2. **Input** — provider string; parsed by `LoginProvider::parse` — new arm
   `"grok"|"xai"|"x-ai" → Grok` (proxy/login.rs:34-39).
3. **Layer flow** —
   `login_start_endpoint` → `LoginRegistry.start(Grok)` (single-slot, 409 when busy —
   unchanged) → spawned task: discovery → device code →
   **registry publishes the URL** via `LoginRegistry::set_verification(state,
   verification_uri_complete — or plain verification_uri when `_complete` is absent,
   user_code)`, stored as `LoginJob` fields (NOT a `LoginPhase::Pending` payload —
   the phase stays a unit variant; see Delta log). Claude/Codex flows never call it,
   so their status carries neither field; islands shows `user_code` beside the link
   so the plain-URI fallback stays usable →
   poll loop (as T1) → on success `AccountConfig` → **inject into the LIVE pool** +
   persist (same path the daemon-run codex login uses, server.rs:376-381) →
   `LoginPhase::Done{account}`.
   Islands: `LlmuxClient.startLogin(provider:"grok")` → poll
   `GET /llmux/login/status` → when `verification_uri` non-nil → render the clickable
   link + `user_code` in the login progress view (the daemon already opened the page
   host-side; a second client-side open would double-tab local setups — the link
   covers remote daemons) → keep polling → Done → refresh status.
4. **Side effects** — config file + live pool gain `grok:{email}`; one browser tab.
5. **Errors** — busy: 409 `{error:"login already pending"}`; poll terminal errors →
   `LoginPhase::Error{message}` (token-free); islands shows message, offers retry;
   cancel endpoint aborts the poll task (existing semantics).
6. **Output** — `login/status` `{state, phase:"done", account:"grok:{email}",
   verification_uri?, user_code?}`.

## T3 — Chat request: Claude Code `/model grok-4.5` (happy path)

1. **Entry** — `POST /v1/messages` (Anthropic Messages, SSE), proxy client auth as
   today (forward.rs entry).
2. **Input** — Messages body: `model:"grok-4.5"`, `messages[]`, optional `system`,
   `tools[]`, `stream:true`, optional `output_config.effort` (Claude Agent SDK).
   Validation unchanged (existing body parse).
3. **Layer flow** (transformation arrows) —
   `body.model "grok-4.5"` → `Classifier::classify` (routing.rs; builtin
   `Prefix("grok")`) → `BackendGroup::Grok` →
   `resolve_group` (forward.rs:1136; on_empty fallback order Claude→Codex→Grok when
   empty) → scheduler `select(group=Grok)` — pool filter
   `BackendGroup::from_kind("grok")` (scheduler/mod.rs:675) → leased
   `AccountCredential::Grok{access_token, …}` (refresh first when
   `expires_at_ms - now < REFRESH_AHEAD_MS` via `refresh_credential` new Grok arm →
   `auth::grok::refresh_at(client, token_endpoint, refresh_token)`) →
   provider dispatch (forward.rs:726-770 → 3-way match) → `state.grok.build_request`:
   `body.model` → `responses::resolve_upstream_model(requested, shape.model,
   flavor.model_passthrough = starts_with("grok-"))` → `"grok-4.5"` (verbatim) ·
   `body.output_config.effort` → `responses::resolve_effort(…, clamp: none|minimal→low,
   xhigh|max|ultra→high)` → e.g. `"high"` ·
   `messages[]` → `responses::messages_to_input` → Responses `input[]` ·
   `system` + folded system-role msgs → `instructions` ·
   `tools[]` → `responses::tools_to_functions` →
   upstream JSON `{model:"grok-4.5", instructions, input, tools,
   parallel_tool_calls:true, store:false, stream:true, prompt_cache_key:session_id,
   reasoning:{effort:"high"}}` (NO `include`, NO `service_tier`; `reasoning` OMITTED
   entirely for models outside the thinking-levels table, e.g. grok-build-0.1;
   `session_id` = per-process `uuid_v4()`, one per `GrokProvider` instance — same
   stability contract as codex's `prompt_cache_key`, codex.rs:79-83; it is NOT
   per-conversation) →
   headers: `Authorization: Bearer {access_token}`, `Accept: text/event-stream`,
   and (upstream == official cli-chat-proxy) identity trio
   `X-XAI-Token-Auth: xai-grok-cli` / `x-grok-client-version: 0.2.93` /
   `User-Agent: xai-grok-workspace/0.2.93` — **no `x-grok-conv-id`** (spec §R1,
   consensus round 3: CLIProxyAPI omits it for standard chat) →
   `POST https://cli-chat-proxy.grok.com/v1/responses` →
   Responses SSE → `responses::SseTransform` (shared, currently proxy/sse.rs path) →
   Anthropic SSE events → client. `client_model` override applies to the reported
   model name if configured (parity codex.rs:41-49).
4. **Side effects** — activity log row `(group=grok, model=grok-4.5, effort=high)` via
   grok `effective_request_meta`; `StreamUsage{input, output, cached}` → dashboard
   token/cost accumulation (pricing: grok-4.5 rates); scheduler in-flight counters.
5. **Errors** — no grok account & on_empty=error → 404 not_found_error (the existing
   `resolve_group` contract, forward.rs:1136 — a clean Anthropic-shaped not-found, NOT
   a 503);
   upstream 401 → refresh-once-then-fail (existing credential path); upstream 429 →
   T5; upstream 4xx/5xx passthrough as today; malformed SSE → existing SSE error
   handling (unchanged core).
6. **Output** — Anthropic SSE (`message_start` … `message_stop`) or non-stream JSON;
   `usage` populated from Responses `response.completed.usage`.
7. **Observability** — `grok.trace=true` mirrors `codex.trace` raw-io capture
   (proxy/raw_io.rs path), same file naming with `grok` tag.

## T4 — Live model/effort switch (daemon HTTP API; interactive callers v1-descoped)

1. **Entry** — `POST /llmux/grok` (new route beside `/llmux/codex`, server.rs:775 chain,
   same auth gate).
2. **Input** — `{"default_model"?: string, "reasoning_effort"?: string}` — partial;
   empty/`"unset"` effort clears; unknown fields ignored (parity: codex struct).
3. **Layer flow** — deserialize → `state.grok.shape()` → merge → validate effort ∈
   SUPERSET {none, low, medium, high, "" (clear)} (invalid → 400; the per-MODEL clamp
   happens at request time against the effective model — spec §R1 single-source rule;
   codex endpoint does not validate at all; recorded as intentional asymmetry) →
   `state.grok.set_shape(…)` (RwLock write) →
   `config::update_path(c.grok.default_model = …, c.grok.reasoning_effort = …)`.
4. **Side effects** — next request uses the new shape; config persisted.
5. **Errors** — config write failure → 200 with live-applied, `persisted:false` in the
   body + log warn (improves on codex's silent best-effort, server.rs:1119-1127;
   documented asymmetry — codex endpoint unchanged in v1).
6. **Output** — `{ok:true, default_model, reasoning_effort, persisted:bool}`.
   Caller in v1: the daemon HTTP API only (direct/script). The TUI `f/m/e`-style
   grok control (codex parity, tui/mod.rs:1160) and the islands settings row are
   v1-descoped (spec §R4) — neither exists in this PR. The core R4 switch
   (Claude Code `/model grok-4.5`) is the routing path in T3, not this endpoint.

## T5 — Quota exhaustion: 429 free-usage-exhausted

1. **Entry** — upstream response inside T3 step "POST …/responses".
2. **Input** — HTTP 429; possibly a `Retry-After` header; body JSON possibly with
   `code`/`error` containing `free-usage-exhausted` / `included free usage`.
3. **Layer flow** — forward.rs upstream-error path → precedence: (1) `Retry-After`
   header present → existing generic handling, UNTOUCHED; (2) else marker match in
   body → new grok arm in the retry_after resolution (forward.rs:1347 kind match):
   `retry_after = Some(24h)` as an **estimated probe-not-before** (the 429 does not
   carry the true rolling-window reset; 24h is xAI's advertised window) → scheduler
   parks the account (same machinery a Retry-After header takes) → selection moves to
   the next grok account. **All grok accounts parked ≠ empty group**: the request
   stays in-group and takes the existing all-limited behavior (identical to the claude
   group today) — `on_empty_group` fires only at zero CONFIGURED accounts (spec §R5).
4. **Side effects** — account window state marked limited w/ estimated reset ts
   (surfaces render as estimate); activity event (existing rate-limit event kind).
5. **Errors** — generic 429 without header or marker → existing backoff (no 24h park).
6. **Output** — client sees retry-on-next-account (transparent) or the in-group
   all-limited response when the pool is exhausted (existing behavior).

## T6 — Stats & status surfaces

1. **Entry** — `GET /llmux/status` (daemon JSON), `llmux status` CLI, islands poller.
2. **Input** — none.
3. **Layer flow** — pool snapshot → per-account `{name:"grok:{email}", type:"grok",
   group:"grok", in_flight, five_hour:null, seven_day:null, parked_until?}` →
   CLI table renders the grok rows with "—" gauges (existing accountless-gauge glyph);
   TUI groups by `BackendGroup::as_str` (adds "grok" bucket);
   islands `LlmuxStatus.group == "grok"` → `UsageProvider.grok` →
   `IslandUsageModel.inFlightCounts` returns `(claude, codex, grok)` → closed label
   `[icon]{n}` triple → tiles/analytics keyed by provider case.
4. **Side effects** — none (read-only).
5. **Errors** — old islands + new daemon: unknown `group:"grok"` string previously fell
   back to `.claude` (IslandUsageModel.provider(of:) checks `== "codex"`); acceptable
   during upgrade window. New islands + old daemon: no `"grok"` groups appear — UI
   renders as today (fixtures excluded).
6. **Output** — status JSON extended additively; no field removed or re-typed.

## Contract tests (RED first; file: tests/ + unit tests beside code)

| # | Kind | Asserts | Source trace |
|---|---|---|---|
| C1 | Contract | Messages body w/ model grok-4.5 + output_config.effort=xhigh → upstream JSON: model verbatim, reasoning.effort="high" (clamped), NO include/service_tier, store:false | T3 §3 |
| C2 | Contract | model "gpt-5.6-sol" routes Codex; "grok-4.5" routes Grok; "claude-…" routes Claude; unmatched → default_group; non-empty `routing.grok_models` REPLACES builtin grok rules | T3 §3 |
| C2b | Contract | on_empty_group="fallback": zero configured grok accounts + grok model → fixed-order fallback (Claude first configured wins) + serving provider's own model rewrite applies; group with only PARKED accounts does NOT fall back | T3 §3, T5 |
| C3 | Contract | grok headers: Bearer + identity trio on official upstream; identity trio ABSENT on custom upstream; NO x-grok-conv-id either way | T3 §3 |
| C4 | Sad | effort "none" → "low" (grok-4.5, zero disallowed); "none" on grok-4.3 (none in level set) → reasoning field OMITTED; "ultra" → "high"; invalid effort string → shape effort fallback; model outside thinking table (grok-build-0.1) → NO reasoning field even with effort configured | T3 §3 |
| C5 | Happy | device-code poll: form carries grant_type+device_code+client_id; pending→pending→success yields TokenData; slow_down grows interval | T1 §3 (mock server) |
| C6 | Sad | poll access_denied → terminal error, no config write | T1 §5 |
| C7 | Contract | AccountCredential::Grok serde round-trip in llmux.json; upsert-by-name idempotent | T1 §3-4 |
| C8 | Contract | refresh_credential Grok arm: form grant_type=refresh_token+client_id to stored token_endpoint; response WITH refresh_token rotates it, WITHOUT keeps old; expires_at_ms+last_refresh_ms updated in same swap; invalid_grant → Permanent | T3 §3, spec §R1 |
| C9 | Sad | 429 + marker body (no Retry-After) → 24h estimated park; 429 + Retry-After header → header wins; generic 429 → no explicit park | T5 |
| C10 | Contract | POST /llmux/grok partial update + persist (response carries persisted:true/false); effort superset none/low/medium/high accepted, garbage → 400 | T4 |
| C11 | Contract | /llmux/login/start grok → status carries verification_uri (+user_code) while pending; _complete absent → plain URI fallback | T2 |
| C12 | Regression | ENTIRE existing codex test suite green with codex.rs as adapter (no behavioral diff), at the extraction commit itself | R5 |
| C13 | Contract | status JSON: grok account row shape (group/type/gauges null) | T6 |
| C14 | Contract | pricing: (grok, grok-4.5) → 2.0/6.0/0.5/0.0; (grok, unknown) → grok fallback | T6 §4 |
| C15 | Contract | client `stream:false` through grok: upstream SSE aggregated to one Anthropic JSON response w/ usage (same path codex takes today) | T3 §3/§6 |
| C16 | Contract | flavor-parameterized translation: system folding, tools_to_functions, tool_use/tool_result round trip, SSE→Anthropic events, usage extraction — asserted under the GROK flavor (not only codex) | T3 §3, R5 |
| C17 | Regression | pre-grok config (no grok fields) parses + round-trips VALUE-stable (new default grok section serializes; old binaries ignore it) — additive-only guarantee | spec §Compatibility |

Swift (islands) — unit-light per repo convention: provider(of:) mapping test if a test
target exists for it; otherwise receipt = snapshot/live capture.

## Implementation status

| Unit | Status |
|---|---|
| responses core extraction (C12) | GREEN (commit cd192f3; 718+38 unchanged) |
| routing 3rd group (C2, C2b) | GREEN |
| config schema + credential (C7, C17) | GREEN |
| auth::grok device flow (C5, C6, C8) | GREEN |
| provider grok adapter (C1, C3, C4, C15, C16) | GREEN |
| forward dispatch + refresh + 429 (C9) | GREEN |
| server endpoints (C10, C11) | GREEN |
| pricing (C14) | GREEN |
| CLI login --grok | built (device-flow units green; interactive path = live receipt) |
| islands Swift surfaces | built (swiftc -parse clean; type-check = preview CI) |
| status JSON grok row (C13) | GREEN — kind-driven group serialization (server.rs:1116) + C7 serde; structurally confirmed on the scratch daemon (`group:"grok"`, `type:"grok"`, gauges null) |
| live receipt (mock upstream) | DONE 2026-07-14 — scratch daemon :3499, 2-turn tool-call e2e vs mock Responses, wire-shape capture (C1/C3/C4/C15/C16), live shape switch/persist (C10), 429→24.0h park (T5); real auth.x.ai device-code mint (C11). Caught+fixed a real defect (condense_error_body, 26aaf98) |
| live receipt (real cli-chat-proxy.grok.com) | NOT STARTED — merge-blocking, needs an authorized grok account (device-flow approval) |

## Trace deviations (Delta log)

- MODIFIED (implementation, 2026-07-14): T2 §3 — `LoginPhase::Pending` stays a
  unit variant; the verification URI/user code live as `LoginJob` fields with
  `LoginRegistry::{set_verification, verification}` and the status endpoint
  reads them. Wire contract unchanged (status JSON carries
  `verification_uri`/`user_code` while pending). Smaller diff than a
  struct-variant extension across every phase match.
- MODIFIED (implementation, 2026-07-14): islands does NOT auto-open the
  verification URL (T2 §3 said `NSWorkspace.open` once). The daemon already
  opens it host-side (parity with the PKCE flows); a second client-side open
  would double-tab local setups. Instead the login progress view shows the
  clickable link + user code, which also covers the remote-daemon case.
- MODIFIED (implementation, 2026-07-14): spec §R4's "islands settings pane
  gains a grok row" is deferred — islands has NO codex settings row either
  (`POST /llmux/codex` is dashboard/CLI-only today), so a grok-only row would
  exceed codex parity and the user requirements (R2 registration + R3 stats
  are the islands asks). `POST /llmux/grok` exists for the TUI dashboard/CLI;
  an islands settings row for BOTH providers is future work.
- MODIFIED (implementation, 2026-07-14): grok raw-io/trace rides the same
  `codex-trace.jsonl` file (T3 §7 "grok tag" = the model field identifies the
  provider); a separate file would fork the trace reader for no diagnostic
  gain.
- ADDED (implementation, 2026-07-14): scheduler idle probe explicitly skips
  grok accounts (both the orchestrator gate and the prober backstop) — grok
  has no quota surface, so `probe_eligible`'s no-window test would re-probe
  forever (spec §R3 corollary).
