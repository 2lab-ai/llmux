# web-session-dashboard — done (append-only)

Loop: ./loop.md · Each round close appends one block at the bottom; nothing above it ever changes (this skill's `scripts/loop-lint.py` C2 compares against git). Rows here are snapshots (Last Measured frozen at move time). A later pass is a `re-verified` line in a loop.md round section; a regression reopens as a new loop.md row with Source `regression:<ID>`.

## R1 — closed 2026-11-09 13:40 KST

- measured 8✅ 0🟡 0🔴 0🚪 0⚫ · result: per-user board, users view, @2lab.ai/ui detail card live on both hosts · closed rows: G1 G2 G3 G4 G5 G6 G7 G8 · commit/PR: 3e7b7ed on `feat/web-session-dashboard`

### Closed rows

| ID | Source | Universe Item | Observable Acceptance | AS-IS Evidence | Status | Gap | Dispatch | Verification Evidence | Last Measured |
|---|---|---|---|---|---|---|---|---|---|
| G1 | ssot 5 + spec A1 | side calls folded | iq-64 board has no side-call card; "부속 호출 N건 숨김" toggle | 155/300 cards are 2-request side calls | ✅ | — | W1 W2 | iq-64 public URL, `/api/sessions`: side-call-like cards on board 0, hidden 1,382; board capture shows "부속 호출 1,284건 숨김 · 보기" | 2026-11-09 13:35 |
| G2 | ssot 5 + spec A2 | user on every card + filter | every card names its user; chips filter | 245/300 cards without user | ✅ | — | W1 W2 | iq-64: 600/600 cards with owner, 11 users; chip click → board owners = the chosen user only (Playwright) | 2026-11-09 13:35 |
| G3 | ssot 2,5 + spec A3 | remote Claude Code sessions readable | title, origin 개인 PC, cwd, version, OS, MCP, instructions, chat | `fd268b59` title = id, labelled soma-work | ✅ | — | W1 W3 | `fd268b59`: title = its first real prompt, 개인 PC · MacOS arm64, cwd + branch, Claude Code 2.1.319, MCP atlassian+github, 17 instructions, 22 files, conversation rendered; 0 remote cards labelled soma-work | 2026-11-09 13:35 |
| G4 | ssot 2,5 + spec A4 | 사용자 view | per-user rows match keys/usage; hourly spend; sessions | keys tab = table only, English, stub buttons | ✅ | — | W1 W2 | `/api/users?window=24h` vs daemon `/llmux/keys/usage?window=24h`: 8 tenants, 0 mismatches; users + user-detail captures | 2026-11-09 13:35 |
| G5 | ssot 5 + spec A5 | detail on @2lab.ai/ui | 개요 instructions/cost/files/links; ui-work-log; raw | hand-rolled panel, no overview | ✅ | — | W3 | soma-work `96d9fb25` + remote `de9ec083` captures: 개요 (stat strip, context window, instructions, links, facts), 작업 로그 (ui-work-log groups per instruction), 요청 (ui-task-runs → raw sheet #7424) | 2026-11-09 13:35 |
| G6 | spec A6 | Q4 flags | burst / long run / stuck tool / errors / context flags visible | none | ✅ | — | W1 W2 | iq-64: 5 live sessions flagged; attention strip + card metrics + overview notice in captures | 2026-11-09 13:35 |
| G7 | ssot 4 + spec A7 | fable-m5max no regression | SSO + sessions + accounts + activities work | working | ✅ | — | W5 | fable public URL: no cookie 302 → bridge, API 401, admin 200, non-admin 403 (page + bridge), tailnet :4780 closed; board/accounts/activity captures, 0 console errors | 2026-11-09 13:35 |
| G8 | ssot 5 (dotprd) | code + .prd in repo | PR with webui/ + .prd + docs | `~/llmux-webui` not in git | ✅ | — | W4 | commit 3e7b7ed: `webui/`, `.prd/26-web-session-dashboard.md`, `docs/web-dashboard.md`, `rules/documents.md` row; `just check` exit 0; `node --test webui/test/` 9/9 | 2026-11-09 13:35 |

### Round section (as it was in loop.md)

- W1 server: owner/tenant attribution, users API, aux folding, origin classifier fix, remote title from excerpts, raw-io meta, pricing solver, Q4 flags → G1–G4, G6 ✅
- W2 page: `@2lab.ai/ui` app shell, board (`ui-agent-inbox`) with user chips, 사용자 view → G2, G4, G6 ✅
- W3 detail: drawer with 개요/작업 로그/대화/Claude Code 화면/요청/에이전트 → G3, G5 ✅
- W4 repo: `webui/`, `docs/web-dashboard.md`, `rules/documents.md` row → G8 ✅
- W5 deploy both hosts (LaunchAgent `ai.2lab.llmux-webui`, backups `~/llmux-webui-backup/pre-v2-*`) + captures → G7 ✅
- Log: iq-64 test instance on :4781 found a stale process holding the port (pkill pattern missed `node server.mjs`); restarted by port. Work log was empty until entries carried the shared instruction number (`gi`). "Tool loaded." and agent hand-backs were counted as instructions until `promptOf`/`agentmsg`.

## R2 — closed 2026-11-09 15:30 KST

- measured 3✅ 0🟡 0🔴 0🚪 0⚫ · result: Claude Code screen, paged requests, full raw viewer live on both hosts · closed rows: H1 H2 H3 · commit/PR: next commit on `feat/web-session-dashboard` (PR #238)

### Closed rows

| ID | Source | Universe Item | Observable Acceptance | AS-IS Evidence | Status | Gap | Dispatch | Verification Evidence | Last Measured |
|---|---|---|---|---|---|---|---|---|---|
| H1 | ssot 6.1 | Claude Code screen looks like Claude Code | welcome box, `>` prompt, `⏺` tool lines with ok/err colour, `⎿` results, diff, todos, thinking, input box | user screenshot 14:58: thinking squeezed into a 2-char column, timestamps per line, wide gaps | ✅ | — | R2 | iq-64 `96d9fb25`: 4 prompts, 78 tools (70 ok green, 7 err red), 44 thinking, welcome + input box; fable `cf788be7`: 107 tools; captures `iq-cc-top/-bottom` | 2026-11-09 15:20 |
| H2 | ssot 6.2 | requests paged on demand | detail payload carries no request list; 요청 loads 50 per page with prev/next, kind filter, page size | detail sent up to 1,500 rows every poll | ✅ | — | R2 | `/api/session/:sid` has `requestCount` only (251 KB vs 2.3 MB before); pager `1–50 / 1,685` → next `51–100` | 2026-11-09 15:20 |
| H3 | ssot 6.3 | full raw record like the TUI | legs + general + headers + body tree (last message open) + raw + copy/curl/all/save | only the last message, clipped | ✅ | — | R2 | `/api/raw-full` verbatim record; tree opens `/messages/<last>` only; Response leg = assembled SSE message (313 events); raw / pretty modes; headers 21/27 | 2026-11-09 15:20 |

### Round section (as it was in loop.md)

- Plan: ccview.js/css renderer from chat entries (+ slim tool inputs in lib/transcript.mjs); `/api/session/:sid/requests`
  pagination; `/api/raw-full` + rawview.js tree/raw viewer modelled on src/tui/ui.rs `raw_content_from_record`.
- Log: first raw capture showed 100 collapsed message rows above the open last message → messages/input start with the
  last 6 drawn, "앞쪽 N개 더 보기" loads earlier ones.
