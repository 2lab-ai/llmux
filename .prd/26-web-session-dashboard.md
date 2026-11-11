# 26 — Web session dashboard (`webui/`, llmux.internals.*)
Status: in-progress
Date: 2026-11-09

Original request and frozen scope: [web-session-dashboard/ssot.md](web-session-dashboard/ssot.md).
Round plan and open gaps: [web-session-dashboard/loop.md](web-session-dashboard/loop.md).

## Problem

The web dashboard (`webui/`, served at `https://llmux.internals.2lab.ai/` on owner-mac and
on a shared team host) was built against one person's machine. On the
shared team-host daemon (16 issued keys, about 10 people) it does not answer the questions it exists for.
Measured [LIVE team host, 2026-11-09 12:50 KST, `GET /api/sessions` + browser capture]:

1. **Half the board is noise.** 155 of 300 cards are soma-work one-shot side calls (safety reviewer,
   "Summarize this AI assistant response", `<session-title-pending>`), each 2 requests, each shown as
   its own session and 16 of them in "응답 필요".
2. **Users are invisible.** Every `activity.jsonl` row carries `tenant` (issued key id), but cards only
   show a name when the row came from the live dashboard doc. 245 of 300 cards have no user.
3. **Personal Claude Code sessions look broken.** People run Claude Code on their own Macs with their
   key (devices `900f8b45`, `0a21832d`, `58d4ffd3`, `a9139eae`). There is no transcript on the host, so
   26 cards show an 8-hex id as the title, no project, no branch, and are labelled `soma-work` because
   the classifier treats any key name containing a Slack id as soma-work. The biggest spenders
   (one session with 4,000 requests ≈$520 over 9 days; another with 3,690 requests) are among them.
4. **The detail panel assumes a local transcript.** For remote sessions it shows only the tail of one
   raw request; there is no overview of what was asked, what changed, or what it cost per instruction.

## Readers and what they check

Viewer: an admin of the host's soma-work (`ADMIN_USERS`), signed in through the soma-work login.

| Reader | When | Checks, in order |
|---|---|---|
| R1 team lead on the team host | several times a day, glance first | Q1, Q2, Q4 |
| R2 admin debugging one session ("my agent is stuck / weird / expensive") | on report | Q3, Q5 |
| R3 Z on owner-mac watching ~20 own consoles and bots | continuously | Q1 (which one needs me), Q3 |

- **Q1 Who is doing what right now?** Per user: running and waiting sessions with title, repo and
  branch, PR links, elapsed time and the step in progress.
- **Q2 Who uses how much?** Per user, per window (1h/24h/7d/30d): cost, requests, tokens, errors,
  model mix, soma-work vs personal Claude Code, which subscription accounts they consume, last seen.
- **Q3 What happened in this session?** Each instruction in order, what the agent did for it (tools,
  commands, files changed, subagents), how it ended, cost and time per instruction.
- **Q4 What is abnormal?** Long-running or high-cost sessions, request bursts, error or abort
  bursts, a tool call stuck for minutes, context near the window limit.
- **Q5 What exactly went over the wire?** One request and response, raw.

Glance rule: the first screen answers Q1 and flags Q4 within 3 seconds without scrolling; every number
links to the sessions or requests behind it.

## Data available (research, [LIVE team host 2026-11-09])

| Source | Fields used | Notes |
|---|---|---|
| `activity.jsonl` row | `ts_ms id kind tenant user_id account model effort fast status duration_ms ttfb_ms ttft_ms gen_ms aborted tokens{input,output,cache_read,cache_creation} excerpt` | `kind` ∈ user, subagent, sdk, other, title, recap, suggest, count, compact, audit. `excerpt` holds the prompt head for `user`/`sdk`/side-call kinds — the title source for remote sessions. No `cost_usd`. |
| `/llmux/dashboard` | `activity.completed[]` (+`cost_usd`, `client_name`), `activity.in_flight[]`, `session_labels`, `tenant_usage`, `client_keys`, `client_usage` (lifetime per session), `usage_stats` (hourly per model with `cost_usd`), `model_usage`, `accounts` | `usage_stats` gives one linear cost equation per hour×model; solving them yields per-token rates, so history rows can be priced from their tokens without copying `src/pricing.rs`. |
| `/llmux/keys/usage?window=` | `tenants[]{tenant,name,email,requests,ok,errors,tokens_in,tokens_out,cost_usd,first_ms,last_ms,models[]}` | Exact per-user usage per window — the Q2 numbers. |
| `~/.config/llmux.json` `client_keys[]` | `id name email kind suspended created_at_ms` | Users that exist but were idle in the window. Never read or show `key_prefix`/`key_digest`. |
| `/llmux/raw-io?id=&at_ms=` | request headers (`user-agent` → Claude Code version and cli/sdk, `x-stainless-os/arch`), body (`system`, `messages`, `tools` incl. MCP servers, `thinking`, `output_config.effort`, `context_management`), response SSE (`stop_reason`, usage incl. `thinking_tokens`) | ~1 MB per main request. The first user message carries CLAUDE.md paths and the working directory; the real user turns rebuild the conversation of a remote session. Fetched only when a session is opened. |
| Claude Code transcript (`<config>/projects/<cwd>/<sid>.jsonl`) | `ai-title`, cwd, branch, version, messages with `requestId`, subagent files | Present only for sessions run on the host (soma-work on the team host, local consoles on owner-mac). |

## Goals

1. Every session and every usage number is attributed to a user (issued key → `client_keys` name);
   `local` is shown as "시스템(키 없음)".
2. Side-call sessions are folded: hidden from the board by default, counted per user, one toggle shows them.
3. Remote Claude Code sessions get a real title (first real user prompt), origin "개인 PC" with device
   and Claude Code version, and a full detail reconstructed from raw-io.
4. A **사용자** view answers Q2: one row per user with window usage, active sessions, origin split,
   models, accounts; opening a user shows their sessions, hourly spend and model mix.
5. The detail task card is rebuilt from `@2lab.ai/ui` (the same components the soma-work dashboard uses)
   and answers Q3 first: 개요 (instructions with outcome, cost and time; files changed; links;
   context) → 작업 로그 → 대화 → Claude Code 화면 → 요청(원문) → 에이전트.
6. Q4 flags appear on the board and on cards: cost or request burst, long run, stuck tool, error burst,
   context ≥ 80%.

## Non-goals (this round)

- Account and key mutations (add, pause, rotate, suspend, remove). Buttons stay visible and say where
  to do it (TUI / `llmux keys`). The daemon routes exist; wiring them needs its own review.
- Showing key secrets, prefixes or digests.
- A new daemon endpoint. Everything comes from the routes listed above.
- Vendoring `@2lab.ai/ui` into this public repo (the package is UNLICENSED). It is loaded from the
  host's soma-work install, falling back to the pinned jsDelivr copy.

## Acceptance (run → observe)

A1. Team-host board, default filters → no card whose only requests are a known side-call template; the
    header shows "부속 호출 N건 숨김"; toggling it shows them.
A2. Team-host board → every card shows a user name; filter chips list each user with their running count;
    choosing one leaves only that user's cards.
A3. A remote personal Claude Code session (e.g. `fd268b59`) → card title is its first real user
    prompt, origin "개인 PC", detail shows working directory, Claude Code version, OS, MCP servers,
    each instruction with its tool calls, and the conversation.
A4. 사용자 view, window 24h → one row per user with cost/requests/errors equal to
    `/llmux/keys/usage?window=24h` for that tenant; opening a user shows hourly spend and their sessions.
A5. Detail of a soma-work session → 개요 lists instructions with tool count, cost and duration, files
    changed and PR links; 작업 로그 renders with `ui-work-log` + `ui-tool-call-card`; 요청 opens a raw
    exchange.
A6. Q4: a session above the burst threshold carries a visible flag on its card and in the attention strip.
A7. owner-mac keeps working with one user (no regressions in SSO, sessions, accounts, activities).
A8. Both public URLs: admin login only, as in request 4; screenshots of each acceptance attached to
    the round receipt.

## Architecture

- **Where:** `webui/` in this repo. `server.mjs` (Node ≥ 20, no dependencies) + `lib/` (index, transcript,
  raw-io parsing, pricing solver, users) + `public/` (static ES modules). Runs as a LaunchAgent bound to
  `127.0.0.1:4780`; Caddy terminates TLS and the soma-work SSO bridge guards it (`docs/web-dashboard.md`).
- **Session index (server):** sessions keyed by `metadata.user_id.session_id`, fed by the daemon
  dashboard doc every 2 s and a 160 MB tail replay of `activity.jsonl` at start. Each session keeps
  `owner` = most frequent `tenant` of its rows, `device`, the first and latest `user`-kind `excerpt`,
  per-kind counts, and the context size of its latest main request.
- **Classification:** origin = soma-work when the transcript lives under the soma-work config root or
  the cwd is `/tmp/U…/session_…`, or the device is the soma-work device of this host; otherwise from
  raw-io `user-agent` (`(external, cli)` → Claude Code, `sdk-*` → SDK) or group `codex`. Aux = ≤ 3
  requests, all `sdk`/side-call kinds, first excerpt matching the side-call template list.
- **Pricing:** per-model rates solved by least squares from `usage_stats` (`cost = in·a + out·b +
  cache_read·c + cache_creation·d`); falls back to average cost per request. Window totals always come
  from `/llmux/keys/usage` (exact).
- **Remote meta:** on detail open, the newest main raw-io record is parsed for headers, cwd, tools and
  real user turns; cached per session id with the record id.
- **UI:** `@2lab.ai/ui` 0.39 served at `/ui/*` from `UI_DIST` (default
  `/opt/soma-work/dev/node_modules/@2lab.ai/ui/dist`, else jsDelivr pinned). Board = `ui-agent-inbox`;
  detail = `ui-drawer` + `ui-status-bar` + `ui-tabs`; 개요 = `ui-stat-strip` + `ui-context-window` +
  `.ui-dl` + instruction rows; 작업 로그 = `ui-work-log` with `ui-tool-call-card` slots; 대화 =
  `ui-conversation`; 요청 = `ui-task-runs` + `ui-tool-call-card` raw; 에이전트 = `ui-agent-tree`; 사용자 =
  `ui-stat-strip` + `ui-compact-breakdown` + `.ui-row` list + `ui-bar-chart`. Elements receive data
  only; the page owns fetching (library rule: elements never fetch).
