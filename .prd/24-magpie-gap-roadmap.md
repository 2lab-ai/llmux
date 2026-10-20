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

Two tracks run in parallel. **Product** closes the gaps magpie users raise most and llmux lacks.
In order: quota display polish, direct vendor keys and coding plans (DeepSeek, Kimi, GLM, MiniMax,
Qwen), an explicit fallback chain from subscriptions to API-key backends, then Windows, local models
and relays, and Docker with remote Islands. **Distribution** makes findable what already ships. magpie's
tagline, "Codex on DeepSeek, Claude Code on Kimi", is already possible in llmux through OpenRouter
(`or-<vendor>/<slug>` from both frontends, [23 §5](23-research-magpie.md)), but nobody knows it.
Features that conflict with llmux's one-harness thesis stay out ([23 §7](23-research-magpie.md)).

## Priority rule

`score = demand × gap × fit ÷ effort`
- **demand** = mention authors: distinct magpie issue authors whose issue text matches the item's
  keywords (`research/magpie-2026-10-20/items.txt`, via `items.py`). This measures how much
  attention and trouble an area draws, not preference or usage. Bug reports count like requests.
  Some keyword sets are wider than the item (noted per item).
- **gap** = 1 if missing in llmux, 0.5 if partial.
- **fit** = 1 if on-thesis, 0.5 if it strains the one-harness thesis.
- **effort** = 1 (S), 2 (M), 3 (L). Grades are estimates; re-estimate before an item starts.

Scores are a heuristic ranking, not a measured forecast.

| # | Item | Demand | Gap | Fit | Effort | Score | Wave |
|---|---|---|---|---|---|---|---|
| 1 | Quota display polish | 270 | 0.5 | 1 | S | **135** | 1 |
| 2 | Direct vendor keys and coding plans (Anthropic-compatible endpoints) | 225 | 0.5 | 1 | S | **113** | 1 |
| 3 | Explicit fallback chain: subscriptions → API-key backends | ≤107 | 1 | 1 | S | **≤107** | 1 |
| 4 | Windows build | 178 | 1 | 1 | L | **59** | 2 |
| 5 | Local models and relays (Chat Completions endpoints) | 97 | 1 | 1 | M | **49** | 2 |
| 6 | Docker image, service install, built-in TLS, Islands to a remote daemon | 84 | 1 | 1 | M | **42** | 2 |
| 7 | Web search on non-Claude backends | 56 | 1 | 1 | M | **28** | 3 |
| 8 | Staggered window warm-up + auto-redeem | 27 | 0.5 | 1 | S | **14** | 3 |
| — | Third harness via Chat Completions inbound | 230 | 1 | 0.5 | L | 38 | not now |

Waves group by rank and theme: wave 1 is what users touch and pay for, wave 2 is where llmux runs
and what it can reach, wave 3 is polish.

**Market default.** The vendor set (DeepSeek, Kimi, GLM, MiniMax, Qwen) and the launch channels
lean CJK-first, because that is where the measured demand is (72% of magpie issues contain CJK). The
alternative is EN-first: OpenRouter, Ollama, Groq and the OpenAI API first, HN and Reddit first. The
owner can flip this default.

## Product track

### 1. Quota display polish — the surface magpie users discuss most
- **What:**
  - (a) A remaining ↔ used toggle on Islands tiles and TUI gauges. magpie's most-discussed issue
    asks for this (#124, 35 comments).
  - (b) An opt-in notice when an account will reset with unused quota.
  - (c) API-equivalent USD per account for the current 5h/7d window (#1423).
- **Why:** 270 authors mention quota or usage display at all. The three pieces come from #124 and
  #1423, not from separate counts. llmux already has the tiles (Islands, TUI), so this is polish.
- **Acceptance:**
  - The toggle persists across restarts, and both values match `llmux status --json`.
  - A reset notice fires once per window, only for accounts above a configured unused share.
  - The USD figure equals the usage-tab cost summed over the window.

### 2. Direct vendor keys and coding plans — "Claude Code on Kimi, Codex on DeepSeek", without OpenRouter
- **What the user can do:** `llmux login --api-key <preset|url>` adds DeepSeek, Kimi/Moonshot, GLM
  (Zhipu/z.ai), MiniMax, Qwen, or a custom Anthropic-compatible URL as an account. This covers
  vendor coding plans that OpenRouter does not resell. Its models then appear in Claude Code's
  `/model` picker and in `llmux run --codex`.
- **What exists:** llmux takes an Anthropic key and an OpenRouter key
  (`docs/operational-reference.md:13`). Through OpenRouter, `or-<vendor>/<slug>` reaches about 400
  models (`docs/configuration.md:209`), from Claude Code natively and from Codex via
  Responses → Messages (`docs/provider-compatibility.md:20`). That is why the gap is 0.5. What is
  missing: direct vendor keys and plans, presets, and a custom URL (`Local` is a stub,
  `src/provider/stubs.rs:74-79`).
- **Shortest path [추론]:** generalize the OpenRouter provider. It is already a passthrough for an
  Anthropic Messages endpoint, with a configurable base URL (`openrouter.upstream`) and a Bearer key
  (`src/provider/openrouter.rs:183-187,218-250`; `docs/openrouter/spec.md`). The vendors publish
  Anthropic-compatible endpoints: `api.deepseek.com/anthropic`, `api.moonshot.ai/anthropic`,
  `open.bigmodel.cn/api/anthropic`, `api.z.ai/api/anthropic`, `api.minimax.io/anthropic`
  (magpie `internal/provider/presets.go:143-186`, 2026-10-20). Before each preset ships, live-probe
  its auth header and model ids.
- **Why:** 225 authors name these vendors or their coding plans (DeepSeek alone appears in 194
  issues). The CLIProxyAPI note calls a generic compatible provider "the biggest Tier-1 unlock"
  (`04-research-cliproxyapi.md:96`; "✅ highest ROI" at `:68`).
- **No non-goal change needed.** OpenRouter is already a production API-key backend on a hosted
  Anthropic-compatible endpoint, and direct vendor keys follow that precedent. Local servers need
  one (item 5).
- **Acceptance:**
  - With a DeepSeek key in env, `llmux login --api-key deepseek` → `claude --model <deepseek-model>`
    through llmux streams an answer that calls tools. The same model answers through
    `llmux run --codex -m <model>`.
  - The TUI and `llmux accounts --json` list the account with its group and auth type `api`.
  - The key never appears in logs, the raw viewer, or `--json` output.

### 3. Explicit fallback chain: subscriptions → API-key backends
- **What:** opt-in, per group. Example: `claude: [pool] → openrouter` today, `→ deepseek-key`
  after item 2. When every account in a group is cooling down, the request goes to the next link
  instead of returning 429.
- **Never silent.** Respect the "no model laundering" line: send an `X-Llmux-Fallback: <provider>`
  response header, show a badge in Islands/TUI, and log a line. Default off.
- **Why:** at most 107 authors. This is an upper bound: the keyword set includes plain 429 and
  "at capacity" errors, not only fallback requests. #1418 (18 comments) is a routing group that
  could not reach its relay. Today llmux returns 429 + retry-after when a group is exhausted
  (`01-spec.md:131-132`). The existing OpenRouter backend is a ready first target, so this item
  does not wait for item 2.
- **Acceptance:**
  - With all Claude accounts paused and a fallback configured, a Claude Code turn returns 200 with
    `X-Llmux-Fallback: openrouter`, and Islands shows the badge.
  - With no fallback configured, behaviour is unchanged.
  - A stream that has already started never switches.

### 4. Windows build
- **What:** the CLI, daemon and TUI on Windows x64/ARM64, with credential import from Windows
  paths, a background service, and a release + winget/scoop channel. Islands stays macOS/KDE.
- **Why:** 178 authors mention Windows or WSL (time-window phrases such as "5h windows" excluded;
  `research/magpie-2026-10-20/praise.txt`). Release targets today are macOS + Linux only
  (`.github/workflows/release.yml:22-33`).
- **Acceptance:** on a clean Windows VM, install → `llmux login` → Claude Code and Codex route
  through the daemon; CI runs the test suite on `windows-latest`.

### 5. Local models and relays — Chat Completions endpoints
- **What:** an `anthropic↔openai-chat` translator pair, so endpoints that only speak OpenAI Chat
  Completions work as accounts. This covers Ollama, LM Studio, vLLM and OpenAI-compatible relays.
- **Why:** 97 authors mention local models or relays. Nothing in llmux reaches them today.
- **Non-goal change, in this item's first PR.** `01-spec.md:59` ("No production Gemini/local
  backends. Stub providers only.") is already stale: OpenRouter is in production and Gemini is in
  preview. Replace it with: "Backends: Claude, Codex, Grok, OpenRouter, Gemini (preview), and API-key
  accounts on Anthropic- or OpenAI-compatible endpoints, local servers included. No hosted brokerage
  of other people's accounts."
- **Acceptance:** an Ollama model on `localhost` answers a Claude Code turn with a tool call through
  llmux. Any lost field is named in `X-Llmux-Omitted-Fields`.

### 6. Docker image, service install, built-in TLS, Islands to a remote daemon
- **What:** `ghcr.io/2lab-ai/llmux` (multi-arch), `llmux daemon install` (launchd/systemd), and
  optional TLS on the remote listener so Islands connects to a remote daemon without a hand-built
  HTTPS proxy (#979: a central NAS daemon plus a local menu-bar quota display).
- **Why:** 84 authors (Docker/NAS/headless). llmux has no Dockerfile and does not terminate TLS.
  Islands requires HTTPS for remote daemons (`docs/remote.md:82-86`). Effort M because built-in TLS
  brings certificate issuance and renewal.
- **Acceptance:**
  - `docker run` with a mounted config dir serves `/llmux/status` 200.
  - Islands connects to `https://<host>:<port>` with a client key and no reverse proxy, and shows
    the remote accounts' quota.

### 7. Web search on non-Claude backends
- **What:** when a Claude Code WebSearch tool request is routed to GPT, Grok or an API-key model,
  llmux runs the search through a configured provider (vendor-native search where it exists, else a
  search API key) instead of refusing or dropping the tool.
- **Why:** 56 authors; the keyword "搜索" also matches issues about magpie's own search box, so
  this is coarse. llmux has no search executor for non-Claude backends: a nameless server tool is
  refused (`src/provider/responses_request.rs:1217-1241`). A Codex CLI request served by Gemini
  drops web_search silently (`docs/provider-compatibility.md:218-220`). Step 0 measures what
  Claude Code's WebSearch does in a GPT session today (not yet measured).
- **Acceptance:** a GPT-routed Claude Code turn that asks for a web search returns cited results;
  with no search provider configured, the omission is named in `X-Llmux-Omitted-Fields`.

### 8. Staggered window warm-up + auto-redeem
- **What:**
  - (a) Opt-in daily warm-up that starts each account's 5h window at `anchor + i × 5h / N`, so
    resets are staggered (#2111).
  - (b) Opt-in automatic redeem of a reset credit before it expires (#624, #2078).
- **Why:** 27 authors. The perishability scheduler and idle probe (`docs/configuration.md:100-124`)
  pick among windows that are already open. They do not open windows on a schedule, which is what
  #2111 asks for.
- **Acceptance:** with 3 accounts and anchor 08:00, `llmux status --json` shows window starts
  about 100 minutes apart. Warm-up spends at most one minimal request per account per day.

### Not now: a third harness
- OpenCode, Pi, Cursor and other agents together reach 230 authors (339 issues name only them).
  Serving them means a Chat Completions inbound, which strains the one-harness thesis (04 note).
- Revisit when llmux's own issue tracker asks for a specific third harness, or when item 5's
  translator makes the inbound direction cheap.

## Distribution track

The research found that much of what magpie users raise already ships in llmux
([23 §5](23-research-magpie.md)). Baseline on 2026-10-20: 8 stars, 426 release-asset downloads in
total, 49 repo views from 15 unique visitors in 14 days.

| # | Item | Acceptance (observable) |
|---|---|---|
| D1 | **One-line positioning + honest comparison.** Lead with the harness and the keys, not with account pooling, which is the policy-fragile half ([23 Tensions](23-research-magpie.md)). Example: "Keep Claude Code or Codex. Run them on any model you pay for, and see every quota in one place." Adds a llmux / magpie / CLIProxyAPI table to `docs/why-llmux.md`. | README first screen states it; the table cites only checked facts |
| D2 | **README that sells in 10 seconds.** A 20–30 s hero GIF (Islands + a Claude Code model switch to an `or-` model), badges, star history, install one-liner, Korean and Chinese READMEs. | GIF renders on github.com; links resolve |
| D3 | **Landing page** (e.g. dosi.dev/llmux): pitch, demo video, three differentiators (reset-aware pooling, multi-model subagents, remote daemon with client keys), install. | URL returns 200 with the video playing |
| D4 | **Launch at the end of wave 1** (quota polish + direct DeepSeek/Kimi keys + fallback chain). Video post on X and Threads, GeekNews, Show HN, r/ClaudeAI and r/codex, V2EX / LINUX DO (72% of magpie issues contain CJK). | Owner publishes; external posts are owner-gated |
| D5 | **Community + response loop.** GitHub Discussions, issue templates, and the existing agent-triage/agent-resolve loop aimed at same-day answers (magpie's median is 5.1 h to close). | Median first response < 24 h over 30 days |
| D6 | **Install friction.** A `curl … \| sh` installer next to brew; Windows (item 4); Docker (item 6). | Clean-machine install < 60 s on macOS/Linux |

D1–D3 do not wait for any product item: the OpenRouter path and the quota tiles already exist.
Measure weekly: stars, release-asset downloads, unique repo visitors, issues from non-owner users.

## Waves

1. **Wave 1:** items 1, 2, 3; D1, D2, D3; launch D4 when wave 1 ships stable.
2. **Wave 2:** items 4, 5, 6; D5, D6.
3. **Wave 3:** items 7, 8.

## Tensions

- Item 5 rewrites a written non-goal (`01-spec.md:59`) that is already stale. The new sentence
  keeps "no hosted brokerage" and does not touch "no task-content routing" (`01-spec.md:54`).
- Item 3 must not become model laundering. Default off, a header and a badge on every fallback
  turn, and no mid-stream switch.
- D4 is outward-facing and runs only when the owner publishes. Plan it, do not send it.
- Demand numbers are keyword counts over one competitor's tracker
  ([23 §2](23-research-magpie.md)). Re-rank once llmux has its own user issues.
- The OpenRouter route to DeepSeek/Kimi models is documented, not live-verified for this note.
  D2's demo GIF is the live check.
