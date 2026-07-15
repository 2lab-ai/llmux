# Configuration reference

llmux stores durable configuration and provider credentials in
`~/.config/llmux.json` by default. `$XDG_CONFIG_HOME` changes the base config
directory; `$LLMUX_CONFIG` overrides the complete path.

The file is created with mode `0600`. Each update reloads the file immediately
before mutation and replaces it atomically, which prevents partial files. There
is no cross-process writer lock or compare-and-swap: overlapping writers can be
last-write-wins. The file contains secrets; never commit or paste it unredacted.

## Complete shape

This example shows the current schema and defaults, not a recommended set of
overrides. Credentials are redacted.

```jsonc
{
  "version": 1,
  "proxy": {
    "port": 3456,
    "api_key": "lm-...",
    "forward_idle_timeout_secs": 120,
    "max_request_bytes": 67108864,
    "idle_probe": {
      "enabled": true,
      "per_account_cooldown_secs": 900,
      "sweep_secs": 900,
      "stale_after_secs": 900
    }
  },
  "upstream": "https://api.anthropic.com",
  "codex": {
    "upstream": "https://chatgpt.com/backend-api/codex",
    "token_url": "https://auth.openai.com/oauth/token",
    "default_model": "gpt-5.6-sol",
    "client_model": null,
    "fast": false,
    "reasoning_effort": null,
    "trace": true
  },
  "grok": {
    "upstream": "https://cli-chat-proxy.grok.com/v1",
    "default_model": "grok-4.5",
    "client_model": null,
    "reasoning_effort": null,
    "trace": false
  },
  "scheduler": {
    "five_hour_max": 0.90,
    "seven_day_max": 0.99,
    "fable_weekly_max": 0.98,
    "usage_poll_secs": 300,
    "usage_max_age_secs": 600,
    "refresh_ahead_secs": 25200,
    "mode": "default"
  },
  "routing": {
    "enabled": true,
    "claude_models": [],
    "codex_models": [],
    "grok_models": [],
    "default_group": "claude",
    "on_empty_group": "error"
  },
  "pricing": {},
  "raw_io": {
    "enabled": true,
    "retention_days": 90,
    "max_body_bytes": 8388608
  },
  "email_anonymous": false,
  "tui_effects": true,
  "show_fable_weekly": true,
  "domain_abbrev": { "insightquest.io": "iq.io" },
  "quota_display": "remaining",
  "paused_accounts": [],
  "account_limits": {},
  "events": [],
  "remote": {},
  "accounts": []
}
```

Omitted keys load their defaults. `null` optional fields are normally omitted
when llmux writes the file.

## Proxy

| Key | Default | Meaning |
| --- | ---: | --- |
| `proxy.port` | `3456` | Listener port. The daemon binds all interfaces; loopback is auth-exempt and off-loopback is API-key-gated. |
| `proxy.api_key` | generated `lm-*` | Presented as `x-api-key` by off-loopback clients. This authenticates ownership; it does not encrypt HTTP. |
| `proxy.forward_idle_timeout_secs` | `120` | Maximum silence between upstream bytes after connection, not a total request deadline. |
| `proxy.max_request_bytes` | `67108864` | 64 MiB ingress body cap; oversized requests return 413 before forwarding. |
| `upstream` | `https://api.anthropic.com` | Anthropic base URL for Claude credentials. |

### Idle probe

Cold Codex and API-key accounts do not have the Claude OAuth usage poller. A
bounded `max_tokens = 1` probe lets their observed quota windows stay visible.
Grok accounts and operator-paused accounts are not probed.

| Key | Default | Meaning |
| --- | ---: | --- |
| `proxy.idle_probe.enabled` | `true` | Master switch for on-demand and sweep probes. |
| `per_account_cooldown_secs` | `900` | Minimum gap between probes for one account. |
| `sweep_secs` | `900` | Background sweep cadence; `0` disables the sweep but leaves on-demand probing. |
| `stale_after_secs` | `900` | Evidence age that makes an account probe-eligible again; `0` means windowless-only. |

At defaults, steady state is bounded to four one-token probes per cold eligible
account per hour.

## Scheduler

| Key | Default | Meaning |
| --- | ---: | --- |
| `scheduler.five_hour_max` | `0.90` | Global 5h utilization ceiling. |
| `scheduler.seven_day_max` | `0.99` | Global 7d utilization ceiling. |
| `scheduler.fable_weekly_max` | `0.98` | Additional Fable-family weekly ceiling; non-Fable traffic ignores it. |
| `scheduler.usage_poll_secs` | `300` | Claude OAuth usage polling interval. |
| `scheduler.usage_max_age_secs` | `600` | Age at which polled usage becomes stale. |
| `scheduler.refresh_ahead_secs` | `25200` | Refresh OAuth credentials within seven hours of expiry. |
| `scheduler.mode` | `"default"` | `default` perishability score or `round-robin` sequential exhaust. |

`paused_accounts` is an array of exact account names excluded from automatic
and manual selection until resumed. The Claude OAuth poller may still refresh
Claude observations; paused Codex/API-key accounts are excluded from idle
probes, so their observations can age while paused.

`account_limits` overrides one or more ceilings by account:

```json
{
  "account_limits": {
    "claude:user@example.com": {
      "five_hour_max": 0.85,
      "seven_day_max": 0.97,
      "fable_weekly_max": 0.95
    }
  }
}
```

See [Scheduling accounts](guides/scheduling.md) for selection behavior.

## Model routing

With `routing.enabled = true`, the incoming model chooses a backend group:

- Claude: `claude-*`, `fable`, `opus`, `sonnet`, `haiku`, and related builtins.
- Codex: `gpt-*`, `codex`, `o1`/`o3`/`o4`, `sol`, `terra`, and `luna`.
- Grok: `grok` and `grok-*`.

| Key | Default | Meaning |
| --- | --- | --- |
| `routing.enabled` | `true` | Enable per-model group filtering and independent sticky group slots. |
| `claude_models` | `[]` | Custom Claude tokens; empty preserves builtins, non-empty replaces them. |
| `codex_models` | `[]` | Custom Codex tokens with the same replacement semantics. |
| `grok_models` | `[]` | Custom Grok tokens with the same replacement semantics. |
| `default_group` | `"claude"` | Group for an absent or unmatched model. Accepts `claude`, `codex`, or `grok`. |
| `on_empty_group` | `"error"` | `error` returns 404 when no account exists in the group; `fallback` scans Claude → Codex → Grok for a configured group. |

Token syntax is case-insensitive:

- `gpt-` — prefix match;
- `~codex` — substring match;
- `=gpt-5.5` — exact match.

Classification checks Codex rules, then Claude rules, then Grok rules. Keep
custom sets disjoint. With routing disabled, no group filter is applied and
Codex remains legacy cross-group overflow.

Routing selects the group; the provider adapter decides the concrete upstream
model. Known Codex IDs pass through, `sol`/`terra`/`luna` aliases resolve, and
unknown/model-less values use `codex.default_model`. Grok forwards concrete
`grok-*` IDs and resolves bare `grok` to `grok.default_model`.

## Codex request shaping

| Key | Default | Meaning |
| --- | --- | --- |
| `codex.upstream` | ChatGPT Codex backend | Base URL; `/responses` is appended. |
| `codex.token_url` | OpenAI OAuth token URL | Refresh-token endpoint. |
| `codex.default_model` | `gpt-5.6-sol` | Pin for absent, decorated, or unknown requested models. |
| `codex.client_model` | absent | Optional model name reported back to Claude Code for client-side context accounting. |
| `codex.fast` | `false` | Send `service_tier: "priority"`. |
| `codex.reasoning_effort` | absent | Configured value overrides client effort; absent/bypass preserves the client's value or backend default. |
| `codex.trace` | `true` | Append Responses trace records to `codex-trace.jsonl`. |

Accepted configured efforts are `none`, `minimal`, `low`, `medium`, `high`,
`xhigh`, `max`, and `ultra`; the chosen model clamps unsupported levels.

`client_model` changes only the name reported to Claude Code. It does not
increase the upstream model's real context window. Pair any display alias with
an honest compaction threshold.

## Grok request shaping

| Key | Default | Meaning |
| --- | --- | --- |
| `grok.upstream` | `https://cli-chat-proxy.grok.com/v1` | Subscription chat proxy base; `/responses` is appended. |
| `grok.default_model` | `grok-4.5` | Pin used by bare `grok` or a non-Grok request that reached the group. |
| `grok.client_model` | absent | Optional name reported to Claude Code. |
| `grok.reasoning_effort` | absent | Configured effort overrides client effort; absent is bypass/backend default. |
| `grok.trace` | `false` | Append tagged Grok records to the shared `codex-trace.jsonl`. |

Configured Grok effort is the `none`/`low`/`medium`/`high` superset and is
clamped by the selected model's supported thinking levels. Grok has no
`fast`/priority tier.

## Remote client

Unset `remote.host` means local mode. A persistent remote client uses:

```json
{
  "remote": {
    "host": "llmux-host",
    "port": 3456,
    "api_key": "lm-..."
  }
}
```

| Key | Default | Meaning |
| --- | --- | --- |
| `remote.host` | absent | Remote daemon host; enables remote mode. |
| `remote.port` | absent → `3456` | Remote port. |
| `remote.api_key` | absent | The remote daemon's `proxy.api_key`, not a provider credential. |

The global `--remote host[:port]` flag overrides host/port for one command.
Transport remains HTTP, so follow the [remote daemon security guide](guides/remote-daemon.md).

## Raw I/O and traces

`raw_io` captures verbatim request and delivered-response bodies to
`$XDG_STATE_HOME/llmux/raw-io.jsonl`. It is enabled by default and sensitive.

| Key | Default | Meaning |
| --- | ---: | --- |
| `raw_io.enabled` | `true` | Master capture switch. |
| `raw_io.retention_days` | `90` | Startup pruning horizon; `0` keeps forever. |
| `raw_io.max_body_bytes` | `8388608` | 8 MiB cap applied separately to request and response captures. |

This cap limits what is retained, while `proxy.max_request_bytes` limits what
the daemon accepts. Activity metadata is a separate append-only history.

Before handling confidential prompts, decide whether payload capture should be
disabled or shortened.

## Pricing overrides

`pricing` maps normalized model slugs to USD per million tokens:

```json
{
  "pricing": {
    "custom-model": {
      "input": 1.0,
      "output": 5.0,
      "cache_read": 0.1,
      "cache_creation": 1.25
    }
  }
}
```

An override wins over the built-in table. Recognized Claude, Codex, and Grok
groups fall back to their group rate when a concrete model is absent from the
built-in table. Only a genuinely unknown group/model remains unpriced in the
usage UI.

## Display and privacy

| Key | Default | Meaning |
| --- | --- | --- |
| `email_anonymous` | `false` | Mask account emails on display surfaces without changing API account IDs. |
| `tui_effects` | `true` | Enable cosmetic effort/model animations; working spinners remain active. |
| `show_fable_weekly` | `true` | Show the Fable weekly gauge in the account table. |
| `domain_abbrev` | `{ "insightquest.io": "iq.io" }` | Display-only domain shortening; `{}` disables it. |
| `quota_display` | `"remaining"` | Gauge fill direction: `remaining` or `used`; `u` overrides for the TUI session. |

Demo mode is environment-driven rather than durable config. See
[Operational privacy](operational-reference.md#privacy-and-recording) and
[Islands privacy](llmux-islands.md#privacy).

## Events

`events` contains dashboard banners. Each object has stable `id`, inclusive
`from`, exclusive `to`, and `content`. Times accept RFC3339 with an offset or
local `YYYYMMDDHHMM`:

```json
{
  "events": [
    {
      "id": "maintenance-20260720",
      "from": "2026-07-20T09:00:00+09:00",
      "to": "2026-07-20T10:00:00+09:00",
      "content": "Provider maintenance window"
    }
  ]
}
```

When several are active, the banner with the earliest end time is shown.

## Account records

Prefer `llmux login` and `llmux import` over hand-editing secrets.

| `type` | Added by | Identity/dedup key |
| --- | --- | --- |
| `oauth` | `llmux login` | Claude `account_uuid`, then name |
| `apikey` | `llmux login --api` | name |
| `codex` | `llmux login --codex` or Codex import | `account_id`, then name |
| `grok` | `llmux login --grok` | OIDC subject, then name |

OAuth-style records contain access/refresh tokens, expiry, and optional last
refresh. Grok additionally persists its discovered token endpoint. A config
with Grok records cannot be parsed by a pre-Grok binary; remove those accounts
before such a downgrade.
