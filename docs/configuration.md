# Configuration

> Most settings are editable live from the TUI's `config` tab (see [operational-reference](operational-reference.md)); rows marked `restart` there persist here and apply on the next daemon start.

llmux stores local configuration at `~/.config/llmux.json` by default. It respects `$XDG_CONFIG_HOME`, and `$LLMUX_CONFIG` can point at a different file.

The config file is written with mode `0600`. Updates use atomic read-merge-write so the daemon and CLI can safely change different parts of the config while llmux is running.

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
| `proxy.port` | `3456` | Local daemon port. Claude Code reaches llmux through `ANTHROPIC_BASE_URL=http://localhost:3456`. |
| `proxy.api_key` | generated | The shared ADMIN credential (`lm-…`): non-loopback clients must present it (or an issued client key) as `x-api-key`, and `/llmux/*` control endpoints require it (or an admin-kind client key) even from localhost. Keyless data-plane requests are loopback-only. |
| `client_keys` | `[]` | Issued downstream client keys (multi-tenant). Managed via `llmux key …` / `POST /llmux/keys/*` — each entry stores id, name, email, kind (`default`\|`admin`), key prefix, SHA-256 digest, suspended flag, and timestamps. The secret itself is never stored; edit this section by hand only for disaster recovery. |
| `upstream` | `https://api.anthropic.com` | Anthropic-compatible upstream base URL for Claude accounts. |

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
| `codex.reasoning_effort` | Optional: `none`, `minimal`, `low`, `medium`, `high`, or `xhigh`. |

For Claude Code model-selection details, including `gpt-5.5[1m]` and the long-context compaction workaround, see [operational-reference.md](operational-reference.md#selecting-the-codex-model-from-claude-code) and [faq.md](faq.md#gpt-55-stops-around-265k-context-what-should-i-do).

## OpenRouter backend

OpenRouter serves the **Anthropic Messages** format natively, so llmux forwards the request body unchanged and only rewrites its `model` field — there is no request shaping to configure, and therefore no `fast` / `reasoning_effort` knob here (effort rides through as client metadata, as it does on the Claude passthrough).

| Key | Default | Meaning |
|---|---|---|
| `openrouter.upstream` | `https://openrouter.ai/api` | Base URL the client's verbatim path is appended to, so the request goes to `{upstream}/v1/messages`. Host root, **not** `…/api/v1` — that would compose `…/api/v1/v1/messages`, which 404s. |
| `openrouter.default_model` | `stealth/ox-alpha` | The slug a bare `or` — or a request that names no model — resolves to. |

Model selection is the `or-` prefix: `or-ox-alpha` and the other curated ids resolve to their OpenRouter slug, `or-<vendor>/<slug>` reaches any of the ~400 uncurated models verbatim, and an unknown bare name is passed through so OpenRouter's own 404 answers it. See [models.md](models.md#alias-semantics).

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

Claude accounts dedupe by `account_uuid`; Codex accounts dedupe by `account_id`; API keys and OpenRouter accounts dedupe by name (an OpenRouter label is not unique per key, so it is used for the name only).

### Downgrading past a new account type

The account list is an internally-tagged enum, so a config carrying a `type` an older binary does not know makes that binary **fail to parse the whole file** — nothing is silently dropped. Before downgrading to a pre-openrouter binary, remove the `or:*` accounts (`llmux remove <name>`, run from the new binary); the same contract applies to `grok:*` accounts and pre-grok binaries. Everything else is additive in both directions: the `openrouter` block and `routing.openrouter_models` are ignored harmlessly by older binaries, and a config written by an older binary loads here with the new keys at their defaults.
