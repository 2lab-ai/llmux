# Codex frontend vertical traces
Status: in-progress
Date: 2026-10-07

## T1 — launch and native Codex roundtrip

0. Client: `run --codex` resolves endpoint, ensures local daemon if applicable,
   fetches catalog and launches Codex. `-m` and `-c` remain user-controlled;
   text/tool SSE reaches actual Codex, whose tools execute in its client sandbox.
1. API: POST `/v1/responses` (alias `/responses`), existing loopback/key auth.
2. Input: JSON object, model and input, optional stream; reject invalid shape,
   unsupported stored-response references and oversized bodies before dispatch.
3. Flow: Request.model → routing classifier → AccountLease → Codex model resolver;
   Request.input/tools/reasoning → native upstream body; leased token → upstream
   Authorization. Existing refresh/429 retry/taxonomy remain shared.
4. Effects: account usage/windows, activity/key usage records. No Codex config edits.
5. Errors: malformed 400, oversized 413, absent pool 404, exhausted 429,
   upstream failure 502; stream failure emits a terminal Responses failure.
6. Output: 200 Responses SSE or aggregated response JSON; input/output/cache
   usage equals upstream evidence. Disconnect drops upstream and account lease.
7. Observe: requested endpoint origin differs from actual served backend.

## T2 — Claude SDK model and external tool loop

0. Client: Codex selects a Claude catalog model and sends its conversation/tools.
1. API/auth: identical T1 ingress and gate.
2. Input: Responses messages/tool calls/outputs → ordered Messages transcript;
   image content preserved; unsupported shapes fail with field paths.
3. Flow: Request.model → Claude catalog alias → Claude pool lease → Agent SDK
   query with leased OAuth/API credential. Request.instructions → SDK system;
   Request.tools → external tool definitions; SDK tool requests → Responses
   function calls → Codex execution → next Request.function_call_output → SDK.
   SDK auth/settings/cwd are isolated; inherited llmux proxy/auth env is removed.
4. Effects: shared account/tenant usage and SDK request-lifetime resources only.
5. Errors: SDK unavailable is explicit actionable failure; no HTTP fallback to
   Anthropic. Auth/429 use shared taxonomy, terminal SDK failures are reported.
6. Output: Responses created/item/text/function-argument/done/completed events,
   stable call ids, real usage. Cancellation terminates bridge and SDK children.
7. Observe: activity endpoint=open_ai even when served backend=claude.

## T3 — activity endpoint coloring

See [activity trace](../../docs/codex-frontend/activity.md). Request URI → Endpoint
→ RequestStarted/Finished → ActivityRow → persisted history/dashboard → dark gray
row background. Older rows default to Anthropic. Errors/cancellations retain origin.

## File map / implementation status

| Files | Contract | Status |
|---|---|---|
| src/cli/{mod,run}.rs | T1 launch/catalog | pending |
| src/proxy/{forward,sse,server,mod}.rs | T1/T2 shared transport | pending |
| src/proxy/responses.rs | T1/T2 protocol boundary | pending |
| src/provider/{mod,codex,claude_sdk}.rs, bridge/ | native Codex / SDK | pending |
| src/tui/{event,activity,view,ui,mod}.rs, src/dashboard.rs | T3 origin | pending |
| tests/ + module tests | contract verification | pending |
| README.md, docs/{operational-reference,provider-compatibility}.md | user docs | pending |

## Trace deviations

2026-10-07 MODIFIED: SDK replay uses public sessionStore/resume; tool-result final
turn is materialized before resume with interrupted-turn continuation, because
actual SDK tests proved a fresh prompt loses tool results. External execution remains Codex-owned.
ADDED: native additional_tools/namespace/custom-item preservation and collected
output_item.done aggregation, based on Codex0.160.1/live gateway receipts.
ADDED: text.format JSON Schema → SDK outputFormat → validated JSON text; the SDK
internal StructuredOutput tool is consumed internally, never sent as a client tool.
MODIFIED: Claude SDK preserves the catalog [1m] model suffix so SDK emits the
required context beta (actual SDK test), unlike raw Messages alias normalization.
ADDED: OpenAI request/response raw capture occurs outside the final protocol
adapter; SDK leg is labeled SDK transport rather than private vendor HTTP.
