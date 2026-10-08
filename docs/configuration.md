# Configuration

> Most settings are editable live from the TUI's `config` tab (see [operational-reference](operational-reference.md)); rows marked `restart` there persist here and apply on the next daemon start.

llmux stores local configuration at `~/.config/llmux.json` by default. It respects `$XDG_CONFIG_HOME`, and `$LLMUX_CONFIG` can point at a different file.

The config file is written with mode `0600`. Updates use atomic read-merge-write so the daemon and CLI can safely change different parts of the config while llmux is running.

### Files beside the config

| Path | What |
|---|---|
| `~/.config/llmux/usage.sqlite3` | Durable per-tenant keys usage (the `K` tab's windows/filters). `llmux-preview` builds use `~/.config/llmux-preview/usage.sqlite3`, so the two channels never share history. |
| `<config dir>/<config stem>/usage.sqlite3` | Where the store moves when `$LLMUX_CONFIG` points at a non-default file — e.g. `LLMUX_CONFIG=/tmp/x/alt.json` → `/tmp/x/alt/usage.sqlite3`. An alternate config therefore never reads or writes your real history. |

The usage database is created `0600` inside a `0700` directory and stores
request METADATA only — timestamp, tenant id, backend group, served model,
status, token counts. No prompts, no responses, no credentials. Deleting it
loses only the keys tab's history (the daemon recreates it and re-imports
whatever `activity.jsonl` still holds); everything else keeps working.

## Example

```json
{
  "version": 1,
  "proxy": { "port": 3456, "api_key": "lm-..." },
  "upstream": "https://api.anthropic.com",
  "scheduler": {
    "five_hour_max": 0.90,
    "seven_day_max": 0.99,
    "usage_poll_secs": 300,
    "usage_max_age_secs": 600,
    "refresh_ahead_secs": 25200
  },
  "routing": {
    "enabled": true,
    "claude_models": [],
    "codex_models": [],
    "grok_models": [],
    "openrouter_models": [],
    "default_group": "claude",
    "on_empty_group": "error"
  },
  "claude_code": {
    "gpt_model_mapping": { "opus": "sol", "sonnet": "terra", "haiku": "luna" },
    "auto_classifier_model": "luna"
  },
  "codex": {
    "default_model": "gpt-5.6-sol",
    "fast": false
  },
  "openrouter": {
    "upstream": "https://openrouter.ai/api",
    "default_model": "stealth/ox-alpha"
  },
  "accounts": [
    {
      "name": "user@example.com",
      "type": "oauth",
      "account_uuid": "...",
      "access_token": "<oauth-access-token>",
      "refresh_token": "<oauth-refresh-token>",
      "expires_at_ms": 1774384968427
    }
  ]
}
```

## Proxy

| Key | Default | Meaning |
|---|---:|---|
| `proxy.port` | `3456` | Daemon port for both Messages and Responses. Claude Code uses `ANTHROPIC_BASE_URL=http://localhost:3456`; Codex uses the session provider at `http://localhost:3456/v1`. |
| `proxy.api_key` | generated | The shared ADMIN credential (`lm-…`): non-loopback clients must present it (or an issued client key) as `x-api-key` (or Bearer on OpenAI endpoints), and `/llmux/*` control endpoints require it (or an admin-kind client key) even from localhost. Keyless data-plane requests are loopback-only. Malformed, conflicting or unknown explicit OpenAI credentials are rejected even there. |
| `client_keys` | `[]` | Issued downstream client keys — per-computer attribution for one person's machines (see [remote daemon](remote.md)). Managed via `llmux key …` / `POST /llmux/keys/*` — each entry stores id, name, email, kind (`default`\|`admin`), key prefix, SHA-256 digest, suspended flag, and timestamps. The secret itself is never stored; edit this section by hand only for disaster recovery. |
| `upstream` | `https://api.anthropic.com` | Anthropic-compatible upstream base URL for Claude accounts. |

## Frontend launch and SDK runtime

`llmux run` selects Claude Code; `llmux run --codex` selects Codex CLI without a
persistent frontend toggle. `codex.default_model` provides the Codex launcher
default; explicit client `-m`/`-c` options override injected launch defaults.
Daemon-side routing and configured provider effort overrides still apply.
`--no-model-picker` disables the selected client’s catalog injection.

Claude-through-Codex needs Node.js 18+ and npm on the daemon host. The binary embeds
the pinned bridge program, manifest and lockfile and lazily installs the SDK into
a private OS cache. `LLMUX_NODE` selects Node; `LLMUX_CLAUDE_SDK_DIR` optionally
selects a preinstalled pinned bridge. Normal installs need neither override nor
a source checkout. The SDK receives the selected account and isolated settings,
not the daemon user’s Claude/Codex authentication environment. See
[bridge runtime](../bridge/README.md) and [remote mode](remote.md).

Ownership: the daemon host owns accounts, routing, scheduling and the SDK runtime; each client machine owns its own Claude Code or Codex settings, hooks, permissions and skills — llmux does not synchronize them between clients (a shared cross-CLI configuration is not a shipped feature). Per-agent models are a client-side setting too: a `.claude/agents/*.md` `model:` field is routed like any request ([multi-model agents](multi-model-agents.md)). `llmux env --codex` is preview-only as of 2026-10-08 ([release availability](operational-reference.md#release-availability)).

## Scheduler knobs

Each account tracks 5-hour and 7-day quota windows. The scheduler chooses among eligible accounts with a perishability-aware score: burn quota that will reset soon while preserving long-runway accounts.

| Key | Default | Meaning |
|---|---:|---|
| `five_hour_max` | `0.90` | Max 5-hour utilization before an account is ineligible. |
| `seven_day_max` | `0.99` | Max 7-day utilization before an account is ineligible. |
| `usage_poll_secs` | `300` | Per-account OAuth usage poll interval. |
| `usage_max_age_secs` | `600` | Usage older than this is stale; stale accounts are skipped unless all are stale. |
| `refresh_ahead_secs` | `25200` | Background refresh threshold; default 7 hours before token expiry. |

See [the scheduler perishability design](../.prd/09-scheduler-perishability.md) for the derivation and edge cases.

## Idle probe (cold-account refresh)

The OAuth usage poller covers Claude subscription accounts only. Codex and
API-key accounts get their 5h/7d gauges from a gated `max_tokens = 1` probe
through their own credential (`proxy.idle_probe`), delivered on demand (real
traffic to the group) and by a background timer sweep. Since 2026-07-15 the
probe also re-fires when an account's freshest window observation goes
**stale**, so cold subscriptions keep live gauges instead of freezing at
their first reading.

| Key | Default | Meaning |
|---|---:|---|
| `proxy.idle_probe.enabled` | `true` | Master kill-switch for ALL probing (on-demand + sweep). |
| `proxy.idle_probe.per_account_cooldown_secs` | `900` | Min gap between two probes of the same account. |
| `proxy.idle_probe.sweep_secs` | `900` | Background sweep cadence; `0` disables the sweep (on-demand only). |
| `proxy.idle_probe.stale_after_secs` | `900` | Window observations older than this make the account probe-eligible again; `0` reverts to windowless-only probing. |

Steady-state cost: at most four 1-token probes per cold account per hour.
Grok accounts are never probed (no quota surface). Operator-paused accounts
are never probed. Configs still carrying an untouched pre-2026-07-15 default
block (`3600/3600` or the old disabled triple) are migrated to these
defaults on load; any other explicit combination is kept verbatim.

## Model routing

With `routing.enabled = true`, the inbound `model` string selects a backend group:

- `claude-*`, `opus`, `sonnet`, `haiku`, `fable-5` route to the Claude group.
- `gpt-*`, `gpt-5.5`, `codex`, `o1`/`o3`/`o4` route to the Codex group.
- `grok`, `grok-*` route to the Grok group.
- `or-*`, a bare `or`, and `openrouter/*` route to the OpenRouter group.

Each group keeps its own sticky current account. If the model does not match a known group, llmux uses `routing.default_group`.

```json
"routing": {
  "enabled": true,
  "claude_models": [],
  "codex_models": [],
  "grok_models": [],
  "openrouter_models": [],
  "default_group": "claude",
  "on_empty_group": "error"
}
```

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | On = model-to-group routing; off = older Codex-as-overflow behavior. |
| `claude_models` | `[]` | Override tokens for Claude-group models. Empty keeps builtin rules. |
| `codex_models` | `[]` | Override tokens for Codex-group models. Empty keeps builtin rules. |
| `grok_models` | `[]` | Override tokens for Grok-group models. Empty keeps builtin rules. |
| `openrouter_models` | `[]` | Override tokens for OpenRouter-group models. Empty keeps the builtin `or-` prefix, exact `or`, and `openrouter/` prefix rules. |
| `default_group` | `"claude"` | Group for unmatched or absent model names: `"claude"`, `"codex"`, `"grok"`, or `"openrouter"`. |
| `on_empty_group` | `"error"` | `"error"` returns a 404 if the matched group has no account; `"fallback"` tries the remaining groups in `claude → codex → grok → openrouter` order. |

Override tokens are matched in order, first-match-wins, case-insensitively:

- `gpt-` means prefix match.
- `~codex` means substring match.
- `=gpt-5.5` means exact match.

## Codex request shaping

Codex settings are configurable in the config file and adjustable live from the dashboard.

| Key | Meaning |
|---|---|
| `codex.default_model` | Upstream Codex model slug; default `gpt-5.6-sol`. |
| `codex.fast` | Sends `service_tier: "priority"` when true. |
| `codex.reasoning_effort` | Optional: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max` (`ultra` on `gpt-6.1-sol`, `gpt-6-astra`, `gpt-6-sol` and `gpt-5.6-sol`/`-terra`). `max`/`ultra` clamp to `xhigh` on models below the gpt-5.6 family. |

For Claude Code model-selection details, including `gpt-5.5[1m]` and the long-context compaction workaround, see [operational-reference.md](operational-reference.md#selecting-the-codex-model-from-claude-code) and [faq.md](faq.md#gpt-55-stops-around-265k-context-what-should-i-do).

## Grok request shaping

Grok settings are configurable in the config file and adjustable live from the dashboard — click the Grok group's `effort:` value on the settings bar, or edit the rows in the config tab (`c`). There is no `fast` knob — xAI has no service tier.

| Key | Default | Meaning |
|---|---|---|
| `grok.default_model` | `grok-4.7` | Upstream slug used when the client's model is not grok-shaped. Any `grok-*` slug is accepted, curated or not. |
| `grok.reasoning_effort` | unset | Optional: `none`, `low`, `medium`, `high`, or `xhigh`; unset = bypass (the client's own effort rides through). The value is clamped against the effective model's level set at request time, so `xhigh` reaches the wire on `grok-4.6` and lands as `high` on `grok-4.5`. |

## OpenRouter backend

OpenRouter serves **Anthropic Messages** natively. For incoming Messages, llmux normalizes the model and removes unsigned foreign thinking before forwarding; there is no `fast` / `reasoning_effort` configuration knob here. Incoming Responses first converts to Messages, so it is not native Responses passthrough. See [provider compatibility](provider-compatibility.md#frontend-transport-matrix).

| Key | Default | Meaning |
|---|---|---|
| `openrouter.upstream` | `https://openrouter.ai/api` | Base URL the client's verbatim path is appended to, so the request goes to `{upstream}/v1/messages`. Host root, **not** `…/api/v1` — that would compose `…/api/v1/v1/messages`, which 404s. |
| `openrouter.default_model` | `stealth/ox-alpha` | The slug a bare `or` — or a request that names no model — resolves to. |

Model selection is the `or-` prefix: `or-ox-alpha` and the other curated ids resolve to their OpenRouter slug, `or-<vendor>/<slug>` reaches any of the ~400 uncurated models verbatim, and an unknown bare name is passed through so OpenRouter's own 404 answers it. See [models.md](models.md#alias-semantics).

## Claude reset grants

`claude_cli_version` (optional, default = the version built into the binary) is the Claude Code version llmux identifies as — `User-Agent: claude-cli/<version> (external, cli)` — when it reads or redeems Claude usage-limit reset grants (`GET /api/oauth/usage?cedar_ember=1`, `POST …/reset_rate_limits`). Anthropic gates those on the client surface and version; when Claude accounts start showing `n/e` with reason `cli_version` in the `rst` column, set this to the currently released Claude Code version. Read at daemon startup; see [operational-reference.md](operational-reference.md#usage-controls-refresh--resets).

## Raw request/response capture

| Key | Default | Meaning |
| --- | --- | --- |
| `raw_io.enabled` | `true` | Capture bounded request/response payloads to `raw-io.jsonl`; disable to stop capture. |
| `raw_io.retention_days` | `90` | Prune older records at startup; `0` keeps all history. |
| `raw_io.max_body_bytes` | `8388608` | Per-body capture limit for requests/responses, streaming or JSON. |

Credential headers are redacted; prompt/response bodies may still contain private
content. The SDK upstream legs are labeled `claude-agent-sdk` and contain bridge
Messages input/output, not the SDK’s private vendor HTTP. Incoming and returned
Responses bodies are recorded separately. See [raw viewer](operational-reference.md#raw-requestresponse-viewer).

## Email anonymous mode

`email_anonymous` masks account emails on every display surface while preserving live usage state. The TUI render layer uses stable fake-email mapping, and llmux Islands pixelizes emails in its Usage panel.

The setting is included in `GET /llmux/status` and can be changed live through `POST /llmux/settings {"email_anonymous": true}` or the Islands ☰ menu.

This differs from demo mode: demo mode uses stable fake identities and suppresses config writes for recording; email anonymous mode preserves the real daemon state and only masks rendered identities.

## TUI cosmetic effects

`tui_effects` (default `true`) gates the dashboard's cosmetic animations: the `max` effort token's rainbow marquee and the headline-model name gradient (`fable-5*`, `gpt-5.6-sol*`). Set it to `false` for a calmer board — those tokens keep a distinct static color and bold instead of cycling. Working spinners animate regardless of this setting. Like `email_anonymous`, the flag is carried on the dashboard document so both the local TUI and `llmux attach` honor it.

`tui_gradient` tunes those gradients (all fields optional; shown with defaults):

```json
"tui_gradient": {
  "speed": 1.0,
  "claude": "#ff79c6",
  "codex": "#56dcdc",
  "max_effort": null
}
```

- `speed` multiplies how fast both gradients drift (`2.0` = twice as fast, `0.5` = half; non-positive or non-finite values fall back to `1.0`).
- `claude` / `codex` are the `#rrggbb` base colors the headline-model gradient breathes around, per backend group (unparseable values fall back to the defaults).
- `max_effort`, when set to a `#rrggbb` color, replaces the `max` effort token's rainbow with a solid gradient on that color; `null`/absent keeps the rainbow.

Like `tui_effects`, the resolved settings ride the dashboard document, so `llmux attach` renders them identically. Read at daemon startup.

## Account types

| Type | Added by | Meaning |
|---|---|---|
| `oauth` | `llmux login` | Claude subscription account. |
| `apikey` | `llmux login --api` | Anthropic API-key account. |
| `codex` | `llmux login --codex` or `llmux import --from ~/.codex/auth.json` | ChatGPT/Codex subscription token. |
| `grok` | `llmux login --grok` | xAI Grok subscription token. |
| `openrouter` | `llmux login --openrouter` | OpenRouter API key (`sk-or-v1-…`), stored with the key label it was minted under. Named `or:<label>` (or `or:key-N` when the label is unavailable). No refresh: the key does not expire. |

Every browser-login type in that table — `oauth`, `codex`, `grok`, `openrouter` — can also be added without leaving the dashboard: `n` opens the provider picker, the flow runs in the client, and the credential is injected into the running daemon (see [operational-reference.md](operational-reference.md#commands)). `apikey` accounts still come from `a` (paste) or `llmux login --api`.

Claude accounts dedupe by `account_uuid`; Codex accounts dedupe by `account_id`; API keys and OpenRouter accounts dedupe by name (an OpenRouter label is not unique per key, so it is used for the name only).

### Downgrading past a new account type

The account list is an internally-tagged enum, so a config carrying a `type` an older binary does not know makes that binary **fail to parse the whole file** — nothing is silently dropped. Before downgrading to a pre-openrouter binary, remove the `or:*` accounts (`llmux remove <name>`, run from the new binary); the same contract applies to `grok:*` accounts and pre-grok binaries. Everything else is additive in both directions: the `openrouter` block and `routing.openrouter_models` are ignored harmlessly by older binaries, and a config written by an older binary loads here with the new keys at their defaults.

## Claude Code internal models (`claude_code`)

With routing enabled and a known GPT main session, implicit Claude Code requests
use `gpt_model_mapping`: Opus → `sol`, Sonnet → `terra`, Haiku → `luna` by default.
This includes session titles, quota probes and recognized helper/subagent turns.
The safety classifier uses the independent `auto_classifier_model` (default `luna`,
medium effort), regardless of its incoming Sonnet name.

Edit these values in the config file and restart the daemon. Partial objects keep
unspecified defaults. Targets must be known Codex catalog models or aliases; null,
unknown targets and Claude targets fail configuration validation. Defaults are
aliases, resolved on every request through the central model catalog: a catalog
alias update advances internal requests without rewriting this configuration.
Explicit concrete model IDs are available when pinning is intentional.

A genuine main `/model` switch to Claude remains Claude and resets session context;
these mappings do not change the model picker or native alias environment exports.
Pinned provider routes and disabled model routing bypass the adapter. Unknown
sessions retain normal routing, except exact initial quota/title requests can use the bounded
launch context provided by `llmux run`. See [launch context and quota behavior](operational-reference.md#claude-code-internal-model-routing).
