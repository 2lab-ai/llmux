# SSOT — llmux-islands todo (2026-07-02)

Fixed target for the `/ssot-converge llmux` loop. Source of truth = this file (not re-read of origin).
Origin: `/Users/zhugehyuk/2lab.ai/teamagent/todo.md` (31 lines, untracked, captured 2026-07-02).
Established by 2 independent subagent extractions, verbatim-identical censuses (one trivial
line-count delta 31 vs 32 = trailing-newline counting; content identical → reconciled).

- Target repo: `llmux` = `/Users/zhugehyuk/2lab.ai/teamagent` = github.com/2lab-ai/llmux, branch `master`
- Affected component: `llmux-islands/` (Swift/Xcode macOS companion app)
- Repo gate: `just check`; runbooks: agent-build (feature branch) / agent-deploy (master→preview) / agent-release
- Hazard: agent runs THROUGH llmux:3456; islands auto-starts the daemon (v0.2.8) — do not kill/restart the daemon
- Reporting: Slack channel not specified by user → default in-session reporting
- SSOT immutable until loop completes; changes only by explicit user event

## Universe (verbatim, from todo.md)

### Item 1 — floating island label (todo.md L5–15)

> 1. `llmux-islands`의 플로팅 아일랜드가 현재는 그냥 검은 박스로 보인다. 맥북에서 확장 화면 등에
>    출력할 때 그 영역에 정보를 쓸 수 있으므로, 이름/상태를 예쁘게 출력한다.
>
>    표시 형식:
>    ```text
>    Llmux Islands [클로드문어] [클로드아이콘]{세션숫자} [코덱스아이콘]{코덱스_세션숫자}
>    ```
>    - `클로드문어`는 현재 앱의 최상단 좌측에 표시 중인 이름을 사용한다.
>    - 실행 중인 모델 activity 세션 숫자가 `0`이면 해당 아이콘/숫자는 출력하지 않는다.
>    - 세션 숫자가 `1` 이상이면 레인보우 색상으로 색상을 루프 돌린다.

### Item 2 — 클로드문어 jump loop animation (todo.md L17–22)

> 2. `클로드문어`가 살짝 점프하도록 루프 애니메이션을 넣는다.
>    - 세션 숫자가 `1`일 때는 정상 속도.
>    - 세션 숫자가 `2` 이상이면 점점 빨라진다.
>    - 세션 숫자가 `10`이면 아주 빠르게 점프한다.
>    - 속도 스케일은 `10`까지만 적용한다.

### Item 3 — email anonymous setting (todo.md L26–31)

> 3. `llmux-islands` 설정에 `email anonymous` on/off 옵션을 추가한다.
>    - `on`이면 `llmux-islands Usage` 화면/영역에서 이메일을 후처리로 모자이크 처리한다.
>    - 이메일 텍스트를 알아볼 수 없을 정도로 `pixelize` 필터를 적용한다.
>    - 기준값은 대략 `4x4` 픽셀라이즈 정도로 시작한다.
>    - `off`이면 기존처럼 이메일을 그대로 표시한다.

## Acceptance rows (observer-visible; matrix spine)

Completeness reconciliation: universe = 3 items + 12 spec bullets + 1 format block →
10 rows below. Merges (each documented): R2.4 = L21+L22 (one observable "fast & clamped at 10"
behavior); R3.2 = L28+L29+L30 (one observable "ON → pixelized-to-illegible ≈4x4" behavior).
No bullet is uncovered.

| Row | Criterion (관측자가 본다) | Source |
|---|---|---|
| R1.1 | 플로팅 아일랜드가 검은 박스가 아니라 `Llmux Islands [클로드문어] [클로드아이콘]{n} [코덱스아이콘]{m}` 형식의 라벨을 보여준다 | L5, L10 |
| R1.2 | `[클로드문어]` 슬롯이 앱 최상단 좌측에 표시 중인 이름과 일치한다 | L13 |
| R1.3 | 세션 숫자 0인 모델의 아이콘+숫자는 라벨에서 보이지 않는다 | L14 |
| R1.4 | 세션 숫자 ≥1인 요소는 레인보우 색상 루프를 돈다 | L15 |
| R2.1 | 클로드문어가 살짝 점프하는 루프 애니메이션이 보인다 | L17 |
| R2.2 | 세션 1일 때 정상 속도로 점프한다 | L19 |
| R2.3 | 세션 ≥2부터 점프가 점점 빨라진다 | L20 |
| R2.4 | 세션 10에서 아주 빠르게 점프하고, 10 초과에서도 그 이상 빨라지지 않는다 | L21, L22 |
| R3.1 | llmux-islands 설정에 `email anonymous` on/off 토글이 보인다 | L26 |
| R3.2 | ON이면 Usage 화면의 이메일이 알아볼 수 없게 픽셀화(≈4x4 시작값)되어 보인다 | L28–30 |
| R3.3 | OFF면 이메일이 기존처럼 그대로 보인다 | L31 |

(11 rows — R1.1–R1.4, R2.1–R2.4, R3.1–R3.3.)

## Tensions → defaults (autonomous; user may override any)

| # | Ambiguity | Default chosen |
|---|---|---|
| T1 | `[클로드문어]` = name text vs mascot graphic (L13 says "이름을 사용", L17 animates it) | AS-IS scan (2026-07-02): the app's top-left displays NO name text — only the pixel-art `ClaudeCrabIcon` mascot (NotchView.swift:215, drawn NotchHeaderView.swift:11–93). Default: `[클로드문어]` = that existing mascot, reused in the closed-state label; Item 2 animates it. |
| T8 | `{세션숫자}` semantics (L10): per-provider count = Σ `in_flight` vs #accounts-with-activity | Σ of `in_flight` over that provider's accounts (daemon already sends per-account `in_flight`; LlmuxStatus.swift:19,26) |
| T9 | Which count drives the jump speed (L19–22 just says "세션 숫자") | The Claude session count (the mascot is Claude-flavored); 0 → idle per T3 |
| T2 | Rainbow scope (L15): icon, number, or whole line | Per-model `[아이콘]{숫자}` element whose count ≥1 (parallel to L14's "해당 아이콘/숫자"), not the whole line |
| T3 | Jump at 0 sessions (unspecified) | No jump (idle) when 0 Claude sessions |
| T4 | `4x4` = 4px blocks vs 4x4 cells total | Pixelize with ~4x4-pixel blocks (e.g. CIPixellate-style scale), a tunable constant |
| T5 | Anonymize scope beyond Usage screen | Usage 화면/영역 only, as written |
| T6 | Label only on extended display vs always | Always render the label (black box merely most visible on extended displays) |
| T7 | Items 1+2 one feature or two | Two separately shippable items, per file numbering |

## Status at capture

All 3 items open (no checkboxes/strike/DONE markers anywhere in todo.md).
Repo dirty state at capture: `screenshots/llmux-demo.gif` modified; `todo.md` untracked.
HEAD: d0b0a01 "release v0.2.8: server runs with 0 accounts + islands auto-starts the daemon".
