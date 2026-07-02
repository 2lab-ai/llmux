# Configuration

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
    "default_group": "claude",
    "on_empty_group": "error"
  },
  "codex": {
    "default_model": "gpt-5.5",
    "fast": false
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
| `proxy.api_key` | generated | Required for non-loopback clients. Localhost is exempt. |
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

## Model routing

With `routing.enabled = true`, the inbound `model` string selects a backend group:

- `claude-*`, `opus`, `sonnet`, `haiku`, `fable-5` route to the Claude group.
- `gpt-*`, `gpt-5.5`, `codex`, `o1`/`o3`/`o4` route to the Codex group.

Each group keeps its own sticky current account. If the model does not match a known group, llmux uses `routing.default_group`.

```json
"routing": {
  "enabled": true,
  "claude_models": [],
  "codex_models": [],
  "default_group": "claude",
  "on_empty_group": "error"
}
```

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | On = model-to-group routing; off = older Codex-as-overflow behavior. |
| `claude_models` | `[]` | Override tokens for Claude-group models. Empty keeps builtin rules. |
| `codex_models` | `[]` | Override tokens for Codex-group models. Empty keeps builtin rules. |
| `default_group` | `"claude"` | Group for unmatched or absent model names. |
| `on_empty_group` | `"error"` | `"error"` returns a 404 if the matched group has no account; `"fallback"` tries the other group. |

Override tokens are matched in order, first-match-wins, case-insensitively:

- `gpt-` means prefix match.
- `~codex` means substring match.
- `=gpt-5.5` means exact match.

## Codex request shaping

Codex settings are configurable in the config file and adjustable live from the dashboard.

| Key | Meaning |
|---|---|
| `codex.default_model` | Upstream Codex model slug; default `gpt-5.5`. |
| `codex.fast` | Sends `service_tier: "priority"` when true. |
| `codex.reasoning_effort` | Optional: `none`, `minimal`, `low`, `medium`, `high`, or `xhigh`. |

For Claude Code model-selection details, including `gpt-5.5[1m]` and the long-context compaction workaround, see [operational-reference.md](operational-reference.md#selecting-the-codex-model-from-claude-code) and [faq.md](faq.md#gpt-55-stops-around-265k-context-what-should-i-do).

## Email anonymous mode

`email_anonymous` masks account emails on every display surface while preserving live usage state. The TUI render layer uses stable fake-email mapping, and llmux Islands pixelizes emails in its Usage panel.

The setting is included in `GET /llmux/status` and can be changed live through `POST /llmux/settings {"email_anonymous": true}` or the Islands ☰ menu.

This differs from demo mode: demo mode uses stable fake identities and suppresses config writes for recording; email anonymous mode preserves the real daemon state and only masks rendered identities.

## Account types

| Type | Added by | Meaning |
|---|---|---|
| `oauth` | `llmux login` | Claude subscription account. |
| `apikey` | `llmux login --api` | Anthropic API-key account. |
| `codex` | `llmux login --codex` or `llmux import --from ~/.codex/auth.json` | ChatGPT/Codex subscription token. |

Claude accounts dedupe by `account_uuid`; Codex accounts dedupe by `account_id`; API keys dedupe by name.
