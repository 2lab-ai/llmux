# llmux operational reference

This is the detailed, operational half of the docs: every command, the daemon/dashboard model, the configuration reference, the scheduling policy, model routing details, and the Codex backend. The conceptual overview — what llmux is and why — lives in [the root README](../README.md).

## Commands

| Command | Description |
|---|---|
| `server [--port N] [--no-tui] [--log-to DIR]` | Start the proxy. `--log-to` writes one file per request with credentials masked. If a llmux daemon already owns the port, attach to it instead. |
| `run [--force] [-- args]` | Ensure the daemon is running, then spawn `claude` pointed at the proxy. `--force` restarts the daemon even if a same-version one is already up. |
| `stop` | Stop a running server gracefully via `POST /llmux/shutdown`. |
| `restart` | Cooperatively drain a running daemon, then respawn it from this binary after an upgrade. |
| `login [--api \| --codex \| --grok \| --openrouter [--paste]]` | Add a Claude account via browser OAuth; `--api` pastes an Anthropic API key; `--codex` runs the ChatGPT OAuth flow, falling back to importing `~/.codex/auth.json`, to add a Codex account; `--grok` runs the xAI device-code flow; `--openrouter` runs the OpenRouter OAuth PKCE flow in the browser and mints a long-lived `sk-or-v1-…` key. `--paste` (only with `--openrouter`) prompts for an existing key instead of opening a browser — and is also the automatic fallback when the browser flow cannot complete locally. |
| `import [--from PATH \| --json JSON]` | Import credentials from a teamclaude config, `~/.claude/.credentials.json`, a Codex `~/.codex/auth.json`, or inline JSON. |
| `dashboard` | Attach to a running daemon and render its dashboard over HTTP. Read-only except manual account switch. |
| `env` | Print shell exports for pointing Claude Code at the proxy. |
| `status [--json]` | Show client/server/update sections plus per-account quota; exits 1 when no server is running. |
| `accounts [-v]` | List configured accounts; `-v` adds quota/cooldown detail. |
| `remove <name> [--yes]` | Remove an account by name. |
| `key new --name L [--email E] [--admin]` | Issue a downstream client key (multi-tenant). The secret is printed once and never stored. |
| `key list` | List issued client keys (id, name, email, kind, state, prefix). |
| `key suspend\|resume\|remove\|rotate <id\|name>` | Suspend/resume/revoke/rotate a key. Takes effect on the very next request — no restart. `remove` is a soft-revoke: usage history keeps its name/email. `rotate` issues a new secret under the same attribution id. |
| `api <path>` | Debug: GET an upstream path with the current account's credentials. |

In the TUI: `s` switches account, `a` adds, `n` starts a new browser login, `r` removes, `R` reloads config, `d` toggles detail, `l` cycles the log panel, `p` opens the perf tab, `K` opens the keys tab (multi-tenant client keys + per-tenant usage; read-only — mutations go through `llmux key …`), `q` quits, and `j`/`k` or arrows navigate. For Codex accounts, `f` toggles fast (priority) mode, `m` cycles the model, and `e` cycles reasoning effort. The Grok group's `effort:` value on the same settings bar is click-cycled (or activated from the config tab's `grok.reasoning_effort` row): `bypass → none → low → medium → high → xhigh`, with the per-model clamp still applied at request time — `xhigh` rides through on `grok-4.6` and lands as `high` on `grok-4.5`. In attach mode (`llmux dashboard`, or `server` attaching to a daemon), config-mutation keys `a`/`r`/`R` are disabled because they would act on the server host's config; `s` still works through `POST /llmux/switch`. Activity-panel mouse semantics are described under [Activity feed](#activity-feed).

`n` (from the accounts overlay or the account switcher) opens a provider picker — Claude (Anthropic OAuth), Codex (ChatGPT OAuth), Grok (xAI device code — it prints the verification URL and user code, best-effort opens the browser, then polls), OpenRouter (PKCE minting an `sk-or-v1-…` key). `↑↓` picks, `Enter` runs the flow, `Esc` cancels. The OAuth flow runs in the client that owns the keyboard, and the minted credential is injected into the daemon — in-process locally, over `POST /llmux/inject-account` when attached — so the account serves traffic without a restart and `n` is live in attach mode too. On a client with no browser (a headless SSH session), `n` refuses with the `llmux login` fallback hint instead of starting a flow that would hang.

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
- `est t/s` = output tokens / the **stream-side span from the first streamed output delta to the end of the upstream stream** (fixed inside the relay pump at upstream EOF — file IO and post-processing never inflate it) (the first `content_block_delta`, thinking deltas included; empty deltas never count). Labeled *estimated* because hidden reasoning (e.g. Codex reasoning that streams only as summaries) may precede the first delta — this is **not** a model decode-speed claim. Derived only from requests whose UPSTREAM stream produced a first delta (a client asking for `stream=false` still counts — llmux streams from the provider either way); legacy history and upstream JSON/non-SSE responses never mix into it.
- Codex **fast mode** splits every series: `⚡` = fast on, no marker = off, `?` = history recorded before the field existed (unknown — kept separate, never counted as off).
- Confidence is judged per statistic: `e2e` dims under its own throughput-sample count, `est` under its measured-sample count (fewer than 5 → dim; still shown — low traffic is itself a signal); aggregates whose summed span can't support a stable ratio show `—`. Quiet days are chart GAPS and `—` health rows, never a fabricated `0`. A mid-stream upstream abort — transport break, converter-level `response.failed`, or an SSE `error` event — counts as an error even though the client already held a 200. In the perf tab, `h`/`l` (←/→) drill the series table into a single day (the user-facing per-day provider/model stats); `Esc`-ward `l` past today returns to the span aggregate.
- Data is rebuilt from the persisted request log on restart and retained 90 days; the tab title carries `timing since <date>` — the first day that actually observed v1 timing, not the oldest replayed legacy row.

The activity feed gains the same per-request view: a `t/s` column (e2e) on every completed row, and the click-expanded detail shows `e2e` / `est … post-delta` / `ttfb` / `first output` when recorded. Attach clients receive the same rows on `GET /llmux/dashboard` (`daily_perf`).

In the `sessions` tab, `o` cycles the sort (recent → tokens → requests), the mouse wheel moves the cursor, and a left-click selects the row under the pointer. The `t/s` column is the honest per-session output rate — Σ output tokens over Σ recorded request durations (raw-io records now carry `duration_ms`; pre-field history shows `—`, never a wall-clock-span fake).

### Config tab (the config editor)

The `config` tab (`c`, or click the tab bar) is a mouse-driven config editor covering the **entire** config schema — every field appears with an honest apply-state label:

- `live` — applies immediately (holder-backed; persisted read-merge-write first, then flipped in the running daemon). Covers scheduler mode + ceilings + usage max age, codex model/effort/fast, grok effort, routing on/off, raw-io capture on/off, email mask, quota fill, TUI effects, the Fable weekly gauge.
- `restart` — persisted now, effective on the next daemon start (routing default/on-empty group, raw-io retention and body cap, gradient speed, upstreams, port, max request bytes). The status line always says which — a saved-but-not-yet-live change never reads as applied.
- `session` — this TUI only (reset display).
- `ro` — managed elsewhere, with the note saying where (accounts → Accounts tab, per-account limits → `L`, events → `POST /llmux/events`, pricing/gradient colors/domain abbreviations → config file, `remote.api_key` → secret).

`j`/`k` (or the wheel) move the cursor; **Enter or a left-click on the value cell** activates the row — toggles flip in place, value rows open a prefilled edit prompt (`Enter` applies, `Esc` cancels). Blast-radius changes — scheduler mode, `routing.enabled`, raw-io capture, upstream endpoints, a ceiling set to 0, a retention decrease — ask `y/n` first. Everything goes through `POST /llmux/settings` semantics (identical validation locally and in attach mode); invalid values are rejected before anything is written.

The `misc` tab (`?`) carries the keybinding reference plus daemon facts: config path, account count, raw-io capture state and file size, and the activity-log size (sizes refresh at most every 30s).

## Install and launch details

### Homebrew channels

```bash
brew install 2lab-ai/tap/llmux
brew install 2lab-ai/tap/llmux-islands
brew install 2lab-ai/tap/llmux-preview
```

Use `llmux-preview` for the rolling preview channel. Use the stable `llmux` formula for normal daily work.

### Channels and updating

llmux is distributed through the [Homebrew tap](https://github.com/2lab-ai/homebrew-tap) on two channels: `stable` (formula `llmux`) and `preview` (formula `llmux-preview`). The active channel is derived from what brew has installed — there is no config field.

```bash
# Print the current channel
llmux channel

# Update in place on the current channel (brew upgrade), restarting the
# daemon if the binary changed or the running one is a different build
llmux update

# Switch channels now (brew uninstall old + install new, mirrored onto the
# llmux-islands cask; restarts a running daemon)
llmux channel preview
llmux channel stable
```

`llmux update` also restarts a running daemon whose version differs from the binary brew has installed, so a daemon left behind by an out-of-band `brew upgrade` — or by a restart skipped on an earlier run — converges instead of reporting "already up to date" while serving the old build. The comparison is server-vs-installed-artifact (`<installed llmux> --version`), not against the `llmux` process you invoked, which may itself be a stale keg or the other channel's binary.

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

## Multi-tenant client keys

One llmux server can serve several machines, each with its OWN issued key, so
usage is recorded per tenant (name + email) instead of one anonymous pool.

- Issue on the server: `llmux key new --name z-macbook --email z@2lab.ai`.
  The plaintext (`lmk-…`) is shown once; only its SHA-256 digest is stored.
- On each client PC, point llmux at the server in `~/.config/llmux.json`:
  `{ "remote": { "host": "<server>", "api_key": "lmk-…" } }` — `llmux run`
  then works unchanged and every request is attributed to that key.
- Authorization is two-axis: the key decides the *tenant* (attribution), its
  kind decides the *scope*. `default` keys reach the data plane only
  (`/v1/*`, `/models`); `admin` keys (and the config's own `proxy.api_key`)
  also unlock the `/llmux/*` control plane — key management, account
  mutation, dashboard/status reads. Loopback is NOT a privilege: control
  calls need an admin credential even from localhost (the local CLI presents
  `proxy.api_key` automatically). Keyless requests are loopback-only and are
  metered as the `local` tenant; keyless remote is refused.
- `suspend`/`remove`/`rotate` bite on the next request without a restart
  (the daemon keeps a live key registry; disk and memory update together).
  The last active admin credential can never be suspended or revoked —
  recovery from a lost admin key is editing the config on the server host.
- HTTP surface (admin): `GET /llmux/keys`, `POST /llmux/keys/new`,
  `POST /llmux/keys/suspend` `{id, suspended}`, `POST /llmux/keys/remove`
  `{id}`, `POST /llmux/keys/rotate` `{id}`.
- The dashboard's `keys` tab (`K`) is the admin view: every issued key with
  its name/email, kind, state, requests, tokens, API-equivalent cost, the
  used-from → used-to span, and a per-model breakdown — the builtin
  `local`/`legacy` buckets included, so every request is accounted for.
  The dashboard document carries the same data as `tenant_usage` and
  `client_keys` (metadata; never secrets). History persisted before this
  feature shows as the `unknown` tenant — it is never folded into a live
  bucket.

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
    "default_model": "gpt-5.6-sol",
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

Codex request-shaping is also settable live from the dashboard's Codex group: `default_model` (the model slug sent upstream, default `gpt-5.6-sol`), `fast` (sends `service_tier: "priority"` when `true`), and `reasoning_effort` (`none`|`minimal`|`low`|`medium`|`high`|`xhigh`|`max`, plus `ultra` on `gpt-5.6-sol`/`-terra`; `max`/`ultra` clamp to `xhigh` below the gpt-5.6 family; omitted by default). Grok request-shaping is the same minus `fast` (xAI has no service tier): `default_model` (default `grok-4.6`) and `reasoning_effort` (`none`|`low`|`medium`|`high`|`xhigh`, clamped per model at request time; omitted by default).

Accounts are `oauth` (Claude subscription), `apikey` (Anthropic API key), `codex` (ChatGPT/Codex subscription token), `grok` (xAI subscription token), or `openrouter` (an `sk-or-v1-…` API key). Claude accounts dedupe by `account_uuid`; Codex accounts dedupe by `account_id`; API keys and OpenRouter accounts dedupe by name. An `lm-...` proxy API key is generated on first run; localhost clients are exempt.

`email_anonymous` (default `false`) masks account emails on every display surface. The TUI render layer uses the same stable fake-email mapping as demo mode, and llmux Islands pixelizes emails in its Usage panel. The value is served in `GET /llmux/status` and can be flipped live via `POST /llmux/settings {"email_anonymous": true}` or the Islands ☰ toggle.

## Activity feed

The dashboard's activity panel shows one row per completed request
(2026-07-15 layout):

```text
▸ HH:MM:SS  kind  name  [model effort]  email…(10) → 200 3.1s 269tok $0.0079 «session» "input text to the screen edge"
```

- **name** — who sent it: the client-key name for keyed requests, the
  builtin bucket id for keyless traffic, shortened to the first
  whitespace-separated token's first 4 chars (`Z (U09…)` → `Z`,
  `angelo (U0…)` → `ange`, `local` → `loca`). Blank for history persisted
  before tenant attribution existed — never coerced to `local`. The
  expanded detail's `name` line keeps the full name plus the tenant id.
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
- **grok group** — `grok` accounts; models `grok`, `grok-*`.
- **openrouter group** — `openrouter` accounts; models `or-*`, a bare `or`, and `openrouter/*`.

Within the matched group the scheduler picks the best eligible account, sticky **per group**. The Claude pick and the Codex pick advance independently. An unrecognized or absent model falls back to `default_group`.

Turn routing **off** (`routing.enabled = false`) for the older behavior: the `model` is ignored for selection and Codex accounts become a cross-group overflow pool. A request lands on the best Claude/API account and only spills to Codex when every Claude account is exhausted.

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
| `enabled` | `true` | On = model→group routing; off = Codex-as-overflow behavior. |
| `claude_models` | `[]` | Models routed to the Claude group. Empty keeps the builtin rules; a non-empty list replaces them. |
| `codex_models` | `[]` | Models routed to the Codex group with the same semantics. |
| `grok_models` | `[]` | Models routed to the Grok group with the same semantics. |
| `openrouter_models` | `[]` | Models routed to the OpenRouter group with the same semantics. Empty keeps the builtin `or-` prefix, exact `or`, and `openrouter/` prefix rules. |
| `default_group` | `"claude"` | Group for an unmatched or model-less request: `"claude"`, `"codex"`, `"grok"`, or `"openrouter"`. |
| `on_empty_group` | `"error"` | When the matched group has no configured account: `"error"` returns a 404 `not_found_error`; `"fallback"` tries the remaining groups in the fixed `claude → codex → grok → openrouter` order and the first group with an account serves the request. |

Override tokens in the per-group model lists are matched in order, first-match-wins, case-insensitively. A bare token is a **prefix** (`"gpt-"`); prefix it with `~` for a **substring** (`"~codex"`) or `=` for an **exact** match (`"=gpt-5.5"`).

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

For account *selection*, the model string's only job is to choose the group. Any `gpt-*` / `codex` / `o1`–`o4` string that classifies to the Codex group is routed there. The model actually sent upstream is the one you asked for whenever llmux recognizes it: the known codex ids (`gpt-5.6-sol` / `-terra` / `-luna`, `gpt-5.5`, …) pass through verbatim, the bare aliases resolve (`sol` → `gpt-5.6-sol`, bare `gpt-5.6` → the sol flagship), and a trailing `[1m]` is stripped before the request leaves llmux, so `gpt-5.6-terra[1m]` reaches the backend as `gpt-5.6-terra`. Only an unrecognized id or a model-less request falls back to `codex.default_model` (default `gpt-5.6-sol`), set in config or live from the dashboard. `/llmux/status` reports the per-group current accounts under `current_by_group` and keeps a representative scalar `current` for back-compat.

### Context-window display for Codex models

When you route to the Codex group with a bare `gpt-5.5`, Claude Code's **remaining context** indicator is wrong because the client computes the window from the **model-name string**. llmux can route and stream the request, but it cannot set Claude Code's local context-window table:

- Claude Code derives the context **window** from the model name, client-side.
- Bare `gpt-5.5` can fall back to a **200,000** token window in Claude Code.
- The Codex `gpt-5.5` backend window is larger, and `gpt-5.5[1m]` makes Claude Code display a **1,000,000** token window.
- No `/v1/messages` response field or endpoint lets the proxy set the client's context window.

Use the `[1m]` suffix when you want Claude Code to display a 1M window:

```text
/model gpt-5.5[1m]
```

`gpt-5.5[1m]` still routes to the Codex group: the `gpt-` prefix still matches, and the suffix is stripped for routing/usage attribution — and, since 2026-08-21, on the codex request path too, so the suffix works on any codex id or alias (`gpt-5.6-sol[1m]`, `sol[1m]`) without degrading to the pin.

How much a 1M display over-reports depends on the model. For the gpt-5.6 family it is modest: probes 2026-08-21 against the ChatGPT-account backend accepted **910,229** input tokens on `gpt-5.6-sol` and were rejected at ~936k, and OpenAI publishes 1,050,000 total for the family — which is why `gpt-5.6-sol[1m]` / `gpt-5.6-terra[1m]` are catalog rows (see [models.md](models.md#the-codex-1m-rows)). For `gpt-5.5` (a 272k model) the 1M display remains a pure client-side workaround, not a promise that every long transcript will be accepted unchanged.

If a long session still blocks near the mid-200k range, use the empirical compaction workaround in [FAQ: gpt-5.5 context stops around 265k](faq.md#gpt-55-stops-around-265k-context-what-should-i-do).

## Codex backend

A ChatGPT/Codex subscription credential can be added with `llmux login --codex` (browser OAuth, falling back to importing `~/.codex/auth.json`) or imported directly:

```bash
llmux import --from ~/.codex/auth.json
```

The Codex provider translates Claude Code Messages requests into the Codex Responses backend and converts the stream back into Anthropic Messages SSE. The upstream model, a fast (`priority`) service tier, and reasoning effort are configurable (`codex.default_model` / `codex.fast` / `codex.reasoning_effort`) and adjustable live from the dashboard (`m` / `f` / `e`). Text, thinking summaries, and tool calls are supported. Images are dropped with a warning for now. `/v1/messages/count_tokens` is answered locally; other non-`/v1/messages` endpoints return a clear 501.

## OpenRouter backend

Add an OpenRouter account with the browser OAuth PKCE flow:

```bash
llmux login --openrouter
```

llmux opens `https://openrouter.ai/auth`, takes the callback on a localhost port, and exchanges the code for a long-lived `sk-or-v1-…` API key. There is **no token refresh**: the exchange yields an API key rather than an expiring access token, so the credential is never rotated. The account is named `or:<key label>` (the label comes from `GET /api/v1/key`), or `or:key-N` when no label is available.

If a browser cannot be opened, the callback times out (2 minutes), or the local port cannot be bound, the flow degrades to a key prompt on its own. You can also ask for that prompt up front — it is the way to reuse a key you already have:

```bash
llmux login --openrouter --paste
```

`--paste` requires `--openrouter`. The key is read from stdin, never printed back, and is masked in logs like every other credential.

Unlike the Codex and Grok backends, the OpenRouter provider is a **passthrough, not a translator**: OpenRouter exposes a native Anthropic Messages endpoint (`POST {openrouter.upstream}/messages`), so llmux forwards the Messages body unchanged apart from its `model` field, plus dropping the Claude-Code-local `anthropic-beta` / `anthropic-dangerous-direct-browser-access` headers OpenRouter does not know. There is no SSE conversion on this path. `/v1/messages/count_tokens` is answered locally (OpenRouter has no equivalent endpoint).

Model selection is the `or-` prefix — `/model or-ox-alpha` routes to the openrouter group and reaches OpenRouter as `stealth/ox-alpha`. A bare `or` (or a model-less request) uses `openrouter.default_model`, `or-<vendor>/<slug>` passes through verbatim for the models llmux does not curate, and an unrecognized bare name is forwarded as typed so OpenRouter's own 404 reaches you. The curated rows, their wire slugs, and their context windows are in [models.md](models.md#alias-semantics).

Cost: the ten curated models are free (`$0` in and out). An uncurated openrouter model has **no known rate** — the usage tab shows `—` and marks the bucket `+?` rather than pricing it as free, because the same group also fronts OpenRouter's paid catalog.

## Development

```bash
just check    # cargo fmt --check + cargo clippy --all-targets -- -D warnings + cargo test
just build    # cargo build --release --locked
```

Contributor conventions are in [`../AGENTS.md`](../AGENTS.md).
