# Activity endpoint origin

Status: shipped

User requirement (2026-10-07):
> (추가로) activity에 배경색으로 claude 엔드포인트 요청인지, codex 요청인지 처리 (openai endpoint 요청은 배경을 어두운 회색으로 표시)

## Vertical trace

0. Client surface: the activity panel uses dark gray for requests received at the OpenAI Responses endpoint. Anthropic endpoint requests retain their existing background. Backend model, account, and provider do not decide this color. Expanded details, in-flight requests, errors, and grouped rows retain this distinction.
1. API entry: incoming `/v1/responses` (and `/responses`) maps to `Endpoint::OpenAi`; existing Anthropic paths map to `Endpoint::Anthropic`, before body parsing or provider routing.
2. Input: endpoint origin is server-derived, never a client body field. Invalid bodies retain their endpoint origin.
3. Layer flow: incoming endpoint → `ActivityEvent::{RequestStarted,RequestFinished}.endpoint` → `InFlight.endpoint` / `CompletedBody::Request.endpoint` → `PersistedRequest.endpoint` and `InFlightDoc.endpoint` / `CompletedDoc::Request.endpoint` → attached `DashboardView` → activity row background. `RequestRouted` changes backend identity without changing origin. A finish without a start is self-contained.
4. Side effects: finished activity JSONL records persist endpoint using `anthropic` / `open_ai`. Existing records and dashboard documents missing endpoint default to Anthropic. Count-row grouping includes origin.
5. Error paths: pre-routing failures, upstream errors, aborted streams, and disconnect completions carry the origin through the same event and document path.
6. Output: OpenAI activity rows have RGB(40,40,40) background across their full rendered width; foreground status/model colors and expand markers remain readable. Anthropic rows and unrelated notes retain existing rendering.
7. Observability: tests inspect rendered ratatui cells, event lifecycle, persistence replay, and dashboard JSON round-trip. Source files: `src/tui/{event,activity,view,ui,triage,mod}.rs`, `src/dashboard.rs`, emitters in `src/proxy/forward.rs`.

## Google endpoint origin (`llmux run --agy`, #316)

A third origin joined on 2026-11-21: requests received at the Google Gemini API
ingress (`/v1beta/models/…`, the Antigravity CLI `agy`) map to `Endpoint::Google`,
persisted and sent on the wire as `google`. The rule is the OpenAI one:

- The origin comes from the request path, never the body, the model or the backend
  that served it. A `gpt-6.2-sol` request from agy is a Google row served by codex.
- Google rows get their own subtle tint, dark slate RGB(28,36,52) (`#1c2434`),
  across the full row width in the TUI, and on the macOS and Linux Islands activity
  lists. OpenAI rows keep RGB(40,40,40); Anthropic rows and notes stay untinted.
- Every native site is a three-way choice, not an "is it OpenAI" test: the Swift
  row model carries `ActivityEndpointOrigin` (`anthropic` / `openAI` / `google`), the
  QML receipt card picks one of three fills. A Google row is never styled or counted
  as Anthropic.
- Count-row folding keys on the origin, so Google rows never fold into Anthropic or
  OpenAI rows. The per-model endpoint breakdown buckets a Gemini path by its method
  (`streamGenerateContent`, `generateContent`), not by the model in the path.
- The shared-core schema enum is `anthropic | open_ai | google`, still defaulting to
  Anthropic for records that predate the field.

Tests: `google_endpoint_rows_take_their_own_tint` (TUI render buffer),
`count_runs_never_mix_endpoint_origins`, `endpoint_origin_survives_dashboard_wire_and_attached_view`,
`endpoint_origin_survives_routing_finish_and_persisted_replay`,
`endpoint_class_buckets_count_tokens_messages_and_other`,
`activity_google_endpoint_origin_reaches_native_receipts` (Islands core),
`testActivityEndpointBackgroundDoesNotFollowProvider` (Swift),
`activity_endpoint_background_is_independent_of_backend_provider` (Linux QML source contract).

## Native Activity surfaces

The same endpoint field flows from `DashboardDoc` into shared-core `ActivityReceipt.endpoint`, then through the macOS Swift decoder/projections/`ActivityRowModel` and Linux QML receipt cards. Both native recent-activity lists use RGB(40,40,40) for OpenAI requests, independent of provider; notes and missing-origin legacy rows keep their previous backgrounds. Shared-core schema remains additive with a default Anthropic origin. Verification includes core fixture round-trip, Swift row-model decoding, macOS build/tests, and Linux source-contract tests (Linux graphical runtime requires a Linux host).

## Verification (2026-10-07)

- Six focused Rust tests pass: `endpoint_origin_survives_routing_finish_and_persisted_replay`, `endpoint_origin_reaches_dashboard_document_from_live_events`, `endpoint_origin_survives_dashboard_wire_and_attached_view`, `count_runs_never_mix_endpoint_origins`, `endpoint_origin_colors_rendered_activity_rows_and_expanded_errors`, `endpoint_origin_colors_folded_count_header`.
- Observed `CompletedFrame` buffer: at width 160, in-flight row y=1 and expanded OpenAI error rows y=2…9 have RGB(40,40,40) across x=0…159; Anthropic→Codex row y=10 retains Reset. Error status remains red. Folded OpenAI count header is dark gray. The completed frame is authoritative because backend diffs omit wide-glyph continuation cells.
- All 349 TUI tests pass. Shared Islands core: 49 tests pass, including `activity_endpoint_origin_reaches_native_receipts_independently_of_provider` and schema checks.
- Native macOS app builds; all 104 Swift tests pass, including `testActivityEndpointBackgroundDoesNotFollowProvider`. This is build/projection evidence, not native screen-pixel evidence.
- Linux `statistics_surface` contract suite: 9 tests pass without default GUI features. Linux graphical runtime was not executed on this macOS host.
- Session receipts: `/tmp/llmux-activity-render.log`, `/tmp/llmux-activity-tui-suite.log`, `/tmp/llmux-activity-core.log`, `/tmp/llmux-activity-xcode.log`, `/tmp/llmux-activity-linux.log`. Published-preview and installed-client receipts are recorded in the
  [feature convergence loop](../../.prd/codex-frontend/loop.md#round-6--corrected-preview-and-installed-client).
