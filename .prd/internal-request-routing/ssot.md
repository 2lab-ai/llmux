# internal-request-routing — SSOT (frozen)

- Repo: `2lab-ai/llmux`.
- Redaction: machine names are written as `[owner Mac]`; account emails are omitted.
- Report channel: the Slack thread that asked for it (#llmux channel, thread 1794465952.178559).
- Frozen: 2026-11-12. Changes only by an explicit user message → append it below, then re-plan.

## Verbatim dictation (oldest first)

1. 2026-11-12 — attached screenshot: the session list of a GPT main session on the [owner Mac]
   console — `제목 · haiku-5-5 · medium · #50916`, `security · gpt-6-luna · medium`,
   `지시 · gpt-6-astra · max`, `서브에이전트 · fable-5-2 · medium`.

   "claude 모델을 사용하면
   처음에 제목을 받기위해서 유저의 지시를 haiku에게 물어봄
   그리고 auto mode classifier 요청을 sonnet?에게 물어봄
   ---

   llmux에서 다른 회사의 모델로 변경할때도 haiku랑 sonnet을 사용하고 있어서
   classifier는 gpt의 경우 luna?로 변경하도록 했거든?

   근데 제목은 지금도 haiku에게 물어보고 있네

   이거 전체적으로 똑같은 건데 니가 븅신처럼 일부만 작업한거잖아?

   일반화해서 $local:using-dotprd 로 작성하고 픽스해줘.

   즉
   claude code sdk에서 자동으로 자신의 모델 호출하는 여러 가지들이 있는데 (title 요청, 시큐리티 체크, recap, compact 등) 모델이 변경되면 메인 모델의 하위 모델을 사용하여 하도록 이를 일반화해서 처리해줘.

   codex는 terra, luna를 사용해주고
   grok은 effort를 줄여서 사용

   그리고 이것들은 전부 alias를 사요해서 나중에 모델이 변경되도 자동으로 상위 모델 사용하도록 처리."

## Fixed scope (from 1)

- One rule for every request Claude Code sends on its own (title, security check, recap, compact,
  quota, status, helper turns): when the session's main model is not a Claude model, the request is
  served by the main provider's smaller models, not by Claude Haiku or Sonnet.
- Codex main sessions use `terra` and `luna`.
- Grok main sessions use the `grok` model with a lower reasoning effort.
- Every target is an alias, resolved per request, so a later model update moves these requests to
  the newest model without a config change.
- The design is written in `.prd` (spec + this workstream) before the fix ships.
