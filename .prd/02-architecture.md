# llmux — Architecture

Rust, edition 2021. Single binary, tokio multi-thread runtime. Module layout follows herdr's
state/runtime separation: scheduler decisions are pure functions over snapshots, runtime state is
folded into dashboard documents, and provider-specific format conversion is isolated from the
Anthropic passthrough fast path.

## Crate layout

```text
src/
  main.rs              # entry, tokio runtime, command dispatch
  cli/                 # clap commands + command impls
    mod.rs             # server/dashboard/run/stop dispatch, daemon attach decision
    daemon.rs          # probe/spawn/wait/stop helpers (herdr-style client/server)
    login.rs import.rs run.rs status.rs accounts.rs api.rs env.rs
  config/              # ~/.config/llmux.json load/save, atomic read-merge-write
    mod.rs schema.rs migrate.rs
  auth/
    oauth.rs           # Claude PKCE flow, token exchange, refresh (coalesced)
    codex.rs           # Codex auth.json import + OpenAI token refresh
    profile.rs         # /api/oauth/profile client (email, uuid, tier)
    credentials.rs     # ~/.claude/.credentials.json import
  scheduler/
    mod.rs             # AccountPool: owns state, applies events, leases
    select.rs          # PURE: eligibility + ranking + selection_order + blocking_reason
    window.rs          # QuotaWindow {utilization, resets_at, fetched_at, source}
    headers.rs         # Anthropic unified + Codex x-codex-* header parsing
    usage.rs           # /api/oauth/usage poller (Claude OAuth only, backoff ladder)
  proxy/
    server.rs          # axum listener, /llmux/* control endpoints, background tasks
    forward.rs         # request rewrite, provider dispatch, retry taxonomy, refresh choke point
    sse.rs             # passthrough + transform relay; SseTransform trait
    logging.rs         # optional request logs, credential masking
  provider/
    mod.rs             # Provider trait + UnifiedRequest/Response types
    anthropic.rs       # passthrough impl (identity hooks, zero-copy fast path)
    codex.rs           # Anthropic Messages <-> OpenAI Responses translation + SSE converter
    stubs.rs           # gemini/local compile-checked drafts
  dashboard.rs         # DashboardHub + DashboardDoc (/llmux/dashboard contract)
  key_usage.rs         # durable per-tenant keys usage: SQLite store (bundled), exact
                       # rolling-window + model-filter queries, one-time activity.jsonl
                       # import, GET /llmux/keys/usage document
  tui/
    mod.rs             # local + remote dashboard loops, attach client
    view.rs            # DashboardView: single render input from live state or DashboardDoc
    ui.rs              # ratatui renderer, no fork between local/attach
    activity.rs logs.rs format.rs event.rs
  build_info.rs        # channel + build id from env (herdr pattern)
tests/
  e2e.rs               # mock upstream + proxy acceptance scenarios
  mock_upstream.rs     # Anthropic/Codex simulators (headers, 429, SSE)
```

## Runtime topology

```text
Claude Code
  │ ANTHROPIC_BASE_URL=http://localhost:3456
  ▼
llmux daemon (axum)
  ├─ AccountPool (scheduler state, windows, leases)
  ├─ Provider dispatch
  │   ├─ AnthropicPassthrough → https://api.anthropic.com
  │   └─ CodexProvider       → https://chatgpt.com/backend-api/codex/responses
  ├─ Background tasks
  │   ├─ usage poller (Claude OAuth accounts)
  │   ├─ token refresh pass (< refresh_ahead_secs remaining)
  │   ├─ scheduler re-evaluation tick
  │   └─ DashboardHub fold (activity/log/poller/switch state)
  └─ Control API
      ├─ GET  /llmux/status
      ├─ GET  /llmux/dashboard
      ├─ POST /llmux/switch
      └─ POST /llmux/shutdown
```

`llmux run` is a client command: it probes `/llmux/status`, spawns a detached daemon with
`server --no-tui` if none is running, waits until ready, then launches `claude` with
`ANTHROPIC_BASE_URL`. `llmux dashboard` is an attach client: it polls `/llmux/dashboard`
and renders the same ratatui layout without binding the proxy port.

## Concurrency model

- `AccountPool` is behind `Arc<RwLock<PoolState>>`; mutations go through event methods
  (`record_headers`, `record_429`, `record_usage`, `switch_to`) that re-validate preconditions
  before applying.
- In-flight requests hold an `AccountLease` (Drop-based guard incrementing/decrementing a
  per-account counter). Switching away never cancels leased requests; the lease pins the
  credential clone for the request lifetime.
- Claude OAuth refresh uses `RefreshCoalescer`: concurrent refresh callers for the same account
  await the same outcome.
- Codex refresh uses the OpenAI refresh-token grant; it is not coalesced in v0.1 because it is
  rare and idempotent enough for the single-user daemon.
- Config writes are read-merge-write and atomic. Refresh updates include `last_refresh_ms` so the
  UI can prove the daemon is actually maintaining credentials.
- DashboardHub is the single fold target for activity/log/poller/switch events. Both local TUI and
  remote attach render from `DashboardView`, so layout logic is not duplicated.

## Scheduler data flow

```text
Anthropic response headers ──┐
Codex x-codex-* headers ─────┼──> headers.rs ─────┐
/api/oauth/usage poll ───────┘                     ├──> PoolState.windows
429 + retry-after ─────────────────> forward.rs ───┘          │
                                                               ▼
                         select.rs::pick(snapshot, now) — pure, deterministic
                         1 gates: health, cooldown, thresholds, staleness
                         2 stickiness + perishability override (SWITCH_MARGIN)
                         3 rank: max score (servable×urgency) → min 5h → min 7d reset → id
                                                               │
                         switch_to(expected_current, target)  # CAS-ish, lease guard
```

Two evidence sources feed the same Claude windows; freshest `fetched_at` wins per window. Headers
are authoritative during traffic; the poller covers idle Claude OAuth accounts. If usage data is
stale, a Claude account is ineligible unless all accounts are stale (headers-only fallback). Codex
has no usage poller, so staleness does not gate it; quota thresholds still gate Codex when
`x-codex-*` header evidence exists.

## Provider dispatch and request flow

1. Buffer incoming request body and create an activity item.
2. Acquire an `AccountLease`. When `routing.enabled` (see `routing.rs`), the request's
   `model` first selects a backend **group** (claude vs codex; the model field — previously
   only carried through `UnifiedRequest` as a future routing key — now drives selection), the
   scheduler is filtered to that group, and the lease is sticky per group. The leased
   credential then determines the provider:
   - `oauth` / `apikey` → `AnthropicPassthrough`.
   - `codex` → `CodexProvider`.

   Routing is **on by default**, so the `model` normally selects the group. With routing disabled
   no group filter is applied: a single legacy current slot is used and codex becomes the
   cross-group overflow pool — the older behavior.
3. For the served Codex/Grok group, validate Messages input and compatibility policy before
   credential refresh or upstream traffic. `validate_request` returns typed `InvalidRequest`
   (local HTTP 400) or a sorted/deduplicated compatibility report. Strict policy rejects any
   report issue. Valid text/tool counts return the local labeled estimate here; image counts
   fail locally. Anthropic/OpenRouter request paths retain their existing behavior.
4. Refresh credential if near expiry; on one 401, force refresh and retry once. Build request:
   - Anthropic: identity body, inject Bearer or x-api-key.
   - Codex/Grok: shared Messages→Responses translation with an explicit `ResponsesFlavor`
     argument alongside `RequestPlan`, plus adapter-owned auth/model/effort. Builder validation also protects
     direct callers; only the HTTP layer applies client strict policy and diagnostic headers.
5. Send upstream, classify response, and retry/switch according to taxonomy.
6. Relay response:
   - Anthropic: byte-identity SSE/body relay; usage observed from emitted Anthropic SSE.
   - Codex: Responses SSE transform relay; converter emits Anthropic SSE and usage accounting sees
     the emitted events.
7. Finish activity, record totals, update DashboardHub.

## Error taxonomy (forward.rs)

| Upstream signal | Action |
|---|---|
| 429 + retry-after | Park that account. If short, wait and retry same account; if long, switch and retry request. |
| 401 on refreshable account | Force one refresh, retry; second 401 marks auth_failed and switches. |
| 5xx / connect reset / timeout | Transient: return 502/close so client retries. |
| Persistent provider error | Mark account error or return provider-shaped error, depending on retryability. |
| Codex non-2xx | Wrap body as Anthropic error event/body; never relay raw Codex JSON to Claude Code. |
| Codex 2xx stream without content-type | Treat as SSE by contract. The live backend omits `content-type`; malformed streams terminate with Anthropic `error`. |

## Config schema (v1)

```jsonc
{
  "version": 1,
  "proxy": { "port": 3456, "api_key": "ta-..." },
  "upstream": "https://api.anthropic.com",
  "codex": {
    "upstream": "https://chatgpt.com/backend-api/codex",
    "token_url": "https://auth.openai.com/oauth/token",
    "default_model": "gpt-5.5",
    "fast": false
  },
  "scheduler": {
    "five_hour_max": 0.90,
    "seven_day_max": 0.99,
    "usage_poll_secs": 300,
    "usage_max_age_secs": 600,
    "refresh_ahead_secs": 25200
  },
  "routing": {            // model→backend-group routing; all keys default-able
    "enabled": true,     // default; false = Codex-as-overflow (no group filter)
    "claude_models": [],  // empty = builtin rules; non-empty replaces them
    "codex_models": [],
    "default_group": "claude",   // unmatched / model-less request lands here
    "on_empty_group": "error"    // "error" = 404 not_found_error; "fallback" = other group
  },
  "accounts": [
    { "name": "a@x.com", "type": "oauth", "account_uuid": "...",
      "access_token": "...", "refresh_token": "...",
      "expires_at_ms": 0, "last_refresh_ms": 0 },
    { "name": "api-1", "type": "apikey", "api_key": "..." },
    { "name": "chatgpt@example.com", "type": "codex", "account_id": "...",
      "access_token": "...", "refresh_token": "...",
      "expires_at_ms": 0, "last_refresh_ms": 0 }
  ]
}
```

`migrate.rs` reads teamclaude's `~/.config/teamclaude.json`; `credentials.rs` reads
`~/.claude/.credentials.json`; `auth/codex.rs` reads Codex CLI `~/.codex/auth.json`.

## Shared Codex / Grok translation details

`provider::responses` owns Messages→Responses validation/conversion and the reverse SSE state
machine; adapters own endpoint, credentials, model and effort resolution. `ResponsesFlavor`
(`Codex` / `Grok`) captures the actual subscription-gateway differences, not public-API parity.
The translator:
- folds top-level `system` and message-level system text into `instructions` (preserving the
  existing Codex-compatible policy; this is not a claim that xAI forbids system-role inputs);
- maps legal input roles to `assistant`, `developer`, or `user` and text to
  `input_text`/`output_text`;
- maps valid `tool_use` to `function_call`, and `tool_result` to `function_call_output` with an
  explicit error-text prefix for `is_error: true`;
- preserves valid user PNG/JPEG base64 images, including nested tool-result images, as
  `input_image` with a data-URI `image_url`; ordered multimodal tool outputs use content arrays;
- rejects URL images, invalid base64/MIME/size (>20 MiB decoded), forbidden-role images,
  unknown/unsupported blocks, malformed tool structures and nameless/server tools with field
  paths (no payload contents) in `InvalidRequest`, rather than dropping them;
- translates choices `auto`→`auto`, `any`→`required`, `none`→`none`, `tool{name}`→flat named
  function selector and inverts boolean `disable_parallel_tool_use`. Named tools must exist;
  `any`/`tool` need tools. Both providers' no-tools requests omit `tools`/`tool_choice`/`parallel_tool_calls`;
- omits Codex `max_tokens` with omission/warning `max_tokens`; forwards Grok `max_tokens` exactly
  as `max_output_tokens` with warning `max_tokens_semantics` (not an omission). Positive integer
  validation is not clamping, a total-budget guarantee, or a billing cap;
- rejects non-null `temperature`/`top_p`/`top_k` and nonempty `stop_sequences` with local 400
  rather than infer subscription support from public schemas. Empty stop sequences are
  vacuous; malformed values fail. Top-level `thinking` is shape-validated, then omitted with
  `thinking_config` in omissions/warnings (strict: 400), without enforcing `budget_tokens` or
  disabled reasoning. Count validation emits no inference-only omission warnings. This is
  an enumerated-controls contract, not an all-fields compatibility guarantee;
- omits prior assistant `thinking`/`redacted_thinking` with corresponding issues. Remaining
  transcript order survives; private reasoning does not. No ciphertext cache/replay or foreign
  signature conversion; those block types on other roles are invalid;
- sends `stream: true`, `store: false`, adapter-resolved model/effort, Codex-only priority tier
  and encrypted-reasoning include. Codex retains its stable `prompt_cache_key`; Grok omits the
  process-wide key because its routing scope is unproven (not an established leak).

The HTTP boundary consumes `CompatibilityReport { omitted_fields, warnings }`: absent or
`X-Llmux-Compatibility: compat` permits the enumerated losses; `strict` rejects any issue with
400 before refresh/network. Other policy values also fail. Successful responses carry relevant
`X-Llmux-Omitted-Fields` / `X-Llmux-Compatibility-Warnings` lists and structured WARN diagnostics
(provider/field list/request id). Headers are machine-readable; client UI display is not promised.
Local validated text/tool counts include serialized tools/property keys, retain a chars/4
heuristic (floor one), and set `X-Llmux-Token-Count: estimate`; image or malformed counts return
400 without refresh/network. The internal Codex idle probe explicitly builds a **no-cap** body,
not a one-token-budget promise or a client strict-policy request.

The response converter is a state machine over Responses SSE events:
- `response.created` → Anthropic `message_start`;
- text deltas → text blocks; reasoning summary deltas → thinking blocks (summaries, not replay);
- function call items/argument deltas → `tool_use` + `input_json_delta`; stable block identity
  uses upstream `output_index`/`item_id` plus `content_index`, preserving overlapping streams
  and out-of-order item completion rather than assigning every delta to the last block;
- completed output → `message_delta` + `message_stop`; malformed executable arguments on
  normal completion are protocol errors, and incomplete empty arguments are not repaired to `{}`;
- `response.incomplete`, or a completed envelope whose response status is incomplete, maps
  `max_output_tokens` to `stop_reason: max_tokens`, preserving partial text and reported usage;
- other incomplete reasons, `response.failed`, or malformed streams → Anthropic SSE `error`
  (aggregate JSON returns HTTP 502). Truncated tool JSON must not become executable `{}`;
- legitimate provider text/thinking/tool output is preserved, not heuristically scrubbed.
  Reported output usage is not clamped to the request limit or reduced by reasoning tokens to
  make the cap appear satisfied. The existing fresh/cache-read input split is independent.

See [the compatibility reference](../docs/operational-reference.md#codex--grok-compatibility-contract)
for the provider matrix, pinned official sources and 2026-09-11 synthetic endpoint receipts.
A single accepted fixture proves neither all-model support nor reasoning-budget equivalence.

## Control-plane auth

Control endpoints share the status endpoint's gate: loopback clients are exempt; non-loopback
clients need the generated proxy API key. This preserves local UX while avoiding unauthenticated
remote control if the user binds beyond localhost.

## Key dependencies

tokio, axum (server) + reqwest (upstream/streaming), serde/serde_json, clap, ratatui + crossterm,
tracing + tracing-subscriber, sha2/base64 (PKCE/JWT payload decode), thiserror, ulid, uuid, libc,
rusqlite (`bundled` — the keys-usage store compiles SQLite in, so no build or run host needs a
system libsqlite3 or a matching version).

### Durable keys usage (`key_usage.rs`, keys-history K)

The ONE database in the tree, deliberately scoped: per-tenant request metadata (`ts, tenant,
group, model, status, token counts`) in a single indexed table beside the config
(`~/.config/llmux[-preview]/usage.sqlite3`, `0600` in a `0700` dir; an explicit `$LLMUX_CONFIG`
derives an isolated sibling directory, which is also what keeps tests off the real file).

- **Write path.** The dashboard fold queues a row per `RequestFinished`; `record` is a bounded
  queue push, never disk IO, so no hub or pool lock is ever held across the database. One
  dedicated writer thread drains the queue into batched transactions. A full queue drops and
  counts (observability never applies backpressure); failures increment `errors`/`last_error`.
- **Read path.** `GET /llmux/keys/usage` and the in-process TUI run the SAME bounded
  `GROUP BY tenant, group, model` query on the blocking pool — cost is bounded by the ANSWER, not
  by the history size. Nothing is queried per frame and the document never rides on
  `/llmux/dashboard` (a 90-day answer must not be re-serialized every second).
- **Migration.** `activity.jsonl` stays the source of truth for every other surface. Its prefix is
  imported ONCE into the store, transactionally against a persisted byte offset, with each row
  keyed by the identity it would have been written live under — so restart, overlap with live
  appends, a torn trailing line, and log rotation are all handled without double counting.

## Porting pitfalls now codified

- SSE events fragment across chunks; both passthrough observer and Codex transform buffer correctly.
- Anthropic reset vs Codex reset timestamps differ; parse per source.
- Do not require `content-type: text/event-stream` for Codex 2xx; live backend omits it.
- Never emit `role:"system"` in Codex input.
- Mask credentials in logs; request logging is opt-in and capped.
- TTY detection: bind/probe happens before TUI init so bind errors never corrupt the terminal.
- Config writes must preserve concurrently refreshed tokens.
