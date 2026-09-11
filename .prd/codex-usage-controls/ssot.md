# Codex usage controls

Status: in-progress
Date: 2026-09-11

## User instruction (verbatim)

> llmux 개선
> codex는 주간 사용량 충전해주는 충전 기능이 있거든? codex에서 /usage로 확인할 수 있고 사용할수 있음 이걸 어떻게 api로 확인하고 사용할수 있는지 먼저 리서치해서 /using-dotprd 로 정의해주고 accounts에 자연스럽게 계정별 리셋 남은 횟수 확ㄷ인하고 사용할 수 있도록 해줘
>
> 그리고 지금 외부에서 리셋해도 사용량이 0%에서 변동하지 않는데 직접 사용량 리프레시할수 있는 명령을 accounts에 refresh 정도 명령으로 추가해줘 (switching을 강제로 하면 이때도 리프레시하도록 개선)

## Acceptance contract

| User clause | Required observable outcome | Evidence |
| --- | --- | --- |
| 먼저 리서치해서 /using-dotprd 로 정의 | Primary-source reset API research, explicit unsupported/unknown boundaries, spec and vertical trace before implementation | Pending research |
| accounts에 자연스럽게 계정별 리셋 남은 횟수 확인하고 사용할 수 | Account-specific remaining resets and deliberate reset action in accounts; never treat unknown as zero | Pending API contract |
| 직접 사용량 리프레시할수 있는 명령을 accounts에 refresh 정도 명령으로 추가 | Explicit refresh performs a new upstream usage read and returns fresh state to accounts, including previously exhausted accounts | Pending trace/test |
| switching을 강제로 하면 이때도 리프레시 | Forced switch triggers usage refresh for the selected account and exposes failure rather than pretending cached data is fresh | Pending trace/test |

## Boundaries

- Integrate into the existing daemon-owned account state and existing accounts surfaces.
- Research distinguishes allowance resets from paid credit purchases. No purchase flows.
- Do not redeem real account reset entitlements during testing without explicit approval for that account and expenditure.
- Do not copy live rotating credentials into test configurations.
- Preserve unrelated dirty files in the main checkout.
- Mock upstream receipts establish implementation behavior; authenticated live reads and post-deploy smoke are distinct evidence.
- No unverified endpoint or schema may be shipped as a functional reset action.
