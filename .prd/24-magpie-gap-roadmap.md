# llmux — Improvement roadmap from the magpie gap (2026-10-20)

Status: planned (2026-10-20). Nothing below is built yet. Evidence: [23-research-magpie.md](23-research-magpie.md).
Workstream driver: [magpie-gaps/](magpie-gaps/).

Owner's request (verbatim, 2026-10-20):

> llmux 개선 https://github.com/yetone/magpie 참조해서 using-dotprd로 이 magpie 리서치하고 분석하고
> 피쳐리스트를 추출해주고 llmux에 가장 부족한 피쳐를 개선리스트로 prd에 추개해줘 해당 repo에 유저들의
> 이슈를 확인해주고 어떤 기능들을 magpie유저들이 가장 좋아하는지 분석하고 그 부분을 우선으로 개선 계획을
> 세우고 시각화해서 보내줘 magpie에서 하는일이랑 llmux에서 하는일이 상당히 겹치는데 내가 마케팅으 ㄹ하나도
> 안한것도 있고

## The plan in one paragraph

Two tracks run in parallel. **Product** closes the gaps magpie users care most about and llmux
lacks: any API key (DeepSeek, Kimi, GLM, MiniMax, Qwen, local) behind Claude Code and Codex; quota
display polish; an explicit fallback chain from subscriptions to API keys; Docker and Windows.
**Distribution** makes the features llmux already has findable: positioning, README demo, a landing
page, and a launch timed to the day "Claude Code on DeepSeek" ships. Features magpie has but that
conflict with llmux's one-harness thesis stay out ([23 §7](23-research-magpie.md)).

## Priority rule

`score = demand × gap × fit ÷ effort`
- **demand** = distinct magpie issue authors whose issues match the item's keywords
  (`research/magpie-2026-10-20/items.txt`, via `items.py`).
- **gap** = 1 if missing in llmux, 0.5 if partial.
- **fit** = 1 if on-thesis, 0.5 if it strains the one-harness thesis.
- **effort** = 1 (S), 2 (M), 3 (L).

Scores rank the items. They are not forecasts. Item 3 depends on item 1, so it follows item 1
whatever its score.

| # | Item | Demand | Gap | Fit | Effort | Score | Wave |
|---|---|---|---|---|---|---|---|
| 1 | Any API key behind Claude Code and Codex | 268 | 1 | 1 | M | **134** | 1 |
| 2 | Quota display polish | 270 | 0.5 | 1 | S | **135** | 1 |
| 3 | Explicit fallback chain: subscriptions → API keys | 107 | 1 | 1 | S | **107** | 2 |
| 4 | Docker image + service install + built-in TLS | 84 | 1 | 1 | S | **84** | 2 |
| 5 | Windows build | 182 | 1 | 1 | L | **61** | 3 |
| 6 | Web search on non-Claude backends | 56 | 1 | 1 | M | **28** | 3 |
| 7 | Staggered window warm-up + auto-redeem | 27 | 0.5 | 1 | S | **14** | 3 |
| — | Third harness via Chat Completions inbound | 230 | 1 | 0.5 | L | 38 | not now |

Items 1 and 2 tie (134 vs 135). Item 1 goes first because item 3 cannot exist without it and it is
the headline the launch (D4) needs.

## Product track

### 1. Any API key behind Claude Code and Codex — "Claude Code on Kimi, Codex on DeepSeek"
- **What the user can do:** `llmux login --api-key <preset|url>` adds DeepSeek, Kimi/Moonshot,
  GLM (Zhipu/z.ai), MiniMax, Qwen (DashScope), Ollama/LM Studio, or any custom URL as an account.
  Its models then appear in Claude Code's `/model` picker and in `llmux run --codex`.
- **Two wire shapes.** (a) *Anthropic-compatible* endpoints need near-passthrough for Claude Code.
  Vendors publish these: `api.deepseek.com/anthropic`, `api.moonshot.ai/anthropic`,
  `open.bigmodel.cn/api/anthropic`, `api.z.ai/api/anthropic`, `api.minimaxi.com/anthropic`
  (magpie presets, 2026-10-20). (b) *OpenAI Chat Completions-compatible* endpoints need an
  `anthropic↔openai-chat` translator pair. This is the "highest ROI" item in the
  [CLIProxyAPI note](04-research-cliproxyapi.md). It serves Codex and vendors without an Anthropic
  endpoint.
- **Why:** this is the largest demand cluster (401 issues, 268 authors; DeepSeek alone in 194
  issues) and magpie's tagline. In llmux's own identity, "other models via API key" is Tier 1 and
  compliance-safe. Only the `Local` stub exists (`src/provider/stubs.rs:74-79`).
- **Non-goal change:** amend `01-spec.md:59` to "API-key providers over standard
  Anthropic/OpenAI-compatible endpoints are supported; no other local backends". Do this in the
  item's first PR.
- **Acceptance:**
  - With a DeepSeek key in env, `llmux login --api-key deepseek` → `claude --model <deepseek-model>`
    through llmux streams an answer that calls tools.
  - The TUI and `llmux accounts --json` list the account with group and auth type `api`.
  - A Chat-only vendor answers through `llmux run --codex -m <model>`. Any lost field is named in
    `X-Llmux-Omitted-Fields`.
  - The key is never printed: absent from logs, the raw viewer, and `--json` output.

### 2. Quota display polish — the surface magpie users touch most
- **What:**
  - (a) A remaining ↔ used toggle on Islands tiles and TUI gauges. magpie's most-discussed issue
    asks for this (#124, 35 comments).
  - (b) An opt-in notice when an account will reset with unused quota.
  - (c) API-equivalent USD per account for the current 5h/7d window (#1423).
  - (d) Islands on a remote daemon without a hand-built HTTPS proxy (#979: a central NAS daemon
    plus a local menu-bar display). This ships with item 4's TLS.
- **Why:** 270 authors. llmux already has the tiles (Islands, TUI), so this is polish.
- **Acceptance:**
  - The toggle persists across restarts and both values match `llmux status --json`.
  - A reset notice fires once per window, only for accounts above a configured unused share.
  - The USD figure equals the usage-tab cost summed over the window.

### 3. Explicit fallback chain: subscriptions → API keys
- **What:** opt-in, per group. Example: `claude: [pool] → deepseek-key`. When every account in a
  group is cooling down, the request goes to the next link instead of returning 429.
- **Never silent.** Respect the "no model laundering" line: send an `X-Llmux-Fallback: <provider>`
  response header, show a badge in Islands/TUI, and log a line. Default off.
- **Why:** 107 authors. #1418 (18 comments) is a routing group that could not reach its relay.
  Today llmux returns 429 + retry-after when a group is exhausted (`01-spec.md:131-134`).
- **Acceptance:**
  - With all Claude accounts paused and a fallback configured, a Claude Code turn returns 200 with
    `X-Llmux-Fallback: deepseek`, and Islands shows the badge.
  - With no fallback configured, behaviour is unchanged.
  - A stream that has already started never switches.

### 4. Docker image, service install, built-in TLS
- **What:** `ghcr.io/2lab-ai/llmux` (multi-arch), `llmux daemon install` (launchd/systemd), and
  optional TLS on the remote listener so Islands can connect directly.
- **Why:** 84 authors (Docker/NAS/headless). llmux has no Dockerfile and does not terminate TLS
  (`docs/remote.md:82-86`).
- **Acceptance:**
  - `docker run` with a mounted config dir serves `/llmux/status` 200.
  - Islands connects to `https://<host>:<port>` with a client key and no reverse proxy.

### 5. Windows build
- **What:** the CLI, daemon and TUI on Windows x64/ARM64, with credential import from Windows
  paths, a background service, and release + winget/scoop channel. Islands stays macOS/KDE.
- **Why:** 182 authors mention Windows/WSL. Release targets today are macOS + Linux only
  (`.github/workflows/release.yml:22-33`).
- **Acceptance:** on a clean Windows VM, install → `llmux login` → Claude Code and Codex route
  through the daemon; CI runs the test suite on `windows-latest`.

### 6. Web search on non-Claude backends
- **What:** when a Claude Code WebSearch tool request is routed to GPT, Grok or an API-key model,
  llmux runs the search through a configured provider (vendor-native search where it exists, else a
  search API key) instead of dropping the tool.
- **Why:** 56 authors. Today web_search tools are "left out silently" on the Codex path
  (`docs/provider-compatibility.md:220`).
- **Acceptance:** a GPT-routed Claude Code turn that asks for a web search returns cited results;
  with no search provider configured, the omission is named in `X-Llmux-Omitted-Fields`.

### 7. Staggered window warm-up + auto-redeem
- **What:**
  - (a) Opt-in daily warm-up that starts each account's 5h window at `anchor + i × 5h / N`, so
    resets are staggered (#2111).
  - (b) Opt-in automatic redeem of a reset credit before it expires (#624, #2078).
- **Why:** 27 authors; partly answered already by the perishability scheduler and idle probe
  (`docs/configuration.md:100-124`).
- **Acceptance:** with 3 accounts and anchor 08:00, `llmux status --json` shows window starts
  about 100 minutes apart. Warm-up spends at most one minimal request per account per day.

### Not now: a third harness
- OpenCode, Pi, Cursor and other agents together reach 230 authors (339 issues name only them).
  Serving them means a Chat Completions inbound, which strains the one-harness thesis (04 note).
- Revisit when llmux's own issue tracker asks for a specific third harness, or when item 1's
  translator makes the inbound direction cheap.

## Distribution track

The research found that most of what magpie users love already ships in llmux
([23 §5](23-research-magpie.md)). Baseline on 2026-10-20: 8 stars, 426 release-asset downloads in
total, 49 repo views from 15 unique visitors in 14 days.

| # | Item | Acceptance (observable) |
|---|---|---|
| D1 | **One-line positioning + honest comparison.** Example: "Every subscription and API key you own, behind the harness you already use — pooled, reset-aware, never silent about what it changed." Adds a llmux / magpie / CLIProxyAPI table to `docs/why-llmux.md`. | README first screen states it; the table cites only checked facts |
| D2 | **README that sells in 10 seconds.** A 20–30 s hero GIF (Islands + a Claude Code model switch), badges, star history, install one-liner, Korean and Chinese READMEs. | GIF renders on github.com; links resolve |
| D3 | **Landing page** (e.g. dosi.dev/llmux): pitch, demo video, three differentiators (reset-aware pooling, multi-model subagents, remote daemon with client keys), install. | URL returns 200 with the video playing |
| D4 | **Launch, timed to item 1 shipping stable.** Video post on X and Threads, GeekNews, Show HN, r/ClaudeAI and r/codex, V2EX / LINUX DO (magpie's audience writes 72% CJK). | Owner publishes; external posts are owner-gated |
| D5 | **Community + response loop.** GitHub Discussions, issue templates, and the existing agent-triage/agent-resolve loop aimed at same-day answers (magpie's median is 5.1 h to close). | Median first response < 24 h over 30 days |
| D6 | **Install friction.** A `curl … \| sh` installer next to brew; Docker (item 4); Windows (item 5). | Clean-machine install < 60 s on macOS/Linux |

Measure weekly: stars, release-asset downloads, unique repo visitors, issues from non-owner users.

## Waves

1. **Wave 1:** item 1 (API keys) + item 2 (quota polish); prepare D1, D2.
2. **Wave 2:** item 3 (fallback chain) + item 4 (Docker/TLS); ship D3 and launch D4 on item 1's
   stable release.
3. **Wave 3:** item 5 (Windows), item 6 (web search), item 7 (warm-up), D5, D6.

## Tensions

- Item 1 reverses a written non-goal (`01-spec.md:59`). The amendment is scoped to standard
  compatible endpoints so that "no hosted brokerage" and "no task-content routing" stay intact.
- Item 3 must not become model laundering. Default off, a header and a badge on every fallback
  turn, and no mid-stream switch.
- D4 is outward-facing and runs only when the owner publishes. Plan it, do not send it.
- Demand numbers are keyword counts over one competitor's tracker
  ([23 §2](23-research-magpie.md)). Re-rank once llmux has its own user issues.
