# llmux #62 — Token usage dashboard (TUI + Islands) 구현 계획

- 상태: **v3 — 이중 리뷰 만장일치 APPROVE** (Claude 엔진 v1 APPROVE→v2 APPROVE / gpt-5.5 v1 REJECT(1 blocking: data_quality 게이트화)→v2 APPROVE; non-blocking 전건 반영). 구현 착수는 유저 결정 대기 (선행 결정 2건 포함).
- 작성: 2026-07-04, zbrain dispatcher (4개 점검 서브에이전트 + dispatcher 직접 팩트체크)
- SSOT: issue #62 본문 + gist 스펙(01-schema / 02-checklist) — **gist가 이슈 본문보다 상세하므로 충돌 시 gist 우선**
- 기준 커밋: `origin/master` `8eaa2bf` (v0.2.11)

## 한 줄 프레임

에픽의 Rust/TUI 절반(통계 계약·비용·히트맵·백컴팻 테스트)은 **이미 master에 배송돼 있다**.
진짜 남은 일은 ① Islands가 `/llmux/dashboard`를 읽게 하기, ② Islands 분석 UI, ③ Rust 추가 필드(`ModelUsageDoc.cost_usd` 등), ④ 데이터-품질 라벨 *렌더링*. 이슈 체크리스트를 액면 그대로 따르면 이미 된 일을 다시 하게 된다.

## Ground truth — Phase별 현재 상태 (origin/master 실코드 검증)

| Phase | 항목 | 상태 | 증거 (origin/master) |
|---|---|---|---|
| 1 | LlmuxDashboard DTO / client.dashboard() / refresh() 전환 / publish | **전부 MISSING** | Swift는 `/llmux/status`만 호출 (`LlmuxClient.swift:50`); `/llmux/dashboard`는 주석에만 존재 (`LlmuxStatus.swift:4`) |
| 2 | summary cards / top models / heat strip / activity / health banner / sheets | **전부 MISSING** | `IslandUsageView.swift` = 계정 타일 그리드만; `UsageDashboardPanel`(UsageTiles.swift:249)은 agent-island 유산 **dead code**(미참조) |
| 3a | `ModelUsageDoc.cost_usd` 추가 | **MISSING** | struct `dashboard.rs:617-644`에 필드 없음. L751 `cost_usd`는 `CompletedDoc::Request`(per-request)임 — 혼동 주의 |
| 3b | per-model cost를 pricing.rs로 계산 | **계산은 됨, 직렬화만 안 됨** | `model_usage_docs()`가 row별 cost 계산 후 합계만 저장; TUI는 `model_cost()`로 재계산해 `$` 컬럼 렌더 |
| 3c | `GlobalTotalsDoc.cost_usd` = model-row 합 | **DONE** | `dashboard.rs:610`, 합산 `:1156→:1200`, 테스트 `:1644+` |
| 3d | 백컴팻 직렬화 테스트 | **DONE (기존분)** | pre-Feature-D doc 파싱 테스트 `:1725-1731`, `email_anonymous` 백컴팻 `:1785+` — 단 **새 필드용 테스트는 신규 작성 필요** |
| 4 | windowed best-effort 라벨 | **DONE (TUI)** | 패널 타이틀 + qualifier 줄 + 렌더 테스트 존재 |
| 4 | cost "API-equivalent estimate" 라벨 | **MISSING (렌더)** | master `ui.rs`에 doc comment로만 존재, 화면 문자열 0건 (grep 검증) |
| 4 | cache 미제공 = `—` (0 아님) | **DONE (TUI)** | `Option<u64>` + `skip_serializing_if` + `opt_count()` `—` 렌더 |
| 4 | model-usage scope 라벨 ("hydrated activity/runtime") | **MISSING** | gist 4번째 라벨 — 이슈 본문엔 없음 |
| — | `DashboardDoc.email_anonymous` | **DONE** | `dashboard.rs:470` (stale 체크아웃엔 없어서 점검 에이전트가 없다고 봤음 — master엔 있음) |

## 선행 결정 2건 (유저 게이트 — 구현 착수 전 답 필요)

1. **fable 브랜치 머지 순서.** `feat/fable-usage`(+19, 재측정 필요) / `feat/islands-fable`(+1)이 미머지 상태로 Phase 1이 고칠 바로 그 파일들(`LlmuxStatus.swift`, `LlmuxClient.swift`, `IslandUsageModel.swift`, 타일)을 수정한다. **권장: 두 브랜치를 먼저 머지하고 #62를 그 위에서 시작** (둘 다 green, 0-behind). 아니면 충돌을 감수.
2. **`ClientUsageDoc.cost_usd` + `last_seen_ms` 추가 여부.** gist는 "recommended additive"(권장이지 minimum 아님), 이슈 본문 밖. **권장: 이번에 같이** (같은 additive 패턴, 별도 배포 아낌) — 단 scope 최소화가 우선이면 drop 가능.

> `DashboardDoc.data_quality`는 결정 사항이 **아니다** — gist §7이 minimum additive field로 지정하고 이 계획이 gist-우선을 선언했으므로 **필수 범위(S2)**. 라벨 문구의 SSOT를 서버 한 곳으로 둬야 TUI/Islands 두 표면의 semantics 일치(acceptance)가 구조적으로 보장된다. (v1→v2: gpt-5.5 리뷰 blocking 반영)

## Work breakdown — 5 slices (순서 고정, slice별 독립 PR 가능)

### S1 — Rust additive 필드 (Phase 3 마무리)
- `ModelUsageDoc`에 `#[serde(default)] pub cost_usd: f64` 추가; `model_usage_docs()`에서 row별 값 저장(이미 계산 중인 값).
- **낡은 제약 해제**: `model_usage_docs` docstring의 "ui.rs exhaustive literal이라 필드 추가 불가" 주장은 현재 거짓 — master ui.rs의 `ModelUsageDoc{}` 리터럴은 **테스트 2곳뿐**(L2661, L3403). 해당 테스트 리터럴에 필드 추가 + docstring 수정. (open PR #47/#38/#36 중 ui.rs 병렬 rewrite 없음 — 착수 시점에 재확인 1회)
- **리터럴 생성처 전수 (구현 브리프에 명시)**: ui.rs 테스트 2곳 외에 `src/dashboard.rs:953`(프로덕션 in-flight-only row push)과 `src/tui/mod.rs:2043`에도 `ModelUsageDoc{}` 생성이 있다 — 컴파일러가 잡아주지만, 이 영역에서 에이전트 오독이 이미 1회 있었으므로 4곳 모두 브리프에 박는다.
- TUI `model_cost()` 재계산 로직을 doc 필드 사용으로 교체(fallback: 필드 0이고 토큰>0이면 재계산 — 구 daemon doc 호환).
- (결정 2 채택 시) `ClientUsageDoc.cost_usd`/`last_seen_ms` 동일 패턴.
- 테스트: 새 필드 백컴팻(필드 없는 구 doc → 0), round-trip, totals=Σrow 불변식 유지.
- 게이트: `just check`.

### S2 — 데이터-품질 라벨 (Phase 4) — data_quality 필드 포함 (필수)
- `DashboardDoc.data_quality: DataQualityDoc` **additive 필드 추가(필수 범위)** — 4개 라벨 문자열: model_usage="hydrated activity/runtime", windowed="best effort", cost="API-equivalent estimate", cache="missing shown as unavailable". `#[serde(default)]` + 구 doc 백컴팻 테스트.
- TUI: `$` 컬럼/코스트 표시부에 **렌더되는** "≈ API-equivalent" 표기(히트맵 qualifier와 같은 패턴), 문구는 `data_quality` 필드에서 읽음(필드 비어있으면 로컬 기본 문구 fallback — 구 daemon 호환) + 렌더 테스트. 기존 windowed/cache 라벨은 손대지 않음.
- 게이트: `just check` + 라벨 렌더 테스트.

### S3 — Islands Phase 1 (계약 연결)
- `LlmuxDashboard` DTO + 중첩 DTO 12종(gist 02 명명 그대로) **+ `LlmuxDashboardDataQuality`(13번째 — gist 12종은 data_quality 이전에 작성됨, optional 디코딩)**, snake_case CodingKeys. **모든 D-마크 필드는 Swift에서 optional/default 디코딩** (구 daemon 호환).
- `LlmuxClient.dashboard()` 추가.
- `IslandUsageModel.refresh()` 전환 + **hard requirement: dashboard 디코드 실패 시 `/llmux/status` fallback** (gist Phase 1 마지막 항목 — 이슈 본문에 없지만 필수). 계정 타일 경로(`tile(from:)`)는 dashboard의 `accounts[]`로 동일하게 공급 — 기존 거동 보존.
- `@Published` 추가: totals / modelUsage / clientUsage / windowed / activity.
- **빌드 게이트 선행 작업**: 진짜 요구사항은 "재현 가능한 xcodebuild 게이트 + 테스트 타깃". `project.yml`에 명시적 `schemes:`/테스트 타깃이 없음 — xcodegen이 기본 앱 scheme을 생성해줄 수 있으므로 착수 시 `xcodegen generate` 후 실측으로 확인하고, 없으면 scheme을 project.yml에 명시. DTO 디코드 테스트를 위한 최소 테스트 타깃 신설(현재 테스트 0개) — 실 daemon 캡처 JSON fixture로 디코드 검증.
- 게이트: `xcodegen generate` + `xcodebuild` + DTO 디코드 테스트.

### S4 — Islands Phase 2 (분석 UI)
- summary cards(requests/tokens/cost/error-rate) → top-3 models → 24h/72h heat strip → recent activity → health banner → model/account detail sheets. gist §4의 expanded/popover 구조를 따르되 **closed-island 문자열 포맷(`llmux C:2 X:1 $0.42`)과 탭 4종은 v1 범위** — 그 외 §4 세부는 후속.
- **v1 명시 규칙 3건** (gist hard rule, 구현 브리프에 그대로): ① closed-island 경고색 = `auth_failed` 계정 존재 또는 quota>90% (health banner의 조건과 동일 소스); ② cache 미제공(`nil`)은 Islands에서도 `—`로 렌더(0 금지); ③ 모델 행 키는 `(group, model)` — 모델명 일치만으로 Claude/Codex 행 병합 금지.
- dead `UsageDashboardPanel`은 레이아웃 참고만, 와이어링 금지(혼입 방지 위해 v1에서 삭제 제안).
- 라벨: cost에 "≈ estimate" 툴팁/캡션 — 문구 소스는 S2의 `data_quality` 필드(동일 SSOT). **구 daemon(필드 부재) fallback은 TUI와 동일한 기본 문구 상수를 사용** — 상수 문자열이 두 표면에서 글자 단위로 일치해야 "라벨 문구 일치" acceptance가 구 daemon 케이스에서도 성립.
- 게이트: `xcodebuild` + 스크린샷.

### S5 — 검증 (gist Phase 5, 이슈 acceptance)
- `cargo fmt` / `cargo clippy -- -D warnings` / `cargo test` / `just check` / `xcodegen` / `xcodebuild`.
- 수동 parity: 같은 daemon에서 TUI vs Islands totals 일치 (requests/tokens/cost).
- 수동 fallback: 구 daemon(v0.2.x status-only 가정) 상대로 Islands 무크래시 + 타일 정상.
- 프롬프트/응답 본문 미노출 확인 (이슈 acceptance).

## 리스크 / gotchas

- **로컬 체크아웃(`docs/readme-islands-update`)은 origin/master 대비 stale** (behind 수치는 측정 시점별로 103→15로 관측됨 — 수치에 의존하지 말 것). 모든 작업은 origin/master 기반 새 worktree에서, 착수 시 재측정.
- **xcodebuild 게이트**: 테스트 타깃 부재 + scheme은 xcodegen 생성 여부 실측 필요 — "재현 가능한 빌드/테스트 게이트"가 acceptance의 숨은 선행 작업 (S3에 포함).
- **에이전트 오독 사례 기록**: recon이 master L751을 "per-row cost_usd 존재"로 오독 → dispatcher가 struct 직접 확인으로 반증. 구현 에이전트에게 "L751은 per-request다"를 브리프에 명시할 것.
- **배포/머지 게이트**: llmux ship 액션(머지·릴리즈·데몬 재시작)은 유저 소유 — 각 slice는 PR까지만.

## Out of scope (이슈 + gist 합의)

호스팅/멀티유저 분석, 외부 분석 백엔드, 프롬프트/응답 내용 표시, 스케줄러 변경, credential 처리 변경, TUI cockpit 대개편(이미 존재), sessions overlay(#34), 클라이언트 귀속 개선(#32).

## Acceptance (요약)

이슈 본문 7항목 + gist Phase 5 전체 = S5 게이트. 추가로: 구 dashboard doc 파싱 유지(기존 테스트 + 신규 필드 테스트), TUI·Islands 라벨 문구 일치.
