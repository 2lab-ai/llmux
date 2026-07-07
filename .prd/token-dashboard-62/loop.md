# token-dashboard-62 — convergence loop

SSOT: ./ssot.md (frozen) · Repo: 2lab-ai/llmux (`~/2lab.ai/llmux`) · Channel: chat · Started: 2026-07-04 (afternoon KST)

Base ref for all measurements/work: `origin/master` @ `4cc97ac` (fable branches already merged — ssot.md Tensions #1).

## Plan (current round — R1)

- W1 (=S1): Rust additive fields — `ModelUsageDoc.cost_usd` per-row + `ClientUsageDoc.cost_usd`/`last_seen_ms` (U17 default-adopt) + stale docstring fix + TUI `model_cost()` → doc-field switch + backcompat tests → rows U14, U17, U16(part) → executor: resolver
- W2 (=S2): `DashboardDoc.data_quality` + TUI rendered labels ("≈ API-equivalent" + scope label) → rows U18, U20(TUI), U22(TUI), U16(part) → executor: resolver, **stacked on W1 branch** (same files: dashboard.rs, ui.rs)
- W3 (=S3): Islands contract — DTO 13종 + `dashboard()` + `refresh()` switch + status-fallback + build/test gate (scheme+test target) → rows U1-U6, U27(part) → executor: resolver, **parallel with W1/W2** (Swift files, no overlap)
- W4 (=S4): Islands analytics UI (cards/top models/heat/activity/banner/sheets + v1 rules + labels) → rows U7-U13, U19(Islands), U20(Islands), U21(Islands), U22(Islands) → after W3
- W5 (=S5): verification sweep (gates + manual parity + fallback + no-content check) → rows U23-U28 재검증 → after W1-W4

## Gap matrix (canonical cross-session state)

| ID | Source | Universe Item | Observable Acceptance | AS-IS Evidence | Status | Gap | Dispatch | Verification Evidence | Last Measured |
|---|---|---|---|---|---|---|---|---|---|
| U1 | gist-02 L15-28 | LlmuxDashboard DTO 13종 | DTO decode tests green | was: 0 hits | ✅ | — | W3 | PR #65 LlmuxDashboard.swift(407L) + 11 decode tests, real-wire fixture; W5 re-run 22 tests 0 fail (orchestrator opened u27 log) | R2 final |
| U2 | gist-02 L29 | LlmuxClient.dashboard() | method + used by refresh | was: status() only | ✅ | — | W3 | PR #65 LlmuxClient.swift +9; U28 harness exercised it live | R2 final |
| U3 | gist-02 L30 | refresh() → /llmux/dashboard | code path + tests | was: status only | ✅ | — | W3 | IslandUsageModel.swift:81-91 dashboard-first; parity harness decoded live capture | R2 final |
| U4 | gist-02 L31 | account tile behavior preserved | tiles identical post-switch | fable fields risk | ✅ | — | W3 | statusRecord bridge + testStatusRecordBridgePreservesTileFields green; snapshot shows tiles w/ 5h/7d/Fab gauges (orchestrator viewed) | R2 final |
| U5 | gist-02 L32 | publish 5 dashboard @Published | present + populated | was: none of 5 | ✅ | — | W3 | PR #65 6 new @Published; apply() publishes totals verbatim (IslandUsageModel.swift:109) | R2 final |
| U6 | gist-02 L33 | decode-fail → status fallback | fault-injection no-crash | was: absent | ✅ | — | W3 | W5 U28: mock 404 dashboard → status 200 fallback, 12 accounts, exit 0; + 4 decode-failure tests | R2 final |
| U7 | gist-02 L54-58 | summary cards ×4 | screenshot | was: absent | ✅ | — | W4 | usage-anon-off.png: 36.6k req / 110M tok / $8.7k "API-equivalent estimate" / 2.8% err (orchestrator viewed); math DashboardAnalytics.swift:82 | R2 final |
| U8 | gist-02 L59 | top models | screenshot | was: absent | ✅ | — | W4 | snapshot: top-3 rows w/ group badges (orchestrator viewed) | R2 final |
| U9 | gist-02 L60 | 24h/72h heat strip | screenshot | was: absent | ✅ | — | W4 | snapshot: 24h/72h strips + "best effort" caption (orchestrator viewed) | R2 final |
| U10 | gist-02 L61 | recent activity list | screenshot, metadata only | was: absent | ✅ | — | W4 | usage-tab-overview.png: 200/502/note/in-flight rows, tokens/cost/duration only (orchestrator viewed) | R2 final |
| U11 | gist-02 L62 | health warning banner | screenshot | was: absent | ✅ | — | W4 | snapshot: "1 account auth failed" banner (orchestrator viewed); DashboardHealth single source | R2 final |
| U12 | gist-02 L63-64 | model/account detail | in-panel expansion (notch sheets unreliable — IslandUsageView.swift:107) | was: absent | ✅ | — | W4 | UsageAnalyticsViews.swift:229/:429 expansion; chevrons visible in snapshot; intent satisfied — justification recorded | R2 final |
| U13 | gist-01 §4 | v1 rules: closed-island fmt+tabs, warn color, cache —, (group,model) | screenshots + code | was: mascot+counts | ✅ | — | W4 | label-c2x1-warning.png `⚠ C:2 X:1 $0.42` amber + crab kept (orchestrator viewed); 4 tabs in snapshots; gpt-5.5 CODEX/CLAUDE rows separate (viewed); DashFormat nil→— | R2 final |
| U14 | gist-02 L80-81 | ModelUsageDoc.cost_usd + populate | field+tests+TUI uses | was: absent + stale docstring | ✅ | — | W1 | PR #64: field + per-row populate + 5 tests green; docstring fixed; TUI prefers server value w/ old-doc fallback | R2 final |
| U15 | gist-02 L82 | GlobalTotals.cost_usd = Σ rows | field+sum+test | pre-existing | ✅ | — | — | dashboard.rs:676,:970-984 + S1 invariant test totals=Σrow | R2 final |
| U16 | gist-02 L83-84 | backcompat incl. new fields | tests green | was: email_anonymous only | ✅ | — | W1+W2 | doc_without_cost_fields_parses_to_zero_defaults + doc_without_data_quality_field_parses_to_canonical_labels green | R2 final |
| U17 | gist-01 L331-338 | ClientUsageDoc.cost_usd+last_seen_ms | fields+tests (Recommended-additive) | was: absent | ✅ | — | W1 | PR #64: fields wire-ready, serde defaults, tests. Values 0 — hub lacks per-client model attribution (activity.rs:711/:123-131); attribution = #32 out-of-scope; UI renders 0 as `—` (clients tab snapshot, orchestrator viewed) | R2 final |
| U18 | gist-01 §7 | DashboardDoc.data_quality | field+backcompat+both surfaces read | was: absent | ✅ | — | W2 | PR #66: DataQualityDoc, Default=canonical, exact-bytes test; Swift optional decode + byte-identical fallback (orchestrator diffed both constant blocks) | R2 final |
| U19 | gist-01 L355/543 | windowed "best effort" both surfaces | render+test / screenshot | TUI only | ✅ | — | W4 | TUI ui.rs:2527+test; Islands caption in snapshot (orchestrator viewed) | R2 final |
| U20 | gist-01 L1000 | cost label rendered both surfaces | render+test / screenshot | comments only | ✅ | — | W2+W4 | TUI stats title `$ ≈ API-equivalent estimate` + render test; Islands cost card caption (orchestrator viewed) | R2 final |
| U21 | gist-01 L1001 | cache missing → `—` both surfaces | code+screenshot | TUI only | ✅ | — | W4 | TUI opt_count; Islands DashFormat nil→`—`, clients `— —` (orchestrator viewed) | R2 final |
| U22 | gist-01 L998 | scope label both surfaces | render+test / screenshot | 0 hits | ✅ | — | W2+W4 | TUI models titles ×2 + test; Islands "top models — hydrated activity/runtime" (orchestrator viewed) | R2 final |
| U23 | issue AC1 | TUI renders older docs | old-doc tests incl. new fields | narrow test only | ✅ | — | W1/W2 | 3 old-doc parse tests (email_anonymous + cost fields + data_quality) all green in final `just check` | R2 final |
| U24 | issue AC4 | TUI↔Islands totals parity | live-daemon comparison | not measurable pre-W3 | ✅ | — | W5 | live capture totals {37275 req / 80,026,876+29,942,232 tok / 1053 err / $8778.34} == Islands DTO+model pipeline output, all 5 fields; TUI copies doc.totals verbatim (view.rs:322-329), both costs server-computed | R2 final |
| U25 | issue AC5 | no prompt/response content | field-list + UI sweep | to verify | ✅ | — | W5 | CompletedDoc/InFlightDoc field lists = counts/metadata only (dashboard.rs:882-918); note text = daemon telemetry (all call sites quoted); S4 grep clean; snapshots show metadata rows (orchestrator viewed) | R2 final |
| U26 | issue AC6 | just check | exit 0 | to run | ✅ | — | W5 | fresh run exit 0 @ S2 head e648b68 (orchestrator opened u26 log tail) | R2 final |
| U27 | issue AC7 | xcodegen+xcodebuild (+test target) | green | no scheme/tests existed | ✅ | — | W3+W5 | scheme+LlmuxIslandsTests added (PR #65); final run: 22 tests 0 failures, TEST SUCCEEDED @ S4 head b723ada (orchestrator opened u27 log tail) | R2 final |
| U28 | gist-02 L115 | old-daemon fallback no-crash | manual vs status-only daemon | not testable pre-W3 | ✅ | — | W5 | real-capture mock (404 dashboard/200 status) through app's actual client+model code: fallback OK, no crash, exit 0 | R2 final |

- Status: ✅ met · 🟡 partial (treated as 🔴 for branching, planning, and stuck-row counting; re-measured fully met → ✅ on evidence, closing while still partial requires user-approved justification) · 🔴 to work · 🚪 gated · ⚫ out of scope (recorded justification + user approval; never orchestrator-assigned)
- Accounting (final): universe 28 + appended 0 = 28✅ + 0🟡 + 0🔴 + 0🚪 + 0⚫ = 28 ✓ — loop CLOSED 2026-07-04
- Evidence ≥1 of: screenshot / route / API req+res / command output / log excerpt / commit / PR / artifact path — bare claims are not evidence

## Round log (append-only)

- R1 2026-07-04 · measured 2✅ 3🟡 23🔴 0🚪 0⚫ @ origin/master 4cc97ac · pre-gate resolutions: fable-merge decision RESOLVED (already merged), ClientUsageDoc fields ADOPTED by default · targets: W1(S1)+W3(S3) parallel → W2(S2) stacked on W1 → then R2: W4, W5 · result: (in progress)
  - W1 DONE → PR #64 `feat/62-s1-rust-cost-fields` @60931b2, `just check` exit 0 (609+38 tests), 5 new tests. ClientUsageDoc emitted wire-ready 0 (hub `ActivityLog.clients: HashMap<String,Totals>` activity.rs:711/:123-131 lacks per-model attribution + last-seen; #32 out-of-scope) → U17 accept-with-note; W4 renders 0 as `—`.
  - W2 DONE → PR #66 `feat/62-s2-data-quality` @e648b68, base=S1 branch (stacked), `just check` exit 0 (613+38). Labels rendered: MAIN models strip `hydrated activity/runtime`; stats overlay `… — $ ≈ API-equivalent estimate`. Backcompat exact-bytes test green.
  - W3 DONE → PR #65 `feat/62-s3-islands-contract` @a323bb0, xcodegen scheme 신설(기존 scheme 부재 확인), xcodebuild BUILD/TEST SUCCEEDED (11 DTO decode tests, 실 daemon wire fixture, 이메일 scrub 확인). Fallback = any-throw → verbatim status path + analytics clear.
  - W4 dispatched (stacked on S3): PR pending.
  - W5 note: parity(U24)는 현 daemon(0.2.14, /llmux/dashboard 있음) 상대 totals로 검증 가능; data_quality/cost_usd 부재는 양표면 fallback 경로가 canonical 문자열/재계산으로 커버 — 그 경로 자체가 U23/U28 증거. daemon 재시작(신규 바이너리) 불필요·유저 게이트라 안 함.
- R2(final) 2026-07-04 · re-measured **28✅ 0🟡 0🔴 0🚪 0⚫** · W4 DONE → PR #67 @b723ada (dispatcher waived resolver ~400-LOC bound: spec-sized slice, waiver in PR body; 22 tests, 8 snapshots, running island untouched) · W5 DONE → U24 parity 5필드 일치(live capture $8778.34…), U28 mock-404 fallback no-crash, U25 field-list+grep clean, U26/U27 게이트 green(오케스트레이터 로그 직접 열람) · **LOOP CLOSED** — ship 잔여는 유저 소유: merge train #64→#66, #65→#67 → release/deploy
- R3(ship) 2026-07-04 · user re-goal "점검해주고 마무리해줘" → 점검: 4 PR 모두 MERGEABLE/CLEAN + CI(macos/ubuntu) pass, master 4cc97ac 불변 · agent 머지 시도는 auto-mode 분류기 차단 → AskUserQuestion으로 명시 승인 획득("승인 — 네가 머지해") · 머지 트레인 완료 (repo 컨벤션 merge-commit + `merge: #NN` 제목): 074d907(#64 S1) → d71be09(#66 S2) → 8a5844a(#65 S3) → 0b57821(#67 S4) · master HEAD 0b57821: CI success + Preview success → prerelease `preview-2026-07-04-1216-0b5782170945` 발행 확인 (중간 커밋 run cancelled = 동시성 대체, 정상) · daemon 재시작/tap bump는 범위 외(별도 유저 지시 시) · **SHIP CLOSED**
- Follow-ups (user-facing, non-blocking): ① rolling-upgrade 창에서 구 doc의 모델 cost가 TUI(로컬 재계산)/Islands(`—`) 비대칭 — 의도됨, 배포로 소멸 ② error-note message가 upstream 에러 스니펫을 실을 가능성(pre-#62 기존 동작) — AC5 엄격 해석 시 별도 이슈 후보 ③ closed-pill 접두어 "Llmux Islands" vs gist 예시 "llmux" — 코스메틱 편차, PR #67 본문 기재 ④ ClientUsageDoc 값 채움은 #32(클라이언트 귀속) 진행 시

## Dispatch format (one work unit, self-contained)

> Goal: <work unit>. Spec basis: <universe IDs + quotes>. Repo/branch: <...>.
> Observable acceptance: <...>. Known state/evidence: <...>. Hypotheses: [가설] <...>.

## Channel report format

- Kickoff: start <ts> · universe N · a✅ b🟡 c🔴 d🚪 e⚫ · round targets
- Round: elapsed <t> · delta since last round · next targets
- Final: start→end · disposition of every row · gated remainder · follow-ups

## Notes / gotchas (carried from plan.md + measurement)

- Open PR #47 (spec/batch-b, 33 additive tests) may touch the same test files as W1/W2 — brief executors to expect rebase, not to fight it.
- dashboard.rs:959-964 docstring falsely claims ModelUsageDoc can't gain fields (ui.rs literals are test-only) — W1 fixes docstring.
- All ModelUsageDoc{} literal sites at 4cc97ac: dashboard.rs:985(prod),:1045(prod), tui/mod.rs:2043(test), ui.rs:2781(test),:3840(test) — compiler catches, but brief lists all 5.
- llmux ship actions (merge/release/daemon restart) are user-owned — every W ends at review-ready PR.
- Islands notch sheets unreliable (IslandUsageView.swift:107) — U12 may land as in-panel detail; that satisfies intent, note in W4 brief.
