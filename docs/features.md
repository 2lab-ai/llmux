# What ships today

The complete feature list, with dates on behavior changes. Start at the root
[README](../README.md) for the short version.

![llmux live session (recorded 2026-07, TUI dashboard with email masking)](../screenshots/llmux-live-session.gif)

[Original live-session recording](../screenshots/llmux-live-session.mp4) (recorded 2026-07, TUI dashboard with email masking)

## See every subscription's remaining usage

- **llmux Islands**, a native macOS menu-bar/notch companion plus an Arch Linux/KDE Qt/Kirigami port for glanceable usage, request receipts, and screen-share-safe email masking. See [llmux-islands.md](llmux-islands.md), [the Linux build guide](../llmux-islands-linux/README.md), and [the cross-platform design/evidence dossier](../.prd/docs/llmux-islands-linux-port/README.md). The first-run setup screens and the **Start coding** launcher shipped 2026-10-07 (preview) and are on stable since v0.2.25 (2026-10-08); see [release availability](operational-reference.md#release-availability).
- **Detached daemon + live TUI dashboard** for quota windows (incl. Fable weekly / Grok rate-limit gauges), account health, routing, and manual switch.
- **Calendar usage & cost tab** (2026-07-15): hourly / daily / monthly buckets × model with all four token classes and API-equivalent USD cost, ledger-aligned amounts, replayed from the persisted request history — `U` in the dashboard. See [operational-reference.md](operational-reference.md#usage-tab-calendar-usage--cost).
- **Perf tab: observed provider/model performance** (2026-07-17): passive per-request timing (TTFB, first streamed delta, estimated post-delta throughput) folded into daily tokens/sec stats per `(provider, model, codex-fast)` — braille chart, date×provider health matrix (requests / error% / latency / e2e / est), single-day drill-down, and a `t/s` column on every activity row and session. Quiet days render as gaps, low samples dim, client disconnects never poison a provider's error rate — `p` in the dashboard. See [operational-reference.md](operational-reference.md#perf-tab-observed-performance) and [`.prd/15`](../.prd/15-perf-telemetry-config-editor.md).
- **Endpoint-origin Activity backgrounds** (2026-10-07): incoming OpenAI requests use dark gray (RGB(40,40,40) in the TUI), independently of the served model; Anthropic requests keep the existing background. Origin survives persistence, remote TUI and native Islands. See [activity rendering](codex-frontend/activity.md).

## Models working together inside your harness

- **Native multi-model agents in Claude Code** (measured 2026-10-08): a `.claude/agents/*.md` file with `model: gpt-6-astra[1m]` or `model: grok-4.7` runs as an ordinary Claude Code subagent and llmux routes it to the matching account; the parent collects the results. Project configuration, not a bundled command. See [multi-model agents](multi-model-agents.md).
- **Two client frontends** (2026-10-07): Anthropic Messages for Claude Code and OpenAI Responses for Codex CLI. `llmux run` launches Claude Code; `llmux run --codex` launches Codex with session-only provider/catalog settings. See [Codex frontend](codex-frontend/README.md).
- **Claude through Codex uses the official Claude Agent SDK**, pinned and installed lazily from embedded assets. Node.js 18+ and npm are needed on the daemon host. Client tools execute in Codex; full transcript/tool-result replay, structured output, process cancellation and shared account failover are supported with explicit compatibility limits.
- **Four backend groups** in one pool: **Claude** (subscription + API key), **Codex** (`gpt-*` / ChatGPT subscription), **Grok** (xAI device-code login + `grok-*` models), **OpenRouter** (2026-08-21: `llmux login --openrouter` browser PKCE — or `--paste` for a key you already have — plus `or-*` models, ten curated free rows and a verbatim escape hatch to the rest of the OpenRouter catalog).
- **Model-to-backend routing**: Claude-like names → Claude group; `gpt-*` / `codex` → Codex; `grok` / `grok-*` → Grok; `or-*` / bare `or` / `openrouter/*` → OpenRouter. Override via config routing tables.
- **Codex + Grok adapters** that accept Claude Code Messages requests and stream Anthropic-style SSE back (Responses-family upstreams under the hood).
- **OpenRouter passthrough for incoming Messages** (2026-08-21): OpenRouter serves the Anthropic Messages format natively, so that path needs no protocol translation: llmux rewrites the `model` (`or-ox-alpha` → `stealth/ox-alpha`) and removes unsigned foreign thinking before forwarding. See [models.md](models.md#alias-semantics) and the design record in [`openrouter/spec.md`](openrouter/spec.md).
- **Reasoning-effort override with bypass** (2026-07-15, behavior change): a configured `codex.reasoning_effort` / `grok.reasoning_effort` now **OVERRIDES** the client's `output_config.effort`; leave it unset (or cycle the dashboard's group-settings bar to `bypass`) to let the client's value ride through. Previously a configured effort was only a fallback the client always outranked — if you relied on that, clear the setting to restore it. The codex cycle now includes `max` (native on gpt-5.6, clamps to `xhigh` below). The grok cycle (2026-08-26) is `bypass → none → low → medium → high → xhigh → bypass`; `xhigh` reaches the wire on models whose level set has it (`grok-4.6`) and clamps to `high` on those that do not (`grok-4.5`).
- **Model catalog API**: `GET /models` and `GET /llmux/models` — curated ids, aliases, efforts, `max_context`, group; `GET /v1/models` returns the OpenAI-shaped list. See [models.md](models.md).

## Account selection before quota expires

- **Multi-account scheduling** with perishable-quota scoring (`default`) or sticky exhaust (`round-robin`), Fable weekly ceilings, and 429 cooldown parking — not manual account juggling. Selection happens within a provider group; llmux never substitutes GPT for Claude. See [schedulers](schedulers.md).

## Operate it

- **One Rust binary, `llmux`**, with daemon, login/import, dashboard, status, account management, channel/update, and Claude Code/Codex launch (`run` / `run --codex`).
- **Browser logins from the dashboard** (2026-08-26): `n` in the accounts overlay opens a provider picker — Claude (Anthropic OAuth), Codex (ChatGPT OAuth), Grok (xAI device code: verification URL + user code, best-effort browser open, then polling), OpenRouter (PKCE minting an `sk-or-v1-…` key). The flow runs in the client and the minted credential is injected into the daemon (`POST /llmux/inject-account` when attached), so no restart and no shell round-trip. A client that cannot open a browser is told to run `llmux login` instead of being left on a flow that would hang.
- **Mouse-editable config tab** (2026-07-17): every config leaf classified live-editable / restart-required / read-only-with-reason (machine-enforced against the schema — a new setting fails the build until classified), click-to-edit value cells, blast-radius confirm gates, and persist-first apply with a typed ack so "saved but needs restart" can never masquerade as applied. Works identically attached to a remote daemon. See [configuration.md](configuration.md) and [`.prd/15`](../.prd/15-perf-telemetry-config-editor.md).
- **Stable + preview channels** via Homebrew (`llmux` / `llmux-preview`), with `llmux channel` and `llmux update`. See [operational-reference.md](operational-reference.md#channels-and-updating), and [see which features need preview](operational-reference.md#release-availability).
- **One central daemon, other computers as clients** (optional): `--remote host[:port]` or `remote.host` in config drives one central daemon from many machines (Tailscale/WireGuard). See [the remote daemon guide](remote.md).
- **Per-computer client keys** (2026-08-06), for one person's machines: `llmux key new --name <pc> [--email …] [--admin]` issues per-machine `lmk-` keys (shown once, stored hashed), so several PCs share one daemon while usage is attributed per computer; suspend/resume/revoke/rotate bite on the next request without a restart, and the `/llmux/*` control plane now requires an admin credential even from localhost. See [operational-reference.md](operational-reference.md#multi-tenant-client-keys).

## See what was actually sent to the model

- **DevTools-style raw request viewer** (2026-07-15): the activity feed's `🔍 request` line opens every captured leg of an exchange — client request, rewritten upstream request, verbatim upstream response, delivered response — with scrolling, `copy` / `copy as curl` / `save` actions. The Claude SDK upstream leg records the bridge transport, not the SDK’s private HTTP bytes. See [the accidental AI debugger](ai-debugger.md) and [operational-reference.md](operational-reference.md#raw-requestresponse-viewer).
- **Captured system prompts**: the system prompts collected in [system-prompts/README.md](system-prompts/README.md) come from the same wire the raw request viewer opens.
