# Account plan tier — SSOT

Status: in-progress
Date: 2026-10-26

## User instruction (verbatim, 2026-10-26)

The request reached the agent through the session goal check and was confirmed in Slack. The
agent asked (verbatim):

> [medium] 다음 목표를 확인해 주세요: llmux가 계정 플랜(티어)을 직접 알아내게 하는 작업을 진행할까요?
> … (1) llmux가 구독 계정 티어를 얻는 방법 리서치, (2) $using-dotprd 사용, (3) llmux를 고쳐 남은
> 사용량 옆에 티어를 보여 주는 릴리스 …

User's answer (verbatim):

> yes-llmux

(option: "맞음. llmux에서 티어를 얻게 해줘 — 리서치 → $using-dotprd로 스펙 → llmux 수정·릴리스(남은
사용량 옆에 티어) → soma-work에서 설정 없이 티어가 보이는지 확인")

## Acceptance contract

| User clause | Required observable outcome | Evidence |
| --- | --- | --- |
| llmux가 구독 계정 티어를 얻는 방법 리서치 | Sources measured on real accounts and recorded | `.prd/25-account-plan-tier.md` §Sources |
| $using-dotprd 사용 | `.prd/25` + this workstream folder (ssot, loop) committed with the code | this folder |
| 남은 사용량 옆에 티어 | `llmux status` lines carry `<plan>` beside `7d N% left`; status JSON carries `plan`/`rate_limit_tier`/`plan_type`; TUI detail names the plan | unit tests; live `llmux status` capture on the deployed daemon |
| 릴리스 | version bump → `v*` tag → Release workflow green → brew formula → upgrade on the dev hosts → `llmux status` shows the new version | loop.md ship table |
| soma-work에서 설정 없이 티어 | soma-work dashboard counts real plans with no `SOMA_LLMUX_ACCOUNT_TIERS` (no "x20 가정") | soma-work API capture on a dev host with the map removed |

## Boundaries

- Display/consumer data only; scheduler untouched.
- Accounts table columns untouched (`.prd/18`, `.prd/19`).
- No config-file persistence.
