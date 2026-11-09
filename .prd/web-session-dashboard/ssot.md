# web-session-dashboard — SSOT (frozen)

- Repo / env: `2lab-ai/llmux` — new surface `webui/` (Node, zero dependency) served behind Caddy on
  fable-m5max (`https://llmux.internals.2lab.ai/`) and iq-64 (`https://llmux.internals.insightquest.io/`).
- Report channel: the Slack thread that requested it (#llmux channel, thread 1793996619.575599).
- Frozen: 2026-11-09. Changes only by an explicit user message → append it below, then re-plan.

## Verbatim dictation (oldest first)

1. 2026-11-07 — "현재 tui만 있는데 이거 ui로도 만들어 줘 ui를 어떻게 만들지는 터치 해 가지고 1차 시안으로 네가 세 개 정도 만들어 주고 전부 다 보여 줘 실제 해당 로컬 포트 아무 데나 열어 가지고 네가 그 서빙 하도록 해 줘"
2. 2026-11-07 — "와 상상력이 이렇게 부족해? 와 / 핵심뷰와 행동: accounts(어떤 계정이 있는지, 신규 계정 추가 삭제 등 관리), activities(현재 요청별 로그 확인), keys (테넌트별 사용량 확인) / 여기에 지금빠져있는 것이 클로드코드나 코덱스에서 한 세센에 대한 sessions 탭이 지금 완성이 안되어 있어. 예를들어 내가 클로드코드 콘솔을 한 20개 씀. 이 각각의 세션을 구분하고 확인할 수 있어야함. claude code에서 보는 뷰를 재 구축해서 보여줄 수 있어야함. 세부 내용은 현재 soma-work 대시보드에서 보는 뷰처럼 / 즉, 지금 내가 원하는건 soma-work에서 요청하는건 soma-work 대시보드로 모든 동작을 상세히 work log 단위로 확인할 수 있음. 같은 방식으로 llmux dashboard에서 세션 단위로 내용을 확인할수 있는 대시보드를 만들기 원함. 즉, 모든 soma-work dashboard에서의 작업 카드는 llmux 에서 한 세션 카드가 되는데 llmux dashboard에서 soma-work가 아니라 직접 claude code로 작업하는 내용도 이렇게 작업 카드처럼 세부 내용을 자세히 요청과 응답의 activities로 볼수 있고 클로드 코드에서 보는 채팅창을 재구성해서 볼수 있어야함. 이거 리서치해서 다시 시안 3개 만들어서 보여줘."
3. 2026-11-07 — "s1 패널로 구현하고 soma-work 대시보드 설치된 iq-64랑 fable-m5max 두곳에 서빙해줘"
4. 2026-11-09 — "지금 soma-work 대시보드는 https://agents.internals.2lab.ai/ 여기에 올려져 있는데 이 llmux 대시보드는 같은 방식으로 같은 인증 사용해서 (둘중에 아무거나 로그인하면 둘다 볼수 있게 해줘) https://llmux.internals.2lab.ai/에 오픈해줘. (이건 관리자만 볼수 있어야함) ---- 그리고 작동 확인 후 https://llmux.internals.insightquest.io/ 도 똑같이 해줘. (iq-64에도 로컬에 llmux가 있음)"
5. 2026-11-09 — "https://llmux.internals.insightquest.io/ 여기 제대로 안나오는거 같은데 여기에 유저가 한 10명 되는데 key로 구분되거든? 각각 유저별로 구분도 하고 네가 뭘 더 자세히 출력할수 있는지 저기 데이터 이용하여 리서치해서 개선해줘 / 디테일 작업 카드도 그렇게 전반적으로 2lab.ai/ui 컴포넌트 이용해서 최대한 충분히 사고하고 누가 어떻게 보고 뭘 확인할건지 생각하고 만들어줘 $local:using-dotprd 이걸로 prd는 다 쓴거야?"

6. 2026-11-09 — "1. 클로드 코드 화면 병신아 이게 뭐야. 진짜 클로드 코드 화면처럼 만들어줘 지금 뭐 완전 잘못 해놓음. (첨부 스샷 참조) 2. 세션 -> 요청의 개별 요청을 한번에 출력하지말고 on demand로 페이지 네이션 처리 3. 매 요청을 llmux tui에서는 실제로 raw 데이터를 모두 볼수 있는데 시발 여기서도 모두 볼수 있게 해줘. json 형태의 데이터니 한번에 전체 펼치지 말고 마지막 메세지만 펼쳐서 보여주되 (지금 니가 마지막 메세지만 보여주고 있는 중), 나머지도 json tree 형태로 원하는 부분을 펼쳐서 원본 메세지를 그대로 볼수 있게 해줘. (raw message로도 볼수 있도록. llmux tui에서 실제 메세지 보는 부분 참조)"
7. 2026-11-09 — "그리고 지금 "키 없음"으로 요청이 오는 경우가 있는데 이거 확인해줘 soma-work에서 요청 왔다고 하는데 57525069-befb-4403-a946-43b001526165 이거네 이거 사용자가 slack에서 요청 보내면 해당 사용자의 key로 요청보내게 되어 있는데 왜 이렇게 됐는지 확인하고 이것도 픽스해줘" / "아 혹시 이거 haiku 워크플로우 확인하는 요청인가? 워크플로우 파악하는 요청에는 유저 키를 안넣고 있는 버그인가? 그러면 픽스해줘 아무튼" (fix lives in 2lab-ai/soma-work)

## Fixed scope (from 1–7)

- Session = one Claude Code / Codex / SDK conversation through llmux; every soma-work task card is one
  session card; personal Claude Code consoles are session cards too.
- Each session card opens a work log (instruction → actions → result), a Claude Code style chat
  reconstruction, and the per-request activities with the raw request/response.
- Accounts, activities and keys views stay.
- **Users are told apart by issued key (llmux tenant).** Every surface that shows sessions or usage can be
  read per user.
- Detail task card uses `@2lab-ai/ui` components (demo https://ui.2lab.ai), designed from who looks
  and what they check (`../26-web-session-dashboard.md` §Readers).
- Admin-only, same login as the host's soma-work dashboard (either login opens both).
- Served on both hosts; iq-64 is verified with its real multi-user data, not only fable-m5max.
- The Claude Code screen looks like the Claude Code terminal (6). A session's requests load page by page on demand (6).
- Every request's raw record is fully viewable like the TUI raw viewer: all legs, headers, JSON tree with only the last
  message expanded, raw text (6).
