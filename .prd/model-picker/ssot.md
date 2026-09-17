# Model picker — SSOT

Status: shipped
Date: 2026-09-17

## User instruction (verbatim)

> 3. 모델 카탈로그 변경해주는 기능이 있는거 같은데 다음 소스 확인해서 /using-dotprd로 모델 카탈로그 변경해서 지원하는 모델 출력해주도록 해줘
> - https://github.com/gargpratyush/jev-router

Referent confirmed with the user 2026-09-17 (question: `modelPicker` injection
vs `/v1/models` discovery vs config-editable catalog): **"llmux run → /model
피커에 llmux 카탈로그"**.

## Acceptance contract

| User clause | Required observable outcome | Evidence |
| --- | --- | --- |
| 다음 소스 확인해서 | jev-router analysed at `a5694a6`; finding recorded in `.prd/17-claude-code-model-picker.md` (no catalog feature; picker-injection mechanism only) | done, 2026-09-17 |
| 모델 카탈로그 변경해서 | `llmux run` injects a `modelPicker` lineup built from `GET /llmux/models` via `claude --settings` | done — PR #161 (main 464f5bb) |
| 지원하는 모델 출력 | `/model` inside a `llmux run` session lists the llmux catalog rows (screen capture) and a selected non-Claude row routes to that backend (activity log line) | done — loop.md gap matrix, 2026-09-17 live receipt + post-deploy re-check |

## Boundaries

- No settings-file writes; `--settings` on the command line only.
- User-supplied `--settings` always wins; llmux never merges.
- Catalog fetch failure never blocks the launch.
- Catalog contents are out of scope (no config-driven catalog).
