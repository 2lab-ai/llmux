# llmux — Research note: magpie (2026-10-20)

Status: research snapshot, 2026-10-20. Not a contract. The improvement plan built on it is
[24-magpie-gap-roadmap.md](24-magpie-gap-roadmap.md). Companion to the CLIProxyAPI note
[04-research-cliproxyapi.md](04-research-cliproxyapi.md).

## Conclusion first

[yetone/magpie](https://github.com/yetone/magpie) (Go, MIT, created 2026-09-23) does most of what
llmux does — pooled subscription accounts, quota-aware failover, a menu-bar quota display, a
remote/central mode — plus the one thing its tagline sells and llmux does not ship: **bring any API
key** ("Codex on DeepSeek, Claude Code on Kimi"). It reached 17,487 stars in 27 days; llmux has 8
stars after 130 days (created 2026-06-12). The gap has three parts. The order is a judgment
[가설], not a measurement:

1. **Distribution.** Several capabilities that magpie users raise most already ship in llmux (§5).
   They are hard to find: no website, no badges, no demo GIF in recent releases, English only, one
   install channel. magpie launched from an author with 9,614 followers and two earlier hits.
2. **Generic API-key providers.** The largest mention cluster in magpie's issue tracker. llmux
   takes an Anthropic API key (`login --api`) and an OpenRouter key (`login --openrouter --paste`,
   `docs/operational-reference.md:13`). It has no generic provider: no presets for DeepSeek, Kimi,
   GLM, MiniMax or Qwen and no arbitrary endpoint. Only a `Local` stub exists
   (`src/provider/stubs.rs:74-79`), and a non-goal stands against it (`01-spec.md:59`).
3. **Reach.** Windows (182 distinct issue authors), Docker/NAS, a cross-provider fallback chain.

## 1. What magpie is

One Go binary. It (a) edits each coding agent's own config in place (about 63 agents) and (b) runs a
local gateway on `127.0.0.1:3425` that translates between OpenAI Chat, OpenAI Responses, Anthropic
Messages and Gemini, and routes across API keys and subscription accounts. Five front ends: menu-bar
tray, desktop window (Wails), TUI, browser UI, CLI. Headless: `serve`, `daemon install`, Docker,
Termux. macOS, Windows, Linux (x64 + ARM64). 86 provider presets
(`internal/provider/presets.go`, counted 2026-10-20). Free; income from relay sponsors.

| | llmux | magpie |
|---|---|---|
| Bet | one harness is capital, the model is the swappable part | every agent, every model, one switcher |
| Inbound | Anthropic Messages + OpenAI Responses (Claude Code, Codex) | Chat, Responses, Messages, Gemini (~63 agents) |
| Backends | Claude, ChatGPT/Codex, Grok, OpenRouter (+ Gemini preview) | 86 presets, any OpenAI/Anthropic-compatible URL, local servers, 5 subscriptions |
| Surface | daemon + TUI + Islands (macOS notch, KDE port) + CLI | tray + window + TUI + web UI + CLI |
| Platforms | macOS, Linux (CLI) | macOS, Windows, Linux, Docker, Termux |
| Build process | agent-driven, gated PRs | "written, reviewed, merged and released by agents" (`LESSONS.md:4`); 1,270 releases in 27 days |

## 2. Evidence base and method

- **Source:** shallow clone of `yetone/magpie` at 2026-10-20 (README, `docs/reference.md`,
  `docs/subsystems/`, `internal/`, `plugins/`). Read, never built or run. "Implemented" below means a
  doc and a source package both exist; nothing was exercised.
- **Issues:** all 3,036 items via `gh api repos/yetone/magpie/issues?state=all`: 2,464 issues and 572
  PRs. The owner filed 326 issues, so **user issues = 2,138 from 1,014 distinct authors**, filed
  2026-09-24 → 2026-10-20. 72% of all issues contain CJK text (1,765/2,464).
- **"Loved" cannot be read from reactions.** All 2,464 issues together carry 33 reactions. The proxy
  used here is **mention authors**: distinct authors whose issue text matches an area's keywords.
  Comment depth and explicit dependency language ("深度依赖" = deeply rely on, #979) are read
  alongside it. Mention authors measure how much attention and trouble an area draws. They do not
  measure preference or usage share, and a bug report counts the same as a feature request.
- **Classifier:** keyword areas over title, then body, multi-label; 33/2,138 unclassified.
  Reproduce: [research/magpie-2026-10-20/](research/magpie-2026-10-20/) (`classify.py`,
  `mentions.py`, outputs `areas.txt`, `mentions.txt`). Keyword counts are coarse. Use them for
  ranking, not as exact figures.

## 3. What magpie users raise most

| Area | Issues | Distinct authors | Feature requests | Signature issues |
|---|---|---|---|---|
| Client-agent support | 859 | 525 | 304 | Codex is mentioned in 537 issues, Claude Code in 287, OpenCode 147, Pi 105 |
| Providers / API keys | 614 | 387 | 248 | DeepSeek 194, relays 111, GLM 65, Kimi 59; #176 Bedrock (19 comments) |
| Usage & quota display | 382 | 250 | 155 | **#124 remaining-vs-used toggle (35 comments, most-discussed issue)**; #1423 USD per account |
| Subscription accounts | 351 | 240 | 120 | #2111 staggered 5h warm-up; #1630 quota card refresh |
| Routing & failover | 387 | 236 | 137 | #1418 "model at capacity" with no fallback (18 comments); #2077 multi-provider 429 |
| Platform / install | 249 | 191 | 61 | Windows/WSL appears in 294 issues by 182 authors; Docker/NAS/server in 139 |
| Remote / LAN | 146 | 113 | 64 | #979 central NAS daemon + local menu-bar quota; #545 LAN sharing |
| Web search / image | 113 | 94 | 44 | #1399 image/video relay (22 comments); #669 search returns empty |
| MCP/skills library | 136 | 94 | 64 | out of llmux scope (§6) |
| Free-credit check-in | 42 | 37 | 15 | #694 Qoder, #808 Trae, #1001 ZCode (17–20 comments each) |

(Areas overlap; an issue can count in several. Full table: `research/magpie-2026-10-20/areas.txt`.)

Signals in plain words:
- Two harnesses dominate the tracker. 703 of 2,138 user issues name Codex or Claude Code, and 339
  name only other agents. [가설] Most magpie use sits on the two harnesses llmux already serves.
  Codex is named about twice as often as Claude Code.
- The provider users want most is a **cheap API key behind their existing harness**: 401 issues from
  268 authors name DeepSeek, Kimi, GLM, MiniMax, Qwen, Ollama, OpenRouter or a relay.
- The single most-discussed issue is about the quota display (#124, 35 comments), and #979 says
  its author "deeply relies" on the menu-bar quota capsules.
- magpie answers fast: median 5.1 h from open to completed for user issues (n=1,065); feature
  requests completed in a median 2.1 h (376 of 803 completed).

## 4. Feature-by-feature gap matrix

Have = llmux ships it. Partial = a narrower version ships. Missing = not built. Out = conflicts with
an llmux non-goal or thesis (§6). Numbers in brackets = magpie inventory id.

| magpie feature | llmux | llmux evidence / note |
|---|---|---|
| [6] 86 presets, any OpenAI/Anthropic-compatible URL, custom headers | **Missing (generic)** | Anthropic key + OpenRouter key only (`docs/operational-reference.md:13`); `Local` stub (`src/provider/stubs.rs:74-79`); non-goal `01-spec.md:59` |
| [13] user-defined routing groups (smart/order/rotate/pace/weight, nested) | Partial | model→group routing + in-group scheduler (`README.md:179-185`, `docs/schedulers.md`) |
| [14] quota failover before first byte, rest until reset | Have | `01-spec.md:131-134` |
| (cross-provider fallback when a group is exhausted) | **Missing** | all-exhausted → 429 + retry-after (`01-spec.md:131-134`) |
| [11] multiple accounts per subscription, import existing logins | Have | 6 login types, 5 import sources (`docs/operational-reference.md:14`) |
| [8] Claude subscription via the real Claude binary/SDK | Have | Codex→Claude through the Claude Agent SDK (`.prd/21-codex-frontend.md`) |
| [9] ChatGPT, Copilot, Gemini, Antigravity as providers | Partial | ChatGPT/Codex, Grok, OpenRouter; Gemini preview; no Copilot |
| [20] daily warm-up of 5h windows | Partial | perishability scheduler + idle probe (`docs/configuration.md:100-124`); no staggered start |
| [21] quota CLI, menu-bar quota cards, reminder before unused quota resets | Partial | `status --json`, Islands tiles, TUI gauges; no reset reminder, no remaining/used toggle |
| [22] usage and cost in USD, tok/s, by key/account | Have | `.prd/14-usage-calendar-stats.md`, perf tab |
| [23] sessions with cost and title, reopen | Partial | TUI sessions tab |
| [24] live routing view | Have | raw request viewer (4 legs, copy as curl, `docs/features.md:46`) |
| [26] Claude Code statusline | Have | separate repo `2lab-ai/llmux-statusline` |
| [27] named gateway keys with budgets and allowlists | Partial | per-computer client keys issue/suspend/rotate; no budgets |
| [28] remote instance: laptop uses the desktop's accounts | Have | remote mode (`.prd/13-remote-cli.md`); Islands needs an HTTPS proxy (`docs/remote.md:82-86`) |
| [29] Docker image, `daemon install`, LAN + TLS | **Missing** | no Dockerfile; llmux does not terminate TLS (`docs/remote.md:82`) |
| (Windows build) | **Missing** | release targets macOS + Linux only (`.github/workflows/release.yml:22-33`) |
| [34] web search for any model | **Missing** | llmux runs no search itself: a nameless server tool is refused (`src/provider/responses_request.rs:1217-1241`); a Codex CLI request served by Gemini drops web_search silently (`docs/provider-compatibility.md:218-220`); Claude Code WebSearch in a GPT session not measured |
| [39] self-update, signed builds | Have | `llmux channel` / `update`, brew stable + preview |
| [43] app/site/docs in 4–5 languages | Missing | English only |
| [3] profiles; [12] import from CC Switch/OpenCode; [36] agent CLI updater; [37] backup/sync; [38] per-account proxy | Missing | small; no demand cluster above 50 authors |
| [1][2] config editing for ~63 agents; [5] Chat/Gemini inbound | Out (thesis) | §6 |
| [17] intent routing classifier | Out | `01-spec.md:54` no task-content routing |
| [25] OTLP export | Out | `01-spec.md:50-53` no exporter |
| [33] MCP/skills/instructions library | Out | `01-spec.md:21` cross-CLI config sync is a non-goal |
| [30][31] plugins, JS middleware | Out (scope) | declarative payload rules preferred (04 note) |
| [32] PII privacy filter | Out (scope) | llmux masks logs and emails only |
| [42] daily check-in for free credits | Out | targets CN free plans, not llmux's market |

## 5. Where llmux already ships what magpie users raise

These match areas that magpie users raise often, and they already ship in llmux. [가설] They are
invisible mainly because they are unmarketed:
- **Quota display**: Islands tiles with 5h/7d windows and reset timers (macOS notch + KDE port).
- **Waste-aware scheduling**: perishability ranking burns the quota that resets soonest
  (`docs/configuration.md:100`). This is a different mechanism from #2111's staggered window
  start, which llmux does not have (roadmap item 7).
- **Central daemon + many machines**: remote mode with per-computer client keys. This is #979's
  request.
- **Multi-model agents**: Claude Code subagents on GPT or Grok inside one session
  (`README.md:41-119`).
- **Codex on Claude** through the official Agent SDK, and **Claude Code on GPT** with internal
  requests mapped (`.prd/02-architecture.md:357-400`).
- **Reset-credit redeem** for Codex and Claude (`.prd/20-claude-reset-grants.md`).
- **Loss-reporting headers / strict mode**: llmux reports omitted fields instead of hiding them.

## 6. Distribution: 17,487 vs 8 stars

| | magpie | llmux |
|---|---|---|
| Created → stars | 2026-09-23 → 17,487 (27 days) | 2026-06-12 → 8 (130 days) |
| Author reach | yetone, 9,614 followers; earlier hits avante.nvim (18,194★), openai-translator (25,006★) | none used |
| Website | usemagpie.ai, 4 languages, `curl … \| sh` installer | homepage = a GitHub doc page |
| README | badges, star-history chart, light/dark screenshots, Chinese README | no badges, PNG hero |
| Channels | DMG, Windows exe, Linux, Docker, Termux, `go install` | Homebrew tap only |
| Community | Discord, community plugin org, EN+ZH release notes | none |
| Release rhythm | 1,270 releases; peak 140/day (2026-10-05) | stable + per-push preview |

Inference [가설]: author reach and the timing of a Chinese-speaking audience explain much of the
first-week curve. The product surface (GUI-first, cross-platform, cheap-model tagline) explains why
the curve held. llmux can copy the second, not the first.

## 7. Deliberately not adopting

- **Config editing for ~63 agents and Chat/Gemini inbound.** This dissolves the one-harness thesis
  (04 note). 703 of 2,138 issues already sit on llmux's two harnesses. Revisit trigger: llmux's own
  issues ask for a third harness.
- **Intent routing, OTLP, a browser UI, an MCP/skills library.** These are existing non-goals
  (`01-spec.md:21,50-54`).
- **Plugins, JS middleware, PII model, free-credit check-in.** Large surface, low fit.

## Tensions

- **Non-goal vs top demand.** `01-spec.md:59` says "No production Gemini/local backends. Stub
  providers only." The largest demand cluster is exactly API-key and local backends. Roadmap item 1
  amends this non-goal when it starts. The CLIProxyAPI note already rated a generic
  OpenAI-compatible provider "highest ROI".
- **Compliance pressure cuts both ways.** magpie retired 13 built-in subscription integrations
  (11 vendors: Command Code, Cursor, Devin, Factory, Grok, Kiro, MiMo, Qoder, WorkBuddy, ZCode, Zed)
  into community plugins, because "reaching them can break their vendors' terms". The move
  "keeps magpie itself from being banned" (`AGENTS.md:26-29`; the move commits are dated from
  2026-10-02). API-key providers are
  [추론] likely to carry less vendor-policy risk than subscription pooling. Vendor terms were not
  reviewed for this note.
- **Counts are keyword-based.** Areas overlap and a few keywords are broad ("codex" appears as
  context in many bugs). Rankings were spot-checked against issue text, but exact numbers are
  approximate.
