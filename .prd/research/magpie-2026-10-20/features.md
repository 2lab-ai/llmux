# magpie feature inventory vs llmux (2026-10-20)

All 44 user-facing features found in `yetone/magpie` (shallow clone 2026-10-20), each graded against
llmux `main`. Companion to [23-research-magpie.md §4](../../23-research-magpie.md), which keeps
only the rows the roadmap acts on.

How the magpie side was read: README, `docs/reference.md` (REF), `docs/subsystems/` (S), `internal/`
(I) and `plugins/` (P). Nothing was built or run. "implemented" = a doc and a source package both
exist. Rows marked **checked** were re-read by the dispatcher in the source. The others come from
one delegated source read and were not re-read.

llmux grades: **Have** · **Partial** · **Missing** · **Out** (conflicts with a non-goal or with
llmux's "keep your harness, swap the model" bet, 23 §7) · **N/A** (does not apply to a proxy).

| id | magpie feature | category | magpie evidence | magpie status | llmux | llmux note |
|---|---|---|---|---|---|---|
| 1 | Pick any provider/model for any agent; the agent's config is edited in place | config | README; I/edit, I/agent | implemented | Partial | for its two frontends llmux wires the endpoint and fills the model list per session — `llmux run` (Claude Code `/model` picker, `.prd/17-claude-code-model-picker.md`) and `llmux run --codex` (`.prd/21-codex-frontend.md`), both also from Islands "Start coding"; it does not rewrite other agents' config files |
| 2 | About 63 agents wired; only installed ones shown | config | REF §Agents | implemented | Partial | two first-class frontends, Claude Code and Codex CLI (`docs/features.md` "Two client frontends", 2026-10-07); any other Anthropic Messages or OpenAI Responses client by hand (`README.md`: "other Messages clients and Codex are served") — roadmap items 5, 7 |
| 3 | Profiles: save and switch every agent's setup at once | config | REF §Use; I/profile | implemented | Missing | small; not on the roadmap |
| 4 | Per-agent model visibility list | config | I/cli/visible_cli.go | implemented | Partial | one curated catalog for `/model`, not per agent (`.prd/17-claude-code-model-picker.md`) |
| 5 | Gateway translation across Chat/Responses/Messages/Gemini; count_tokens; speech-to-text; Ollama-style `/api/tags` | routing | REF; I/gateway | implemented | Partial | Messages + Responses inbound (`docs/provider-compatibility.md:15-21`); no Chat Completions inbound (roadmap item 7), no Gemini inbound; count_tokens is a labelled local estimate (`01-spec.md:65-68`) |
| 6 | 86 presets, custom URLs, custom headers, send the agent's own User-Agent | providers | `internal/provider/presets.go` | implemented (**checked**: 86) | Partial | Anthropic key + OpenRouter key, any OpenRouter model (`docs/configuration.md:209`); no direct vendor keys or custom URL — roadmap items 2, 6 |
| 7 | Live model lists from vendors + models.dev metadata, auto-refresh, retired models hidden | providers | REF; I/catalog | implemented | Partial | curated catalog with an `or-<vendor>/<slug>` escape hatch (`docs/models.md:146-150`) |
| 8 | Claude subscription served through the real local `claude` binary, tools bridged over MCP | providers | S/claude-subscription-bridge.md; I/claudebridge | implemented (**checked**: package exists) | Have | Codex → Claude through the official Agent SDK (`.prd/21-codex-frontend.md`) |
| 9 | ChatGPT, Copilot, Gemini Code Assist, Antigravity sign-ins as providers | providers | REF §Signed-in agents | implemented | Partial | ChatGPT/Codex yes, Gemini via Antigravity preview, no Copilot |
| 10 | Grok, Cursor, Devin, Factory, Kiro, Qoder, WorkBuddy, Zed, ZCode, MiMo, Command Code subscriptions — moved to community plugins | providers | `AGENTS.md:26-46` | partial (**checked**: 13 entries) | Partial | Grok is in llmux core (device-code login); the rest do not apply |
| 11 | Several accounts per subscription; reads existing sign-ins; imports account files from codexbar, CLIProxyAPI, Sub2API, Cockpit Tools | auth | REF | implemented | Partial | 6 login types, 5 import sources (`docs/operational-reference.md:13-14`); the third-party import formats are missing |
| 12 | Imports provider configs from CC Switch, Claude Code, Codex, Alma, OpenCode; "Add to magpie" links | install | REF §Import links | implemented | Missing | small; not on the roadmap |
| 13 | Routing groups (smart/order/rotate/usage/pace/weight), nested, automatic groups | routing | REF §Routing groups; P/groups, P/rotation | implemented | Partial | model→group routing + `default`/`round-robin` scheduler (`README.md:179-185`, `docs/schedulers.md`) |
| 14 | Failover on quota: 429 rests the account until reset, next account before any bytes | routing | REF §How magpie picks an account | implemented | Have | `01-spec.md:131-134` |
| 15 | A conversation stays on the answering account while its prompt cache is worth keeping | routing | REF | implemented | Have | session stickiness with a perishability override, prompt-cache locality named (`01-spec.md:32-34`) |
| 16 | Pin a request to one account by header; replies name provider and model in headers | routing | REF | implemented | Partial | pin/pause and manual switch in the TUI (`docs/operational-reference.md`); no per-request pin header; headers name omitted fields, not the serving account |
| 17 | Intent routing by a classifier model | routing | README; v0.1.95 | implemented | Out | `01-spec.md:54` no task-content routing |
| 18 | Ask another account again after a safety-filter refusal | routing | REF | implemented | Missing | not found; llmux fails over on Claude SDK account policy errors (#189), which is a different case |
| 19 | Outside the gateway, switch Claude Code's or Codex's own sign-in at 98% use | auth | REF | implemented | N/A | llmux is the gateway; it switches accounts per request instead |
| 20 | Daily warm-up that starts 5h windows at chosen times | cost | `plugins/warmups/` | implemented (**checked**: plugin exists) | Partial | perishability scheduler + idle probe pick among open windows; no scheduled start — roadmap item 10 |
| 21 | `quota`/`quota wait`, menu-bar quota cards, reminder before unused quota resets | observability | I/cli/quota_*.go | implemented | Partial | `status --json`, Islands tiles, TUI gauges; no reset reminder, no remaining/used toggle — roadmap item 1 |
| 22 | Usage and cost: tokens, cache, reasoning, USD/CNY, custom prices, tok/s, by key/account/agent, CSV/JSON | cost | README; I/usage | implemented | Have | `.prd/14-usage-calendar-stats.md`, perf tab |
| 23 | Sessions with cost and title; reopen in terminal; relocate sessions after a folder rename | observability | docs/session-relocation.md; I/sessions | implemented | Partial | TUI sessions tab |
| 24 | Live routing view; context-window inspector | observability | README; S/gui-shell.md | implemented | Partial | raw request viewer (`docs/features.md:46`); no context-window inspector |
| 25 | OTLP traces and metrics export | observability | REF §OTLP; I/otlp | implemented | Out | `01-spec.md:50-53` no exporter |
| 26 | `magpie statusline` for Claude Code | observability | I/cli/statusline_cli.go | implemented | Have | separate repo `2lab-ai/llmux-statusline` |
| 27 | Named gateway keys for LAN clients with token/cost budgets and model/account allowlists | auth/cost | REF; I/gwbudget, P/budget | implemented | Partial | per-computer client keys issue/suspend/rotate; no budgets or allowlists |
| 28 | Remote magpie: a laptop uses the desktop's providers, groups and quotas | other | I/provider/remote_quotas.go | implemented | Partial | remote mode for the CLI (`.prd/13-remote-cli.md`); Islands needs a hand-built HTTPS proxy (`docs/remote.md:82-86`) — roadmap item 8 |
| 29 | Headless serve/web with LAN + TLS, daemon install, Docker, Termux | install | REF §Install, §Docker | implemented | Missing | daemon auto-start exists (`01-spec.md:34`); no Docker image, service install or TLS — roadmap item 8 |
| 30 | Plugins on Bun: OpenCode auth plugins and pi packages become providers | providers | I/plugin, I/pluginrt | implemented | Out | scope (23 §7) |
| 31 | JavaScript gateway middleware + New-API-compatible packages | other | S/gateway-middleware.md; P/middleware | implemented | Out | scope; declarative payload rules preferred (04 note) |
| 32 | Privacy masking of secrets/personal data; optional local privacy-filter model | other | I/redact, I/pii | implemented | Out | scope; llmux masks logs and emails only |
| 33 | Library: instructions, MCP servers, skills, subagents, hooks written into every agent's format | config | S/library.md | implemented | Out | `01-spec.md:21` cross-CLI config sync is a non-goal |
| 34 | Web search for any model (Tavily, Brave, Exa…), `web_search`/`web_fetch` over MCP | other | P/search | implemented | Missing | roadmap item 9 |
| 35 | Image tool over MCP for any agent; video through a Grok subscription | other | I/imagemcp | implemented | Missing | not on the roadmap (94 authors mention web search or image together; not split) |
| 36 | Show each agent CLI's version and update it the way it was installed | install | README | implemented [not checked in source] | Missing | small; not on the roadmap |
| 37 | Backup and sync: file, WebDAV, S3, encrypted GitHub repo | config | S/sync-backup.md; I/davsync | implemented | Missing | small; not on the roadmap |
| 38 | A proxy per key or account; installer/updater `--proxy`/`--mirror` | install | I/netproxy; I/update/mirror.go | implemented | Missing | small; not on the roadmap |
| 39 | Self-update; signed and notarised Mac builds | install | REF §Install | implemented | Have | `llmux channel` / `update`, brew stable + preview |
| 40 | Wires agents inside WSL on Windows | config | I/wslrun | implemented | N/A | no Windows build yet — roadmap item 4 |
| 41 | Keep-awake while agents work, autostart, Omarchy theming, light/dark | UI | I/awake, I/autostart, I/omarchy | implemented | Partial | daemon auto-start only; no keep-awake |
| 42 | Automatic daily check-in for free credits | cost | REF | implemented | Out | targets CN free plans |
| 43 | App in en/zh/zh-TW/ja/de; site in 4 languages; EN+ZH release notes | UI | I/gui/assets/i18n.js | implemented | Missing | English only; KO/ZH READMEs are distribution item D2 |
| 44 | Daily anonymous usage ping, can be turned off | other | README §Privacy | implemented | N/A | telemetry, not a user feature |

Totals over 44 rows: Have 6 · Partial 18 · Missing 10 · Out 7 · N/A 3 (sum 44).

magpie's own GUI (tray panel, desktop window, browser UI) is the surface behind several rows (21, 23,
24); llmux has no GUI window, only the TUI and Islands. That gap is roadmap item 1.
