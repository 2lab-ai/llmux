# Activity endpoint origin

Status: in-progress

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

## Native Activity surfaces

The same endpoint field flows from `DashboardDoc` into shared-core `ActivityReceipt.endpoint`, then through the macOS Swift decoder/projections/`ActivityRowModel` and Linux QML receipt cards. Both native recent-activity lists use RGB(40,40,40) for OpenAI requests, independent of provider; notes and missing-origin legacy rows keep their previous backgrounds. Shared-core schema remains additive with a default Anthropic origin. Verification includes core fixture round-trip, Swift row-model decoding, macOS build/tests, and Linux source-contract tests (Linux graphical runtime requires a Linux host).

## Verification (2026-10-07)

- Six focused Rust tests pass: `endpoint_origin_survives_routing_finish_and_persisted_replay`, `endpoint_origin_reaches_dashboard_document_from_live_events`, `endpoint_origin_survives_dashboard_wire_and_attached_view`, `count_runs_never_mix_endpoint_origins`, `endpoint_origin_colors_rendered_activity_rows_and_expanded_errors`, `endpoint_origin_colors_folded_count_header`.
- Observed `CompletedFrame` buffer: at width 160, in-flight row y=1 and expanded OpenAI error rows y=2…9 have RGB(40,40,40) across x=0…159; Anthropic→Codex row y=10 retains Reset. Error status remains red. Folded OpenAI count header is dark gray. The completed frame is authoritative because backend diffs omit wide-glyph continuation cells.
- All 349 TUI tests pass. Shared Islands core: 49 tests pass, including `activity_endpoint_origin_reaches_native_receipts_independently_of_provider` and schema checks.
- Native macOS app builds; all 104 Swift tests pass, including `testActivityEndpointBackgroundDoesNotFollowProvider`. This is build/projection evidence, not native screen-pixel evidence.
- Linux `statistics_surface` contract suite: 9 tests pass without default GUI features. Linux graphical runtime was not executed on this macOS host.
- Session receipts: `/tmp/llmux-activity-render.log`, `/tmp/llmux-activity-tui-suite.log`, `/tmp/llmux-activity-core.log`, `/tmp/llmux-activity-xcode.log`, `/tmp/llmux-activity-linux.log`. Overall feature release verification remains owned by the feature workstream.
