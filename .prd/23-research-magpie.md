# llmux — Research note: magpie (2026-10-20)

Status: research snapshot, 2026-10-20. Not a contract. The improvement plan built on it is
[24-magpie-gap-roadmap.md](24-magpie-gap-roadmap.md). Companion to the CLIProxyAPI note
[04-research-cliproxyapi.md](04-research-cliproxyapi.md).

## Conclusion first

[yetone/magpie](https://github.com/yetone/magpie) (Go, MIT, created 2026-09-23) does most of what
llmux does: pooled subscription accounts, quota-aware failover, a menu-bar quota display, a
remote/central mode. On top of that it takes direct keys from about 86 vendors and endpoints. Its
tagline is "Codex on DeepSeek, Claude Code on Kimi". It had 17,487 stars on 2026-10-20 after 27
days. llmux has 8 stars after 130 days (created 2026-06-12). The gap has three parts. The order is
a judgment [가설], not a measurement:

1. **Distribution.** Much of what magpie users raise already ships in llmux (§5). That includes the
   tagline experience itself: `or-<vendor>/<slug>` reaches about 400 OpenRouter models, DeepSeek,
   Kimi and GLM among them, from Claude Code and from Codex (`docs/configuration.md:209`,
   `docs/provider-compatibility.md:20`). It is documented, not live-verified for this note. None of
   it is findable: no website, no badges, no demo GIF in recent releases, English only, one install
   channel. magpie launched from an author with 9,614 followers and two earlier hits.
2. **Direct vendor keys and endpoints.** This is the largest backend-related mention cluster in
   magpie's tracker (client-agent support is larger overall, §3).
   llmux takes an Anthropic key (`login --api`) and an OpenRouter key (`login --openrouter --paste`,
   `docs/operational-reference.md:13`). It has no direct DeepSeek/Kimi/GLM/MiniMax/Qwen keys or
   coding plans, no local models (Ollama, LM Studio), no relays, and no custom URL. `Local` is a stub
   (`src/provider/stubs.rs:74-79`).
3. **Reach.** A GUI dashboard (159 authors name one of magpie's screens in an issue title), Windows
   (178 distinct issue authors), other agents (106 on protocols llmux already serves, 129 Chat-only),
   Docker/NAS (84), and a cross-provider fallback chain.

llmux already serves two client frontends: Claude Code over Anthropic Messages and Codex CLI over
OpenAI Responses (`docs/features.md`, "Two client frontends", 2026-10-07). An earlier draft of this
note called llmux a one-harness product; that came from the 2026-06 CLIProxyAPI note and was wrong
by 2026-10-07.

## 1. What magpie is

One Go binary. It (a) edits each coding agent's own config in place (about 63 agents) and (b) runs a
local gateway on `127.0.0.1:3425` that translates between OpenAI Chat, OpenAI Responses, Anthropic
Messages and Gemini, and routes across API keys and subscription accounts. Five front ends: menu-bar
tray, desktop window (Wails), TUI, browser UI, CLI. Headless: `serve`, `daemon install`, Docker,
Termux. macOS, Windows, Linux (x64 + ARM64). 86 provider presets
(`internal/provider/presets.go`, counted 2026-10-20). Free; income from relay sponsors.

| | llmux | magpie |
|---|---|---|
| Bet | keep your harness, swap the model (Claude Code and Codex first-class) | every agent, every model, one switcher |
| Inbound | Anthropic Messages + OpenAI Responses (Claude Code, Codex) | Chat, Responses, Messages, Gemini (~63 agents) |
| Backends | Claude (OAuth + API key), ChatGPT/Codex, Grok, OpenRouter (~400 models), Gemini preview | 86 presets, any OpenAI/Anthropic-compatible URL, local servers, 5 subscriptions |
| Surface | daemon + TUI + Islands (macOS notch, KDE port) + CLI; no GUI window | tray + window + TUI + web UI + CLI |
| Platforms | macOS, Linux (CLI) | macOS, Windows, Linux, Docker, Termux |
| Build process | agent-driven, gated PRs | "written, reviewed, merged and released by agents" (`LESSONS.md:4`); 1,270 releases in 27 days (counted in `yetone/magpie-releases`; the main repo keeps the latest 100) |

## 2. Evidence base and method

- **Source:** shallow clone of `yetone/magpie` at 2026-10-20 (README, `docs/reference.md`,
  `docs/subsystems/`, `internal/`, `plugins/`). Read, never built or run. "Implemented" below means a
  doc and a source package both exist; nothing was exercised.
- **Issues:** all 3,036 items via `gh api repos/yetone/magpie/issues?state=all` (snapshot
  2026-10-20 07:41Z, last issue #3038): 2,464 issues and 572 PRs. The owner filed 326 issues, so
  **user issues = 2,138 from 1,014 distinct authors**, filed 2026-09-24 → 2026-10-20. 72% of all
  issues contain CJK text (1,765/2,464).
- **"Loved" cannot be read from reactions.** All 2,464 issues together carry 33 reactions. The main
  proxy is **mention authors**: distinct authors whose title or body matches an area's keywords.
  Mention authors measure how much attention and trouble an area draws. They do not measure
  preference or usage share, and a bug report counts the same as a feature request. Each count is
  a keyword-matched set with both false positives and false negatives.
- **Explicit praise** is read separately (`praise.py`, word-boundary matching for English words).
  An earlier count matched "love" inside "failover" and wrongly made routing the most-praised area.
- **Classifier:** keyword areas over title, then body, multi-label; 33/2,138 unclassified.
  Reproduce: [research/magpie-2026-10-20/](research/magpie-2026-10-20/) (`classify.py`,
  `mentions.py`, `items.py`, `praise.py` and their `.txt` outputs). Use the counts for ranking, not
  as exact figures.

## 3. What magpie users raise most

| Area | Issues | Distinct authors | Feature requests | Signature issues |
|---|---|---|---|---|
| Client-agent support | 859 | 525 | 304 | Codex is mentioned in 537 issues, Claude Code in 287, OpenCode 147, Pi 105 |
| Providers / API keys | 614 | 387 | 248 | DeepSeek 194, relays 111, GLM 65, Kimi 59; #176 Bedrock (19 comments) |
| Usage & quota display | 382 | 250 | 155 | **#124 remaining-vs-used toggle (35 comments, most-discussed issue)**; #1423 USD per account |
| Subscription accounts | 351 | 240 | 120 | #2111 staggered 5h warm-up; #1630 quota card refresh |
| Routing & failover | 387 | 236 | 137 | #1418 "model at capacity" with no fallback (18 comments); #2077 multi-provider 429 |
| Platform / install | 249 | 191 | 61 | Windows/WSL: 289 issues, 178 authors (`praise.txt`, time-window phrases excluded); Docker/NAS/headless: 107 issues, 84 authors (`items.txt`) |
| Remote / LAN | 146 | 113 | 64 | #979 central NAS daemon + local menu-bar quota; #545 LAN sharing |
| Web search / image | 113 | 94 | 44 | #1399 image/video relay (22 comments); #669 search returns empty |
| MCP/skills library | 136 | 94 | 64 | out of llmux scope (§7) |
| Free-credit check-in | 42 | 37 | 15 | #694 Qoder, #808 Trae, #1001 ZCode (17–20 comments each) |

(Areas overlap; an issue can count in several. Full table: `research/magpie-2026-10-20/areas.txt`.)

What users praise by name (`praise.txt`): 81 issues contain a praise word, 58 of them only thank
the author, and 23 praise something specific. That is too few to rank. The named items are
multi-account management and dynamic routing (#1432), the growing list of supported plans together
with multi-account support (#840), checking usage in the floating window (#546), skills management
(#1734), custom request headers (#61) and the central management UI (#2957). They point to the same
areas as the mention counts.

Signals in plain words:
- Two harnesses dominate the tracker. 703 of 2,138 user issues name Codex or Claude Code, and 339
  name only other agents. [가설] Most magpie use sits on the two harnesses llmux already serves.
  Codex is named about twice as often as Claude Code.
- The provider cluster is about **cheap keys behind an existing harness**. 225 authors name
  DeepSeek, Kimi, GLM, MiniMax, Qwen or their coding plans, and 97 name local models or relays
  (`items.txt`).
- The single most-discussed issue is about the quota display (#124, 35 comments), and #979 says
  its author "deeply relies" on the menu-bar quota capsules.
- magpie answers fast: median 5.1 h from open to completed for user issues (n=1,065); feature
  requests completed in a median 2.1 h (376 of 803 completed).

## 4. Feature-by-feature gap matrix

Have = llmux ships it. Partial = a narrower version ships. Missing = not built. Out = conflicts with
an llmux non-goal or thesis (§7). Numbers in brackets = magpie inventory id. The full 44-row
inventory, with an llmux grade for every row (Have 6 · Partial 18 · Missing 10 · Out 7 · N/A 3), is
[research/magpie-2026-10-20/features.md](research/magpie-2026-10-20/features.md). This table keeps
the rows the roadmap acts on.

| magpie feature | llmux | llmux evidence / note |
|---|---|---|
| [6] 86 presets, any OpenAI/Anthropic-compatible URL, custom headers | **Partial** | Anthropic key + OpenRouter key (`docs/operational-reference.md:13`); any OpenRouter model via `or-<vendor>/<slug>` (`docs/configuration.md:209`); no direct vendor keys, no local, no custom URL (`Local` stub, `src/provider/stubs.rs:74-79`) |
| [13] user-defined routing groups (smart/order/rotate/pace/weight, nested) | Partial | model→group routing + in-group scheduler (`README.md:179-185`, `docs/schedulers.md`) |
| [14] quota failover before first byte, rest until reset | Have | `01-spec.md:131-134` |
| (cross-provider fallback when a group is exhausted) | **Missing** | all-exhausted → 429 + retry-after (`01-spec.md:132`) |
| [11] multiple accounts per subscription, import existing logins | Have | 6 login types, 5 import sources (`docs/operational-reference.md:13-14`) |
| [8] Claude subscription via the real Claude binary/SDK | Have | Codex→Claude through the Claude Agent SDK (`.prd/21-codex-frontend.md`) |
| [9] ChatGPT, Copilot, Gemini, Antigravity as providers | Partial | ChatGPT/Codex, Grok, OpenRouter; Gemini preview; no Copilot |
| [20] daily warm-up of 5h windows | Partial | perishability scheduler + idle probe (`docs/configuration.md:100-124`); no scheduled window start |
| [21] quota CLI, menu-bar quota cards, reminder before unused quota resets | Partial | `status --json`, Islands tiles, TUI gauges; no reset reminder, no remaining/used toggle |
| [22] usage and cost in USD, tok/s, by key/account | Have | `.prd/14-usage-calendar-stats.md`, perf tab |
| [23] sessions with cost and title, reopen | Partial | TUI sessions tab |
| [24] live routing view | Have | raw request viewer (4 legs, copy as curl, `docs/features.md:46`) |
| [26] Claude Code statusline | Have | separate repo `2lab-ai/llmux-statusline` |
| [27] named gateway keys with budgets and allowlists | Partial | per-computer client keys issue/suspend/rotate; no budgets |
| [28] remote instance: laptop uses the desktop's accounts | Partial | remote mode for the CLI (`.prd/13-remote-cli.md`); Islands needs a hand-built HTTPS proxy (`docs/remote.md:82-86`) |
| [29] Docker image, `daemon install`, LAN + TLS | **Missing** | no Dockerfile; llmux does not terminate TLS (`docs/remote.md:82`) |
| (Windows build) | **Missing** | release targets macOS + Linux only (`.github/workflows/release.yml:22-33`) |
| [34] web search for any model | **Missing** | llmux runs no search itself: a nameless server tool is refused (`src/provider/responses_request.rs:1217-1241`); a Codex CLI request served by Gemini drops web_search silently (`docs/provider-compatibility.md:218-220`); Claude Code WebSearch in a GPT session not measured |
| [39] self-update, signed builds | Have | `llmux channel` / `update`, brew stable + preview |
| [43] app/site/docs in 4–5 languages | Missing | English only |
| [3] profiles; [12] import from CC Switch/OpenCode; [36] agent CLI updater; [37] backup/sync; [38] per-account proxy | Missing | small; demand not measured for these |
| [1][2] model choice wired into ~63 agents | Partial | two first-class frontends: `llmux run` (Claude Code) and `llmux run --codex`, both also from Islands "Start coding"; other Messages/Responses clients by hand (`README.md`, "other Messages clients and Codex are served") — roadmap item 5 |
| [5] gateway across Chat/Responses/Messages/Gemini inbound | Partial | Messages + Responses inbound; no Chat Completions inbound (roadmap item 7), no Gemini inbound (Gemini CLI: 14 issues) |
| (GUI dashboard: tray panel, desktop window, browser UI) | **Missing** | TUI dashboard and Islands only; non-goal `01-spec.md:50-53` — roadmap item 1 |
| [17] intent routing classifier | Out | `01-spec.md:54` no task-content routing |
| [25] OTLP export | Out | `01-spec.md:50-53` no exporter |
| [33] MCP/skills/instructions library | Out | `01-spec.md:21` cross-CLI config sync is a non-goal |
| [30][31] plugins, JS middleware | Out (scope) | declarative payload rules preferred (04 note) |
| [32] PII privacy filter | Out (scope) | llmux masks logs and emails only |
| [42] daily check-in for free credits | Out | targets CN free plans, not llmux's market |

## 5. Where llmux already ships what magpie users raise

These match areas that magpie users raise often, and they already ship in llmux. [가설] They are
invisible mainly because they are unmarketed:
- **Claude Code and Codex on DeepSeek, Kimi, GLM and about 400 other models** through one
  OpenRouter key: `or-<vendor>/<slug>` (`docs/configuration.md:209`, `docs/models.md:148`), from both
  frontends (`docs/provider-compatibility.md:20`). This is magpie's tagline, minus direct keys.
- **Quota display**: Islands tiles with 5h/7d windows and reset timers (macOS notch + KDE port).
- **Waste-aware scheduling**: perishability ranking burns the quota that resets soonest
  (`docs/configuration.md:100`). It is a different mechanism from #2111's scheduled window start,
  which llmux does not have (roadmap item 8).
- **Central daemon + many machines**: remote mode with per-computer client keys. This answers the
  daemon half of #979. The menu-bar half still needs a reverse proxy (`docs/remote.md:84-86`).
- **Two harnesses, first-class**: Claude Code (`llmux run`) and Codex CLI (`llmux run --codex`)
  share one account pool and routing, and Islands "Start coding" opens either for a project folder.
- **Multi-model agents**: Claude Code subagents on GPT or Grok inside one session
  (`README.md:41-119`).
- **Codex on Claude** through the official Agent SDK, and **Claude Code on GPT** with internal
  requests mapped (`.prd/02-architecture.md:357-400`).
- **Reset-credit redeem** for Codex and Claude (`.prd/20-claude-reset-grants.md`).
- **Loss-reporting headers / strict mode**: llmux reports omitted fields instead of hiding them.

## 6. Distribution: 17,487 vs 8 stars

| | magpie | llmux |
|---|---|---|
| Created → stars | 2026-09-23 → 17,487 (27 days; snapshot 2026-10-20) | 2026-06-12 → 8 (130 days) |
| Author reach | yetone, 9,614 followers; earlier hits avante.nvim (18,194★), openai-translator (25,006★) | none used |
| Website | usemagpie.ai, 4 languages, `curl … \| sh` installer | homepage = a GitHub doc page |
| README | badges, star-history chart, light/dark screenshots, Chinese README | no badges, PNG hero |
| Channels | DMG, Windows exe, Linux, Docker, Termux, `go install` | Homebrew tap only |
| Community | Discord, community plugin org, EN+ZH release notes | none |
| Release rhythm | 1,270 releases (`magpie-releases`); peak 140/day (2026-10-05) | stable + per-push preview |

Inference [가설]: author reach and the timing of a Chinese-speaking audience explain much of the
first-week curve. The product surface (GUI-first, cross-platform, cheap-model tagline) explains why
the curve held. llmux can copy the second, not the first.

## 7. Deliberately not adopting

- **Editing other agents' config files.** magpie rewrites each agent's own config in place. llmux
  launches the harness pointed at itself instead (`llmux run`, `run --codex`, Islands "Start
  coding"). More agents come in through llmux's endpoints (roadmap items 5 and 7), not through config
  rewriting.
- **Gemini inbound.** Gemini CLI appears in 14 issues.
- **Intent routing, OTLP, a browser UI, an MCP/skills library.** These are existing non-goals
  (`01-spec.md:21,50-54`).
- **Plugins, JS middleware, PII model, free-credit check-in.** Large surface, low fit.

## Tensions

- **The largest raw area is client-agent support (525 authors).** llmux covers the two agents named
  most (Codex 537 issues, Claude Code 287). Of the rest, 106 authors name agents that already speak
  llmux's protocols (roadmap item 5) and 129 name Chat-only agents (item 7).
- **The non-goal is stale.** `01-spec.md:59` says "No production Gemini/local backends. Stub
  providers only." OpenRouter has been a production API-key backend since 2026-08-21
  (`docs/openrouter/spec.md`), and Gemini ships as preview. So direct vendor keys (roadmap item 2)
  follow OpenRouter's precedent and need no non-goal change. Local models (item 5) do need one. The
  04 note framed the generic provider as an OpenAI-compatible translator
  (`04-research-cliproxyapi.md:68`). The OpenRouter half has since shipped as a native Messages
  passthrough.
- **Compliance pressure cuts both ways.** magpie retired 13 built-in subscription integrations
  (11 vendors: Command Code, Cursor, Devin, Factory, Grok, Kiro, MiMo, Qoder, WorkBuddy, ZCode, Zed)
  into community plugins, because "reaching them can break their vendors' terms". The move
  "keeps magpie itself from being banned" (`AGENTS.md:26-29`; the move commits are dated from
  2026-10-02). [추론] API-key backends likely carry less vendor-policy risk than subscription
  pooling, so positioning should lead with "your harness, any key" rather than "pooled accounts".
  Vendor terms were not reviewed for this note.
- **Counts are keyword-based.** Areas overlap, and title+body matching both over- and under-counts.
  Some words are broad: "codex" appears as context in many bugs, "cursor" also matches UI
  cursors, and "搜索" also matches magpie's own search box. Rankings were spot-checked against issue
  text, but exact numbers are approximate.
