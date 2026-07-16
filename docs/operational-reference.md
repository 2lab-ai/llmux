# llmux operational reference

This is the detailed, operational half of the docs: every command, the daemon/dashboard model, the configuration reference, the scheduling policy, model routing details, and the Codex backend. The conceptual overview — what llmux is and why — lives in [the root README](../README.md).

## Commands

| Command | Description |
|---|---|
| `server [--port N] [--no-tui] [--log-to DIR]` | Start the proxy. `--log-to` writes one file per request with credentials masked. If a llmux daemon already owns the port, attach to it instead. |
| `run [--force] [-- args]` | Ensure the daemon is running, then spawn `claude` pointed at the proxy. `--force` restarts the daemon even if a same-version one is already up. |
| `stop` | Stop a running server gracefully via `POST /llmux/shutdown`. |
| `restart` | Cooperatively drain a running daemon, then respawn it from this binary after an upgrade. |
| `login [--api \| --codex]` | Add a Claude account via browser OAuth; `--api` pastes an Anthropic API key; `--codex` runs the ChatGPT OAuth flow, falling back to importing `~/.codex/auth.json`, to add a Codex account. |
| `import [--from PATH \| --json JSON]` | Import credentials from a teamclaude config, `~/.claude/.credentials.json`, a Codex `~/.codex/auth.json`, or inline JSON. |
| `dashboard` | Attach to a running daemon and render its dashboard over HTTP. Read-only except manual account switch. |
| `env` | Print shell exports for pointing Claude Code at the proxy. |
| `status [--json]` | Show client/server/update sections plus per-account quota; exits 1 when no server is running. |
| `accounts [-v]` | List configured accounts; `-v` adds quota/cooldown detail. |
| `remove <name> [--yes]` | Remove an account by name. |
| `api <path>` | Debug: GET an upstream path with the current account's credentials. |

In the TUI: `s` switches account, `a` adds, `r` removes, `R` reloads config, `d` toggles detail, `l` cycles the log panel, `p` opens the perf tab, `q` quits, and `j`/`k` or arrows navigate. For Codex accounts, `f` toggles fast (priority) mode, `m` cycles the model, and `e` cycles reasoning effort. In attach mode (`llmux dashboard`, or `server` attaching to a daemon), config-mutation keys `a`/`r`/`R` are disabled because they would act on the server host's config; `s` still works through `POST /llmux/switch`. Activity-panel mouse semantics are described under [Activity feed](#activity-feed).

### Usage tab (calendar usage + cost)

The `usage` tab (`U`, or click the tab bar) shows calendar-bucketed usage over the persisted request history: hourly, daily, or monthly buckets (`g` cycles), each bucket broken down per model with request count, the four token classes (input / output / cache read / cache write), and the API-equivalent USD cost per model and per bucket. `j`/`k` (or arrows, the mouse wheel, `PgUp`/`PgDn`; `Home`/`End` jump) scroll by bucket; the title carries the period totals. Retention: hourly buckets cover the trailing 72 h, daily buckets 180 days, monthly buckets are unbounded (all replayed history from `activity.jsonl`). Day/month boundaries follow the daemon's local calendar; costs are API-equivalent estimates priced with the daemon's `pricing` overrides — not a bill. Amounts render ledger-style — decimal points aligned to one column (up to $999,999 per bucket), thousands separators, integer digits emphasized over the dimmer fraction digits, per-model rows a tier darker than bucket totals. A model with no known rate shows `—` instead of a cost, its bucket total is marked `+?`, and the title gains `(+unpriced)` — a missing rate is never rendered as a free `$0`. The same rows are served to attach clients on `GET /llmux/dashboard` (`usage_stats`), so local and remote render identically.

### Perf tab (observed performance)

The `perf` tab (`p`, or click the tab bar) is the observed-performance surface: passive telemetry from real proxied requests — deliberately not an active healthcheck. Three panes over a selectable trailing span (`d` cycles 7/14/30/90 days):

- **Daily tokens/sec chart** — one braille line per top `(provider, model, fast)` series, y = end-to-end output tokens/sec (`Σoutput / Σduration` per day).
- **Provider health matrix** — one row per day, one column per provider: request count, error %, average TTFB (time to first upstream body byte), and e2e t/s. A day with no traffic renders `—`, never a fabricated `0`.
- **Series table** — per `(provider, model, fast)` over the span (`j`/`k` scroll): requests, error %, output tokens, `e2e t/s`, `est t/s`, average TTFB, and measured coverage (`measured/throughput` samples).

Metric semantics (v1, deliberately conservative):

- `e2e t/s` = output tokens / total request duration — always available, includes queueing and time-to-first-token.
- TTFB and the first-delta offset are measured from the **served attempt's upstream dispatch** (the moment the winning upstream request was sent) — request-body buffering, scheduling, token refreshes, and 429 parks never contaminate them.
- `est t/s` = output tokens / the **stream-side span from the first streamed output delta to the end of the upstream stream** (the first `content_block_delta`, thinking deltas included; empty deltas never count). Labeled *estimated* because hidden reasoning (e.g. Codex reasoning that streams only as summaries) may precede the first delta — this is **not** a model decode-speed claim. Derived only from requests that recorded a first delta; legacy/non-streaming requests never mix into it.
- Codex **fast mode** splits every series: `⚡` = fast on, no marker = off, `?` = history recorded before the field existed (unknown — kept separate, never counted as off).
- Confidence is judged per statistic: `e2e` dims under its own throughput-sample count, `est` under its measured-sample count (fewer than 5 → dim; still shown — low traffic is itself a signal); aggregates whose summed span can't support a stable ratio show `—`. Quiet days are chart GAPS and `—` health rows, never a fabricated `0`. A mid-stream upstream abort counts as an error even though the client already held a 200.
- Data is rebuilt from the persisted request log on restart and retained 90 days; the tab title carries `timing since <date>` — the first day that actually observed v1 timing, not the oldest replayed legacy row.

The activity feed gains the same per-request view: a `t/s` column (e2e) on every completed row, and the click-expanded detail shows `e2e` / `est … post-delta` / `ttfb` / `first output` when recorded. Attach clients receive the same rows on `GET /llmux/dashboard` (`daily_perf`).

In the `sessions` tab, `o` cycles the sort (recent → tokens → requests), the mouse wheel moves the cursor, and a left-click selects the row under the pointer. The `t/s` column is the honest per-session output rate — Σ output tokens over Σ recorded request durations (raw-io records now carry `duration_ms`; pre-field history shows `—`, never a wall-clock-span fake).

## Install and launch details

### Homebrew channels

```bash
brew install 2lab-ai/tap/llmux
brew install 2lab-ai/tap/llmux-islands
brew install 2lab-ai/tap/llmux-preview
```

Use `llmux-preview` for the rolling preview channel. Use the stable `llmux` formula for normal daily work.

### Source build

```bash
git clone https://github.com/2lab-ai/llmux && cd llmux
just build
```

`just build` runs `cargo build --release --locked`.

### Running Claude Code through llmux

`llmux run` spawns `claude` with only `ANTHROPIC_BASE_URL` set and passes arguments through after `--`. If nothing is listening on the configured port, `run` auto-starts a detached daemon and waits until it is ready.

Daemon stderr is written to `~/.local/state/llmux/server.log`, respecting `$XDG_STATE_HOME`. A port occupied by a foreign process is an error; llmux never overwrites it.

Manual shell wiring:

```bash
eval "$(llmux env)"
claude
```

## Daemon and dashboard

Only one process can own port 3456 — normally the background daemon created by `llmux run`. To inspect it:

- `llmux dashboard` polls `GET /llmux/dashboard` once a second and renders the same view model the in-process TUI uses. Dropped connections show a reconnecting banner and keep retrying.
- `llmux server`, when a llmux daemon already owns the port, prints `daemon already running (pid N) — attaching…` and enters attach mode instead of failing with `Address already in use`.
- A foreign process on the port remains a clean error and is never overwritten.

Both attach paths are read-only except manual switching through the gated loopback control endpoint.

## Demo, recording, and privacy modes

Recording a demo or sharing your screen? Set `LLMUX_DEMO_MODE=1` and the dashboard, status, and logs show **stable fake emails** in place of your real account names. Config writes are suppressed so the aliases never touch disk.

The Islands app has the same masking via `--demo` or `LLMUX_ISLANDS_DEMO=1`; it also opens and holds the notch panel open for recording.

For persistent masking, `email_anonymous` in the config masks every email surface — TUI render and Islands mosaic — and can be flipped live from the Islands ☰ menu or `POST /llmux/settings`.

Two demo GIFs are regenerated at deploy time and attached to each release:

- **CLI / TUI** — [`demo/llmux.tape`](../demo/llmux.tape) (vhs) → [`llmux-demo.gif`](https://github.com/2lab-ai/llmux/releases/latest/download/llmux-demo.gif)
- **Islands app** — `--demo` capture → [`llmux-islands-demo.gif`](https://github.com/2lab-ai/llmux/releases/latest/download/llmux-islands-demo.gif)

Recorders live in [`demo/`](../demo/): `record-cli.sh`, `record-islands.sh`, and `record-all.sh` (records both + `gh release upload`). The app capture needs a one-time macOS **Screen Recording** grant for the terminal you run it from.

## Configuration

Config lives at `~/.config/llmux.json` by default, respects `$XDG_CONFIG_HOME`, and can be overridden with `$LLMUX_CONFIG`. File mode is `0600`. Writes are atomic read-merge-write so the server and CLI can update concurrently.

See [configuration.md](configuration.md) for the full config reference.

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

Scheduler knobs:

| Key | Default | Meaning |
|---|---:|---|
| `five_hour_max` | `0.90` | Max 5-hour utilization before an account is ineligible. |
| `seven_day_max` | `0.99` | Max 7-day utilization before an account is ineligible. |
| `usage_poll_secs` | `300` | Per-account OAuth usage poll interval. |
| `usage_max_age_secs` | `600` | Usage older than this is stale; stale accounts are skipped unless all are stale. |
| `refresh_ahead_secs` | `25200` | Background refresh threshold; default 7 hours before token expiry. |

Codex request-shaping is also settable live from the dashboard's Codex group: `default_model` (the model slug sent upstream, default `gpt-5.5`), `fast` (sends `service_tier: "priority"` when `true`), and `reasoning_effort` (`none`|`minimal`|`low`|`medium`|`high`|`xhigh`; omitted by default).

Accounts are `oauth` (Claude subscription), `apikey` (Anthropic API key), or `codex` (ChatGPT/Codex subscription token). Claude accounts dedupe by `account_uuid`; Codex accounts dedupe by `account_id`; API keys dedupe by name. An `lm-...` proxy API key is generated on first run; localhost clients are exempt.

`email_anonymous` (default `false`) masks account emails on every display surface. The TUI render layer uses the same stable fake-email mapping as demo mode, and llmux Islands pixelizes emails in its Usage panel. The value is served in `GET /llmux/status` and can be flipped live via `POST /llmux/settings {"email_anonymous": true}` or the Islands ☰ toggle.

## Activity feed

The dashboard's activity panel shows one row per completed request
(2026-07-15 layout):

```text
▸ HH:MM:SS  kind  [model effort]  email…(10) → 200 3.1s 269tok $0.0079 «session» "input text to the screen edge"
```

- **kind** — what the request was, classified once at forward entry from the
  buffered body: `user`, `count` (count_tokens), `security`, `compact`,
  `summary`, `title`, `suggest`, `audit`, `subagent`, `sdk`, plus two
  harness control pings — `quota` (Claude Code's per-session rate-limit
  probe: a bare `"quota"` turn with `max_tokens: 1`) and `recap` ("The user
  stepped away…"). See
  [system-prompts/families.md](system-prompts/families.md) for the wire
  fingerprints.
- **badge** — `[model effort]`, group conveyed by color; the vendor prefix
  is stripped (`claude-opus-4-8[1m]` → `opus-4-8[1m]`). Columns are padded
  to the widest visible value per frame, and the input excerpt takes the
  remaining terminal width.
- **Clicking a row** expands its detail lines (full method+path, client id,
  account, token/cost breakdown). Clicking again collapses.
- **Grouping** — only consecutive `count` probes fold, into
  `▸ HH:MM:SS count N× …` (start time). The leading `▸` marker toggles the
  fold; clicking the header body only ever expands; clicking a member row
  inside an open fold expands that member's own detail instead of closing
  the group.
- **Infinite scroll** — wheel/arrow scrolling past the in-memory window
  hydrates the persisted `activity.jsonl` in the background and pages older
  rows in on demand. Works for the in-process TUI and loopback attach; a
  cross-host attach deliberately refuses local-file history (the file
  belongs to a different daemon) until a remote paging endpoint exists.

Two request-handling behaviors keep the feed honest at session start:
`HEAD|GET /` (Claude Code's base-URL reachability check) is answered locally
with 200 instead of being forwarded upstream to a guaranteed 404, and a
`quota` probe gets exactly one upstream attempt — no failover sweep across
the pool, no exhaustion park — so a rate-limited moment paints one line, not
one per account.

### Raw request/response viewer

An expanded entry's `🔍 request` detail line opens a DevTools-style modal
over the dashboard with the request's captured payloads (from
`raw-io.jsonl`; see `raw_io` in [configuration.md](configuration.md)).

- **Tabs follow the wire.** A translated (codex/grok) exchange shows all
  four legs — `Request` (client → llmux) → `Upstream Req` (the rewritten
  Responses-API request llmux sent) → `Upstream Resp` (the provider's
  verbatim reply before conversion) → `Response` (what the client
  received). A byte-identity Anthropic passthrough shows the classic 2 tabs
  (client and upstream exchange are the same bytes). Click a tab or walk
  with `←`/`→`/`Tab`/`h`/`l`.
- **Scrolling.** `↑`/`↓`/`j`/`k`, `PgUp`/`PgDn`, `Home`/`End`, and the
  wheel scroll vertically; `H`/`L` (or a horizontal wheel) pan sideways.
  Proportional scrollbars render on the right and bottom edges when the
  content overflows.
- **Action buttons** (top-right, click or key): `copy` (`c`) puts the
  active tab's raw body on the system clipboard; `copy as curl` (`C`)
  reconstructs a replayable curl for that tab's side of the exchange
  (redacted credential values stay `•••redacted` — substitute your own);
  `copy all` (`a`) copies every leg with section markers; `save` (`s`) /
  `save all` (`S`) write the body / the whole record JSON to `~/Downloads`
  (timestamped). Clipboard uses `pbcopy`/`wl-copy`/`xclip`/`xsel` when
  available, falling back to the OSC 52 terminal escape; the outcome
  flashes on the modal's hint line.
- `Esc`/`q`/`Enter` closes. Records written by a daemon predating a field
  render it as "(not captured)"; passthrough records carry no upstream
  half by design.

## Scheduling model

Each account tracks two quota windows: 5-hour session and 7-day weekly. Anthropic accounts get passive data from upstream response headers plus active OAuth usage polling; Codex accounts ingest `x-codex-*` headers when present. Selection is pure and deterministic over a snapshot, re-evaluated when the current account becomes ineligible and on a 60-second tick — never per request.

1. **Eligibility gate.** Keep accounts with healthy auth, no active 429 park, both windows under their thresholds, and fresh-enough usage data. A degraded headers-only mode kicks in if every account's usage is stale.
2. **Score = usable burst now × perishability.** `servable_now = min(5h headroom, 7d headroom)` is what an account can serve before it next gates on either limit. `urgency` ramps up the closer an account is to its 7-day reset, linearly across the week and capped at 4×. So `score = servable_now × urgency` prefers quota that is **about to reset and would otherwise evaporate unused**, but only while it is still usable.
3. **Sticky, with a perishability override.** Stay on the current account while it is eligible — unless another account is clearly worth more, currently 25% above the current score. This protects upstream prompt-cache locality while still burning soon-to-reset quota.
4. **Backend grouping.** With routing on by default, only same-group accounts compete and each group keeps its own sticky pick. With routing off, Codex accounts rank last as a cross-group overflow pool.
5. **Exhaustion.** Honor `retry-after` on 429. If every account is exhausted, return 429 with the soonest window reset as `retry-after`.

The full derivation, edge cases, and the wasted-quota simulation that validates this policy live in [`.prd/09-scheduler-perishability.md`](../.prd/09-scheduler-perishability.md).

## Model routing

By default (`routing.enabled = true`) the request's `model` selects the backend **group**:

- **claude group** — `oauth` + `apikey` accounts; models `claude-*`, `opus`, `sonnet`, `haiku`, `fable-5`.
- **codex group** — `codex` accounts; models `gpt-*`, `gpt-5.5`, `codex`, `o1`/`o3`/`o4`.

Within the matched group the scheduler picks the best eligible account, sticky **per group**. The Claude pick and the Codex pick advance independently. An unrecognized or absent model falls back to `default_group`.

Turn routing **off** (`routing.enabled = false`) for the older behavior: the `model` is ignored for selection and Codex accounts become a cross-group overflow pool. A request lands on the best Claude/API account and only spills to Codex when every Claude account is exhausted.

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
| `enabled` | `true` | On = model→group routing; off = Codex-as-overflow behavior. |
| `claude_models` | `[]` | Models routed to the Claude group. Empty keeps the builtin rules; a non-empty list replaces them. |
| `codex_models` | `[]` | Models routed to the Codex group with the same semantics. |
| `default_group` | `"claude"` | Group for an unmatched or model-less request. |
| `on_empty_group` | `"error"` | When the matched group has no configured account: `"error"` returns a 404 `not_found_error`; `"fallback"` falls back to the other group. |

Override tokens in `claude_models` / `codex_models` are matched in order, first-match-wins, case-insensitively. A bare token is a **prefix** (`"gpt-"`); prefix it with `~` for a **substring** (`"~codex"`) or `=` for an **exact** match (`"=gpt-5.5"`).

### Selecting the Codex model from Claude Code

The inbound `model` string **is** the selector — point Claude Code's model at a Codex-group model and the proxy routes the request to a Codex account:

```bash
# Per-session: route this Claude Code session's requests to the Codex group
ANTHROPIC_MODEL=gpt-5.5 claude
```

or set the model in Claude Code's own model setting:

```text
/model gpt-5.5
/model gpt-5.5[1m]
```

For account *selection*, the model string's only job is to choose the group. Any `gpt-*` / `codex` / `o1`–`o4` string that classifies to the Codex group is routed there. The actual upstream model sent to Codex is `codex.default_model` (default `gpt-5.5`), set in config or live from the dashboard. `/llmux/status` reports the per-group current accounts under `current_by_group` and keeps a representative scalar `current` for back-compat.

### Context-window display for `gpt-5.5`

When you route to the Codex group with a bare `gpt-5.5`, Claude Code's **remaining context** indicator is wrong because the client computes the window from the **model-name string**. llmux can route and stream the request, but it cannot set Claude Code's local context-window table:

- Claude Code derives the context **window** from the model name, client-side.
- Bare `gpt-5.5` can fall back to a **200,000** token window in Claude Code.
- The Codex `gpt-5.5` backend window is larger, and `gpt-5.5[1m]` makes Claude Code display a **1,000,000** token window.
- No `/v1/messages` response field or endpoint lets the proxy set the client's context window.

Use the `[1m]` suffix when you want Claude Code to display a 1M window:

```text
/model gpt-5.5[1m]
```

`gpt-5.5[1m]` still routes to the Codex group: the `gpt-` prefix still matches, and the suffix is stripped for routing/usage attribution. The tradeoff is that the 1M display can **over-report** the real usable Codex window; it is a client-side display workaround, not a promise that every long transcript will be accepted unchanged.

If a long session still blocks near the mid-200k range, use the empirical compaction workaround in [FAQ: gpt-5.5 context stops around 265k](faq.md#gpt-55-stops-around-265k-context-what-should-i-do).

## Codex backend

A ChatGPT/Codex subscription credential can be added with `llmux login --codex` (browser OAuth, falling back to importing `~/.codex/auth.json`) or imported directly:

```bash
llmux import --from ~/.codex/auth.json
```

The Codex provider translates Claude Code Messages requests into the Codex Responses backend and converts the stream back into Anthropic Messages SSE. The upstream model, a fast (`priority`) service tier, and reasoning effort are configurable (`codex.default_model` / `codex.fast` / `codex.reasoning_effort`) and adjustable live from the dashboard (`m` / `f` / `e`). Text, thinking summaries, and tool calls are supported. Images are dropped with a warning for now. `/v1/messages/count_tokens` is answered locally; other non-`/v1/messages` endpoints return a clear 501.

## Development

```bash
just check    # cargo fmt --check + cargo clippy --all-targets -- -D warnings + cargo test
just build    # cargo build --release --locked
```

Contributor conventions are in [`../AGENTS.md`](../AGENTS.md).
