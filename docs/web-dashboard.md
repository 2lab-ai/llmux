# Web dashboard

An admin-only web page for one llmux daemon, at `https://llmux.<your domain>/`. It answers four questions
about the people who share the daemon: who is doing what right now, who uses how much, what happened in
one session, and what exactly went over the wire.

It runs from `webui/` beside the daemon and signs in through the host's soma-work dashboard: one login
opens both. Only soma-work admins (`ADMIN_USERS`) get in.

## What you see

**세션 (sessions).** One card per Claude Code, Codex or soma-work conversation that went through llmux.

- Columns: 응답 필요 (waiting for a person), 실행 중 (running), 완료 (done, by day).
- The name pill on each card is the person whose issued key sent its requests. The chips above the board
  filter by person. The number on a chip is how many of their sessions are running.
- 출처 (origin): `soma-work` (a soma-work task on this host), `개인 PC` (Claude Code on someone's own
  computer with their key) or `이 머신` (a console on this host).
- 주의 (attention) lists live sessions that cross a threshold:
  - more than $25 in the last hour;
  - more than 150 requests in 10 minutes;
  - 3 or more errors or aborts in 30 minutes;
  - a tool call waiting more than 10 minutes;
  - more than 3 hours since the last instruction;
  - context window 80% or more full.
- Side calls are hidden by default and counted on the line under the chips ("부속 호출 N건 숨김"). These are
  safety reviews, card titles, summaries and next-prompt suggestions: automatic calls of 1–3 requests that
  are not work a person did.

**세션 상세 (session detail).** Open a card to see its detail.

- 개요 (overview): cost, requests, tokens, instructions, tool calls, duration and errors.
  - Each instruction the person gave, with the tools it used, the files it changed, the number of
    requests, its cost and how it ended.
  - The files changed, PR and issue links, and how full the context window is.
  - Where it ran: folder, branch, Claude Code version, OS, MCP servers and the subscription accounts used.
- 작업 로그 (work log): one group per instruction, each with its tool calls (arguments and results).
- 대화 (conversation): the chat.
- Claude Code 화면 (Claude Code screen): the session drawn the way the Claude Code terminal draws it:
  - the welcome box, `>` prompts, `⏺` text and tool lines (green when the tool succeeded, red on error);
  - `⎿` results with `… +N lines`, `Update(file)` with its diff, `Update Todos`, `✻ Thinking…`;
  - the input box and status line at the bottom.
  Click a tool line to expand its full result; times are in the hover title.
- 요청 (requests): the session's llmux requests, newest first. The next older page loads by itself as you
  scroll to the end of the list (25 to 200 at a time; filter by kind). A running session's new requests are
  added on top without moving what you are reading. A session keeps its newest 4,000 requests; the end of
  the list says so when older ones were let go. Open one to see the raw record.
- Raw viewer: the web twin of the TUI raw viewer.
  - Legs: `Request`, `Upstream Req`, `Upstream Resp` and `Response`. The upstream pair appears only on translated
    exchanges.
  - Each leg shows the general block, headers and body.
  - The body opens as a JSON tree with only the last message expanded. An SSE response is shown as the assembled
    message plus its events. Expand any node; long strings open in full.
  - 원문 shows the bytes as captured, and 원문(정렬) shows them pretty-printed.
  - 복사 copies the body, curl 복사 copies a replayable command, 전체 복사 copies all legs, and 기록 저장
    downloads the record JSON. Credential header values stay `•••redacted`.
- 에이전트 (agents): subagents, when the transcript is on this host.

For a session run on someone else's computer there is no transcript on the host. The detail is rebuilt
from the newest raw-io request of that session, so raw-io capture must be on (`raw_io.enabled`) for these
sessions.

**사용자 (users).** One row per issued key.

- Each row shows cost, requests and errors for the chosen window (1h, 24h, 7d or 30d). These are the
  daemon's exact numbers (`/llmux/keys/usage`).
- It also shows what the person has running or waiting now, where today's sessions ran, and an estimated
  24-hour spend curve.
- Open a row to see that person's live and recent sessions, models and the accounts they used.
- `키 없음 · 이 머신` is traffic sent from the daemon's own machine without a key: local consoles and soma-work's own
  automation.

**활동 (activity)** and **계정 (accounts)** show the same data as the TUI. Account and key changes are made
in the TUI or with `llmux keys`; the web page is read-only.

## Numbers

- Window usage on 사용자 comes from the daemon and is exact.
- Session cost uses the daemon's own number for recent requests. Older history rows carry no cost, so
  they are priced from their tokens with per-token rates solved from the daemon's priced hourly
  aggregates. That makes the session cost an estimate (`≈`). Both include a request's
  sub-inferences (an advisor call, a compaction, a billed fallback attempt — each at its own model's
  rates), and so do the session's token totals (2026-11-11, #278). The solved rates include the
  extended-TTL cache-write rate (Claude 1h, OpenAI 12h) next to the short-TTL one, and the per-search
  web-search fee, so history rows that report them price like the daemon's (2026-11-11, #278). A model
  whose recent writes were all one TTL takes that TTL's rate for the other.
  A history row is priced in its own price class (long context, Fast, dated rates): the solver fits
  each class on the `price_classes` parts the daemon prices separately, and the standard rates on the
  rest of each row. A class with no solved rate falls back to the model's standard rates, which
  understates it; an unbilled refusal costs nothing (2026-11-12, #278, #283).
- Context fill is the newest main request's input plus cache plus output, set against the model window.
  A session past 170k tokens counts as a 1M-window session.

## Run it

See [`webui/README.md`](../webui/README.md) for settings. A host that already serves soma-work at
`agents.<domain>` needs three things:

1. The LaunchAgent from `webui/deploy/ai.2lab.llmux-webui.plist.example`. It binds `127.0.0.1:4780` and sets
   `AUTH=soma`, `SSO_ORIGIN`, `PUBLIC_ORIGIN` and `COOKIE_DOMAIN`.
2. The two Caddy sites from `webui/deploy/Caddyfile.example`. They add the `/__llmux/*` bridge, the return
   after login and the shared logout on `agents.<domain>`, and add `llmux.<domain>` itself.
3. A certificate and DNS record for `llmux.<domain>`.

How the login works:

1. The bridge reads the soma-work session cookie.
2. It checks `/auth/me` for `isAdmin`.
3. It copies the same token into `llmux_sso`, scoped to the parent domain.

Logging out of soma-work clears `llmux_sso` too. Because the cookie is scoped to the parent domain, every
`*.<domain>` site on that host receives it. It is `HttpOnly` and `Secure`.
