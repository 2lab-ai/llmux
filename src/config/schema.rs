//! Config schema v1 for `~/.config/llmux.json` (see `.prd/02-architecture.md`).
//! These structs are the on-disk contract; they are complete and purely declarative.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::pricing::ModelPrice;

/// Default proxy listen port (teamclaude-compatible).
pub const DEFAULT_PORT: u16 = 3456;

/// Default ingress request-body admission cap: 64 MiB. A client request body
/// larger than this is rejected with 413 before it is buffered for forwarding,
/// bounding the heap one oversized request can pin (see
/// [`ProxyConfig::max_request_bytes`]).
pub const DEFAULT_MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;

/// Default upstream base URL.
pub const DEFAULT_UPSTREAM: &str = "https://api.anthropic.com";

/// Default OpenAI Codex backend base URL (the path `/responses` is appended
/// per request).
pub const DEFAULT_CODEX_UPSTREAM: &str = "https://chatgpt.com/backend-api/codex";

/// Default OpenAI OAuth token endpoint used to refresh Codex access tokens.
pub const DEFAULT_CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// Root of `~/.config/llmux.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Schema version. Always `1` for now; bump on breaking layout changes.
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub proxy: ProxyConfig,
    /// Upstream base URL requests are forwarded to.
    #[serde(default = "default_upstream")]
    pub upstream: String,
    /// OpenAI Codex backend endpoints (used only by `type: "codex"` accounts).
    #[serde(default)]
    pub codex: CodexConfig,
    /// xAI Grok backend endpoints (used only by `type: "grok"` accounts).
    /// Additive (`#[serde(default)]`): pre-grok configs load with defaults.
    #[serde(default)]
    pub grok: GrokConfig,
    /// OpenRouter backend endpoint + free-model pin (used only by
    /// `type: "openrouter"` accounts). Additive: pre-openrouter configs load
    /// with defaults (docs/openrouter/spec.md §R4).
    #[serde(default)]
    pub openrouter: OpenRouterConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    /// Model→backend-group routing (default: disabled — exactly today's
    /// overflow behavior). See [`RoutingConfig`].
    #[serde(default)]
    pub routing: RoutingConfig,
    /// API-equivalent pricing overrides (Feature D). Keyed by the *normalized*
    /// model slug (display suffixes like `[1m]` are stripped on lookup; match
    /// is case-insensitive). An entry here wins over the built-in default rate
    /// table in [`crate::pricing`]; absent/empty (the default) means "use the
    /// built-in rates". All rates are USD per 1,000,000 tokens. Additive: a
    /// config written before this field loads with an empty map.
    #[serde(default)]
    pub pricing: HashMap<String, ModelPrice>,
    /// Raw input/output payload capture (Feature B). When `enabled` (the
    /// default), the proxy appends one JSON line per request — the raw request
    /// and response bodies — to `$XDG_STATE_HOME/llmux/raw-io.jsonl`, pruned to
    /// `retention_days`. Best-effort: capture never affects the request path.
    /// See [`RawIoConfig`]. Additive: a config written before this field loads
    /// with the defaults (capture on, 90-day retention).
    #[serde(default)]
    pub raw_io: RawIoConfig,
    /// Render account emails masked on DISPLAY surfaces (the TUI and
    /// llmux-islands' pixelization), reusing the deterministic
    /// [`crate::demo::alias_always`] mapping at the render layer. This is a
    /// display-only concern: API responses (`/llmux/status`,
    /// `/llmux/dashboard`) keep REAL account names — clients that mask do so
    /// themselves per surface (islands needs the real data for its OFF state).
    /// Settable at runtime via `POST /llmux/settings` (persisted
    /// read-merge-write + applied live, no restart). Independent of
    /// `LLMUX_DEMO_MODE`, which keeps its load-time aliasing and therefore
    /// takes precedence when both are on. Additive (`#[serde(default)]`): a
    /// config written before this field loads with masking OFF.
    #[serde(default)]
    pub email_anonymous: bool,
    /// TUI cosmetic animations: the effort-token rainbow marquee (`max`) and
    /// the headline-model name gradient (`fable-5*`/`gpt-5.6-sol*`). Display-
    /// only, default ON. Set `false` for a calmer, still-legible board — the
    /// effort/model tokens keep a distinct STATIC color+bold instead of
    /// cycling. Working spinners animate regardless (they predate this knob).
    /// Carried on the dashboard document so BOTH TUI backends honor it, same
    /// convention as `email_anonymous`/`show_fable_weekly`. Additive
    /// (`#[serde(default = "default_true")]`): a config written before this
    /// field loads with effects ON.
    #[serde(default = "default_true")]
    pub tui_effects: bool,
    /// TUI gradient animation tuning (UI-8): drift speed + per-group base
    /// colors for the headline-model gradient, and an optional solid override
    /// for the `max` effort token. Display-only, carried on the dashboard
    /// document like `tui_effects` so both TUI backends honor it. Additive
    /// (`#[serde(default)]`): older configs load the defaults.
    #[serde(default)]
    pub tui_gradient: TuiGradient,
    /// Render the model-scoped "Fable" weekly gauge in the dashboard accounts
    /// table (fable-usage U9a). Display-only, default ON. This feature is
    /// TEMPORARY — the upstream Fable weekly limit is expected to disappear
    /// within ~a week — so it is opt-OUT: set `false` to render the accounts
    /// table exactly as before (no `Fbl` column / marker, no width taken). The
    /// daemon always COLLECTS and emits the scoped data (`/llmux/status`,
    /// `/llmux/dashboard`); this flag only gates the TUI's rendering of it.
    /// Additive (`#[serde(default = "default_true")]`): a config written before
    /// this field loads with the gauge ON.
    #[serde(default = "default_true")]
    pub show_fable_weekly: bool,
    /// TUI display: shorten well-known email domains in the accounts table
    /// (`ai3@insightquest.io` → `ai3@iq.io`). Render-only — API documents and
    /// interactive targets (switch/remove) keep real ids, same layering as
    /// `email_anonymous`. Additive
    /// (`#[serde(default = "default_domain_abbrev")]`): a config without this
    /// field loads the built-in `{"insightquest.io": "iq.io"}` map; set `{}`
    /// explicitly to disable abbreviation.
    #[serde(default = "default_domain_abbrev")]
    pub domain_abbrev: BTreeMap<String, String>,
    /// Which quantity the TUI quota gauges FILL with: `"remaining"` (default
    /// — a fresh account is a full green bar that drains as quota burns, per
    /// Z's 2026-07-09 direction) or `"used"` (the bar grows instead). Color
    /// bands stay keyed on USED utilization either way. The TUI `u` key
    /// overrides this live for the session; this field is the boot default.
    /// Additive (`#[serde(default)]`): older configs load as `remaining`.
    #[serde(default)]
    pub quota_display: QuotaDisplay,
    /// Account names the scheduler must NOT auto-select (operator pause).
    /// A paused account is ineligible for automatic selection AND manual
    /// switch (resume it first); its live windows keep polling so the gauges
    /// stay truthful. Kept as a top-level set (not a per-account field) so
    /// the on-disk account entries stay pure credentials. Additive
    /// (`#[serde(default)]`): older configs load with nothing paused.
    #[serde(default)]
    pub paused_accounts: std::collections::BTreeSet<String>,
    /// Per-account overrides of the scheduler's utilization ceilings, keyed by
    /// account name. Absent fields fall back to the global `scheduler.*`
    /// values. Kept as a top-level map (like `paused_accounts`) so account
    /// entries stay pure credentials. Additive: older configs load empty.
    #[serde(default)]
    pub account_limits: BTreeMap<String, AccountLimits>,
    /// Dashboard event banners (config `events`). Each entry is an
    /// [`EventBanner`] with an active window `[from, to)`; the TUI renders the
    /// active one with the EARLIEST `to` as a single top line, and nothing when
    /// none is active. Managed at runtime via `POST /llmux/events` (upsert by
    /// `id`) and remove. Additive (`#[serde(default)]`): older configs — and a
    /// config still carrying the removed singular `event` key — load with an
    /// empty list (the orphan key is ignored and dropped on the next save).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventBanner>,
    /// Point the CLI *client* commands at a remote llmux daemon instead of a
    /// local one (issue: remote proxy support). When `remote.host` is set,
    /// `llmux run` exports `ANTHROPIC_BASE_URL`/`ANTHROPIC_API_KEY` for the
    /// remote and does NOT auto-start a local daemon; `llmux server`,
    /// `llmux dashboard`, `llmux status`, and `llmux env` attach to / describe
    /// the remote. This is the CLI analogue of what llmux-islands already does
    /// (host/port + `x-api-key`). The `--remote host[:port]` global flag
    /// overrides it per-invocation. Additive (`#[serde(default)]`): a config
    /// written before this field loads with remote OFF (all-local behavior).
    #[serde(default)]
    pub remote: RemoteConfig,
    /// Downstream client keys the proxy ISSUES to its own callers
    /// (multi-tenant metering + access control; issue #22). A distinct
    /// namespace from upstream `accounts` credentials: each entry carries a
    /// stable attribution id, tenant metadata (name/email), an authz kind, and
    /// the SHA-256 digest of the secret — the plaintext is shown exactly once
    /// at issuance and never stored (the human-mediated config-excerpt channel
    /// cannot be closed by tests, so nothing secret lives here). Entries are
    /// soft-revoked (`revoked_at_ms`), never hard-deleted, so historical usage
    /// keeps resolving to name/email forever. Additive (`#[serde(default)]`):
    /// older configs load with no client keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_keys: Vec<ClientKey>,
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
}

/// Authorization kind of an issued client key: `default` unlocks the data
/// plane only (`/v1/*` forwarding); `admin` additionally unlocks the control
/// plane (`/llmux/*` management + dashboard).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientKeyKind {
    #[default]
    Default,
    Admin,
}

/// One issued downstream client key (config `client_keys`). The secret itself
/// is NOT stored — only its SHA-256 digest (hex, `sha256:` prefixed) plus a
/// short display prefix. `id` is the immutable attribution identity: usage
/// records reference it, rotation replaces the digest under the same `id`,
/// and soft-revocation preserves the row so old usage keeps its name/email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientKey {
    /// Immutable attribution id (`k-` + 8 hex chars). Mutations target this.
    pub id: String,
    /// Human label for the tenant (PC/user), required at issuance.
    pub name: String,
    /// Optional tenant email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default)]
    pub kind: ClientKeyKind,
    /// First characters of the issued secret (`lmk-xxxx`), display only.
    pub key_prefix: String,
    /// `sha256:<hex>` of the full issued secret; the auth gate compares
    /// digests, never plaintext.
    pub key_digest: String,
    /// Operator pause: a suspended key authenticates to 401 until resumed.
    #[serde(default)]
    pub suspended: bool,
    /// Issuance timestamp (epoch ms).
    pub created_at_ms: u64,
    /// Soft-revocation timestamp (epoch ms). A revoked key no longer
    /// authenticates but the row is preserved for attribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ms: Option<u64>,
}

/// One dashboard event banner (an element of config `events`). Display-only:
/// the TUI renders the active banner (`from <= now < to`) with the earliest
/// `to` as a single top line. `id` is the stable upsert key (`POST
/// /llmux/events` replaces the entry with the same `id`); `from`/`to` are
/// timestamps in EITHER RFC3339-with-offset (`2026-07-12T23:59:59-07:00`) or
/// compact `YYYYMMDDHHMM` (12 digits, LOCAL wall-clock) form — see
/// [`crate::event::parse_event_time`]; `content` is the rendered message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventBanner {
    /// Stable identifier, the upsert/remove key (e.g. `20260712-fable5`).
    pub id: String,
    /// Window start (inclusive). RFC3339-with-offset or compact
    /// `YYYYMMDDHHMM` (local time).
    pub from: String,
    /// Window end (exclusive). Same two accepted forms as `from`; must parse
    /// to an instant strictly after `from`.
    pub to: String,
    /// Rendered banner text, e.g. `Fable 5 Available until 7/12`.
    pub content: String,
}

/// Per-account utilization-ceiling overrides (config `account_limits`); every
/// field optional — `None` falls back to the global scheduler value.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct AccountLimits {
    /// Override of `scheduler.five_hour_max` (0..=1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour_max: Option<f64>,
    /// Override of `scheduler.seven_day_max` (0..=1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day_max: Option<f64>,
    /// Override of `scheduler.fable_weekly_max` (0..=1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fable_weekly_max: Option<f64>,
}

impl AccountLimits {
    /// True when no field overrides anything — the map entry can be dropped.
    pub fn is_empty(&self) -> bool {
        self.five_hour_max.is_none()
            && self.seven_day_max.is_none()
            && self.fable_weekly_max.is_none()
    }
}

/// Fill direction for the TUI quota gauges (config `quota_display`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuotaDisplay {
    /// Fill = fraction of the window already used (the bar grows).
    Used,
    /// Fill = fraction still available before the ceiling (default — a full
    /// green bar drains toward the reset).
    #[default]
    Remaining,
}

/// Built-in domain abbreviations for the accounts table (`domain_abbrev`).
pub fn default_domain_abbrev() -> BTreeMap<String, String> {
    BTreeMap::from([("insightquest.io".to_string(), "iq.io".to_string())])
}

/// Remote-daemon target for the CLI client commands. All fields optional:
/// `host` unset (the default) means "operate against the local daemon" and the
/// whole section is inert. When `host` is set, off-loopback access requires the
/// remote's proxy `api_key` presented as `x-api-key` — so `api_key` is
/// effectively mandatory unless the remote runs with no key configured.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfig {
    /// Remote daemon host (e.g. `llmux-host` or `100.64.0.1`). Unset →
    /// remote mode OFF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Remote daemon port. Unset → [`DEFAULT_PORT`] (3456).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Proxy `api_key` presented to the remote as `x-api-key`. Required
    /// off-loopback unless the remote has no api_key configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: default_version(),
            proxy: ProxyConfig::default(),
            upstream: default_upstream(),
            codex: CodexConfig::default(),
            grok: GrokConfig::default(),
            openrouter: OpenRouterConfig::default(),
            scheduler: SchedulerConfig::default(),
            routing: RoutingConfig::default(),
            pricing: HashMap::new(),
            raw_io: RawIoConfig::default(),
            email_anonymous: false,
            tui_effects: true,
            tui_gradient: TuiGradient::default(),
            show_fable_weekly: true,
            domain_abbrev: default_domain_abbrev(),
            quota_display: QuotaDisplay::default(),
            paused_accounts: std::collections::BTreeSet::new(),
            account_limits: BTreeMap::new(),
            events: Vec::new(),
            remote: RemoteConfig::default(),
            client_keys: Vec::new(),
            accounts: Vec::new(),
        }
    }
}

/// Raw input/output payload capture config (Feature B). The proxy keeps a
/// verbatim record of each proxied request's request body and the response
/// body delivered to the client, so traffic can be replayed/audited offline.
///
/// This is DISTINCT from activity persistence (`activity.jsonl`, per-request
/// metadata): this store holds the actual payload bytes. Capture is strictly
/// best-effort — it never blocks, mutates, or slows the bytes forwarded to the
/// client, and every IO/serialization error is swallowed (see
/// [`crate::proxy::raw_io`]). All fields are additive (`#[serde(default)]`), so
/// a config written before this section existed loads with capture ON and a
/// 90-day window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawIoConfig {
    /// Master switch. When `true` (the default), each request appends one
    /// [`crate::proxy::raw_io::RawIoRecord`] to the raw-io log. When `false`,
    /// nothing is captured or written.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Days of history to keep. On startup, records older than
    /// `now - retention_days * 86_400_000 ms` are pruned. `0` = keep forever
    /// (no pruning). Default `90`.
    #[serde(default = "default_raw_io_retention_days")]
    pub retention_days: u64,
    /// Per-body capture cap, in bytes, applied identically to the request body
    /// and the response body on BOTH the streaming and non-streaming paths. A
    /// body over this is clipped on a UTF-8 char boundary with a
    /// `…[truncated N bytes]` marker. This is the raw-io retention cap and is
    /// DELIBERATELY decoupled from the debug request-log's 8 KiB
    /// [`crate::proxy::logging::BODY_LOG_LIMIT`]: the debug log stays a short
    /// 8 KiB excerpt while raw-io retains the full (bounded) payload — most LLM
    /// responses stream tens to hundreds of KB, so an 8 KiB raw-io cap would
    /// lose almost the entire response. Default
    /// [`crate::proxy::raw_io::RESPONSE_CAP_BYTES`] (8 MiB), generous for a real
    /// request/response yet bounding the memory a pathological body can pin.
    #[serde(default = "default_raw_io_max_body_bytes")]
    pub max_body_bytes: usize,
}

impl Default for RawIoConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: default_raw_io_retention_days(),
            max_body_bytes: default_raw_io_max_body_bytes(),
        }
    }
}

/// TUI gradient animation tuning (UI-8): how fast the headline-model /
/// max-effort gradients drift and which base colors the solid (per-group)
/// gradient breathes. Display-only — carried on the dashboard document (like
/// `tui_effects`) so the local AND attach TUIs honor it; read at boot from
/// the config file.
///
/// ```json
/// "tui_gradient": { "speed": 2.0, "claude": "#ff79c6", "codex": "#56dcdc" }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TuiGradient {
    /// Speed multiplier for the temporal drift of BOTH gradient modes.
    /// `1.0` (default) is the baseline drift; `2.0` doubles it, `0.5` halves
    /// it. Non-finite or non-positive values fall back to `1.0` at render
    /// time (the TUI never freezes on a bad config).
    #[serde(default = "default_gradient_speed")]
    pub speed: f32,
    /// Base color (hex `#rrggbb`) the claude headline-model gradient breathes
    /// around. Unparseable values fall back to the built-in default.
    #[serde(default = "default_gradient_claude")]
    pub claude: String,
    /// Base color (hex `#rrggbb`) for the codex headline-model gradient.
    #[serde(default = "default_gradient_codex")]
    pub codex: String,
    /// Optional hex base for the `max` effort token: when set, the rainbow is
    /// replaced by a solid gradient on this color; `None` (default) keeps the
    /// 3-phase sine rainbow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_effort: Option<String>,
}

impl Default for TuiGradient {
    fn default() -> Self {
        Self {
            speed: default_gradient_speed(),
            claude: default_gradient_claude(),
            codex: default_gradient_codex(),
            max_effort: None,
        }
    }
}

fn default_gradient_speed() -> f32 {
    1.0
}

fn default_gradient_claude() -> String {
    "#ff79c6".to_string()
}

fn default_gradient_codex() -> String {
    "#56dcdc".to_string()
}

/// Model→backend-group routing config. When `enabled` is false (the
/// default), routing is OFF and the scheduler behaves exactly as before:
/// no group filter anywhere, codex accounts stay the cross-group overflow
/// pool. When `enabled` is true, an inbound request's `model` selects a
/// backend group (claude vs codex) and the scheduler picks within that
/// group, sticky per group.
///
/// Empty `claude_models` / `codex_models` keep the builtin rules for that
/// group (see [`crate::routing::Classifier`]); a non-empty list replaces the
/// builtins for that group. All fields are additive (`#[serde(default)]`),
/// so a config written before routing existed loads with `enabled = false`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingConfig {
    /// Master switch. When `true` (now the default), an inbound request's
    /// `model` selects its backend group and the scheduler picks within it —
    /// claude models → claude accounts, codex models → codex accounts,
    /// independent of which account is "current". This is what makes
    /// `gpt-5.5` reach a codex account instead of being forwarded verbatim to
    /// Anthropic (which 404s "model not found"). When `false`, no group filter
    /// is applied and codex stays a cross-group overflow pool (the original
    /// behavior). Toggleable from the dashboard.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Models routed to the claude group (empty → builtin claude rules).
    #[serde(default)]
    pub claude_models: Vec<String>,
    /// Models routed to the codex group (empty → builtin codex rules).
    #[serde(default)]
    pub codex_models: Vec<String>,
    /// Models routed to the grok group (empty → builtin grok rules).
    /// Additive: pre-grok configs load with the builtin `grok` prefix rule.
    #[serde(default)]
    pub grok_models: Vec<String>,
    /// Models routed to the openrouter group (empty → builtin `or-` / bare
    /// `or` / `openrouter/` rules). Additive: pre-openrouter configs load
    /// with those builtins (docs/openrouter/spec.md §R2).
    #[serde(default)]
    pub openrouter_models: Vec<String>,
    /// Group an unmatched / model-less request routes to. Default `"claude"`.
    #[serde(default = "default_routing_group")]
    pub default_group: String,
    /// What to do when the matched group has ZERO CONFIGURED accounts
    /// (parked/limited accounts still count as configured): `"error"`
    /// (default) returns a clean 404 not_found_error; `"fallback"` tries the
    /// remaining groups in the fixed `Claude → Codex → Grok → OpenRouter`
    /// order and the
    /// first group with ≥1 configured account serves the request under its
    /// own provider semantics (docs/grok/spec.md §R5).
    #[serde(default = "default_on_empty_group")]
    pub on_empty_group: String,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            claude_models: Vec::new(),
            codex_models: Vec::new(),
            grok_models: Vec::new(),
            openrouter_models: Vec::new(),
            default_group: default_routing_group(),
            on_empty_group: default_on_empty_group(),
        }
    }
}

/// OpenAI Codex backend endpoints + request defaults. Endpoint defaults target
/// the ChatGPT backend the codex CLI itself uses; overridable for tests/staging.
/// The request-shaping fields (`default_model`, `fast`, `reasoning_effort`)
/// mirror what the codex CLI sets on its Responses requests and are settable
/// from the dashboard's codex group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexConfig {
    /// Base URL the Responses request is POSTed to (`{upstream}/responses`).
    #[serde(default = "default_codex_upstream")]
    pub upstream: String,
    /// OAuth token endpoint for Codex refresh-token grants.
    #[serde(default = "default_codex_token_url")]
    pub token_url: String,
    /// Model slug the codex provider requests upstream. Was a hardcoded
    /// `gpt-5.5` const; now config-driven so the dashboard can change it.
    /// Additive: configs written before this field load with the default.
    #[serde(default = "default_codex_model")]
    pub default_model: String,
    /// When set, llmux reports THIS model name to the client (Claude Code) in
    /// the response instead of the real codex model. Claude Code picks its
    /// context-window denominator by a hardcoded model-name lookup
    /// (unknown→200k, known 1M models→1,000,000) and offers no per-model
    /// window override, so set this to a 1M-window model name (e.g.
    /// `claude-opus-4-8`) to stop Claude Code cutting codex sessions off at
    /// ~200k. Pair with the `CLAUDE_CODE_AUTO_COMPACT_WINDOW=272000` env var on
    /// the Claude Code side to make auto-compaction fire at codex's real limit.
    /// None (default) = report the real codex model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_model: Option<String>,
    /// "Fast" service tier. When `true`, the Responses request carries
    /// `service_tier: "priority"` — the exact wire value the codex CLI sends
    /// for its fast mode (config stores "fast", wire sends "priority"). When
    /// `false`, no `service_tier` field is sent. Default `false`.
    #[serde(default)]
    pub fast: bool,
    /// Reasoning effort for the Responses request: one of
    /// `none|minimal|low|medium|high|xhigh` (the codex CLI's `ReasoningEffort`
    /// wire values). `None` → omit `reasoning.effort` and let the backend use
    /// the model's default. Display + request only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Append a JSON-line trace of every codex request/response to
    /// `$XDG_STATE_HOME/llmux/codex-trace.jsonl` (input size breakdown +
    /// terminal outcome + verbatim upstream usage). Best-effort: write errors
    /// never affect the request. Default `true` while we diagnose token issues.
    #[serde(default = "default_true")]
    pub trace: bool,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            upstream: default_codex_upstream(),
            token_url: default_codex_token_url(),
            default_model: default_codex_model(),
            client_model: None,
            fast: false,
            reasoning_effort: None,
            trace: true,
        }
    }
}

/// xAI Grok backend endpoints + request defaults (docs/grok/spec.md §R1/§R4).
/// The chat upstream defaults to the Grok-CLI chat proxy (the subscription
/// path); the OAuth endpoints are discovered via OIDC at login time and the
/// token endpoint is persisted PER ACCOUNT (`AccountCredential::Grok`), so no
/// token URL lives here. No `fast` — xAI has no service tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrokConfig {
    /// Base URL the Responses request is POSTed to (`{upstream}/responses`).
    /// The Grok-CLI identity headers are only attached when this is the
    /// official cli-chat-proxy host (docs/grok/spec.md §R1).
    #[serde(default = "default_grok_upstream")]
    pub upstream: String,
    /// Model slug the grok provider requests upstream when the client's
    /// requested model is not grok-shaped. Settable from `POST /llmux/grok`.
    #[serde(default = "default_grok_model")]
    pub default_model: String,
    /// When set, llmux reports THIS model name to the client instead of the
    /// real grok model (same contract as `codex.client_model`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_model: Option<String>,
    /// Reasoning effort default: superset `none|low|medium|high`; the
    /// per-model clamp happens at request time (docs/grok/spec.md §R1).
    /// `None` → omit `reasoning` and let the backend default (high) apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Append a JSON-line trace of every grok request/response, mirroring
    /// `codex.trace`. Default `false` (flip on while diagnosing).
    #[serde(default)]
    pub trace: bool,
}

impl Default for GrokConfig {
    fn default() -> Self {
        Self {
            upstream: default_grok_upstream(),
            default_model: default_grok_model(),
            client_model: None,
            reasoning_effort: None,
            trace: false,
        }
    }
}

/// OpenRouter backend endpoint + free-model pin (docs/openrouter/spec.md §R4).
///
/// Deliberately much thinner than [`GrokConfig`]/[`CodexConfig`]: OpenRouter
/// speaks the **Anthropic Messages** wire format natively
/// (`POST {upstream}/messages`, live-probed 2026-08-21), so llmux does not
/// shape the request at all beyond rewriting the `model` field. There is
/// therefore no `reasoning_effort` / `fast` / `trace` knob here — effort rides
/// through as client metadata exactly as it does on the claude passthrough.
///
/// There is also no token endpoint: the OAuth PKCE exchange yields a
/// long-lived API key (`sk-or-v1-…`), not an expiring access token, so the
/// credential never needs refreshing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRouterConfig {
    /// Base URL the client's verbatim path is appended to — i.e. the request
    /// goes to `{upstream}/v1/messages`. Host root, NOT `…/api/v1`; see
    /// [`default_openrouter_upstream`].
    #[serde(default = "default_openrouter_upstream")]
    pub upstream: String,
    /// Slug the bare `or` family alias resolves to — the free-model pin.
    /// Mirrors bare `grok` resolving to the live grok pin.
    #[serde(default = "default_openrouter_model")]
    pub default_model: String,
}

impl Default for OpenRouterConfig {
    fn default() -> Self {
        Self {
            upstream: default_openrouter_upstream(),
            default_model: default_openrouter_model(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Listen port. Default 3456.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Proxy-level API key (`lm-...`), auto-generated on first run.
    /// Localhost clients are exempt from presenting it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Forward-path idle (inactivity) timeout, seconds: how long the proxy
    /// waits for the NEXT byte from upstream after the connection is
    /// established before aborting the stream. This is an inactivity ceiling,
    /// NOT a total-request deadline — legitimate LLM streams run for minutes
    /// with long inter-token gaps, so the clock resets on every chunk. A
    /// silent upstream that connects and then stalls would otherwise hang the
    /// session and pin the account; this bounds the silence. Default 120.
    /// Applied two ways (defense in depth): `reqwest`'s `read_timeout` on the
    /// serving client and a per-chunk `tokio::time::timeout` around the SSE
    /// pump (see [`crate::proxy::sse::passthrough_body`]).
    #[serde(default = "default_forward_idle_timeout_secs")]
    pub forward_idle_timeout_secs: u64,
    /// Hard cap, in bytes, on a client request body buffered on the ingress
    /// forward path before it is relayed upstream. The body must be fully
    /// buffered (it can be replayed across account retries), so an unbounded
    /// read lets one oversized request pin arbitrary heap and OOM the daemon.
    /// A request whose body exceeds this returns 413 Payload Too Large.
    /// Default [`crate::config::DEFAULT_MAX_REQUEST_BYTES`] (64 MiB).
    ///
    /// This is the ingress admission limit and is DELIBERATELY distinct from
    /// [`RawIoConfig::max_body_bytes`] (the observability-tee retention cap):
    /// raw-io clips what is *retained* for inspection; this rejects what is
    /// *accepted* for forwarding. Additive (`#[serde(default)]`) so configs
    /// written before this field load with the 64 MiB default.
    #[serde(default = "default_max_request_bytes")]
    pub max_request_bytes: usize,
    /// On-demand idle-account usage probe (issue #21). Additive
    /// (`#[serde(default)]`), so a config written before this section existed
    /// loads with the conservative defaults — and, critically, with probing
    /// DISABLED (`enabled = false`).
    #[serde(default)]
    pub idle_probe: IdleProbeConfig,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            api_key: None,
            forward_idle_timeout_secs: default_forward_idle_timeout_secs(),
            max_request_bytes: default_max_request_bytes(),
            idle_probe: IdleProbeConfig::default(),
        }
    }
}

/// Idle-account usage probe (issue #21, extended by #45). An account with no
/// known 5h/7d window produces no usage data for the scheduler's
/// ranking/display. When this is enabled, such an account can be populated by a
/// single `max_tokens = 1` request through its own credential (`POST
/// /v1/messages` for oauth/apikey, `POST /responses` for codex): the response's
/// `anthropic-ratelimit-*` / `x-codex-*` headers feed the existing
/// [`crate::scheduler::window::WindowSource::Headers`] path.
///
/// Two delivery modes, both behind the SAME guards:
/// - **On demand** (#21): the forward path probes idle accounts so the next
///   ranking/display has real data.
/// - **Timer sweep** (#45): when `sweep_secs > 0`, a background task fires the
///   same probe for EVERY cold account (any provider) on a timer, so their
///   windows stay populated with ZERO client traffic. The usage poller already
///   covers cold oauth accounts, so in practice the sweep is what keeps cold
///   Codex and api-key accounts (which have no poller) visible; an oauth account
///   the poller already warmed is skipped because it already has a window.
///
/// Both modes share a global kill-switch (`enabled = false` disables ALL
/// probing) and a per-account cooldown so a single account is probed at most
/// once per `per_account_cooldown_secs`. Defaults are ALWAYS-ON (issue #45):
/// probing enabled with an hourly sweep, so cold accounts populate their
/// windows out of the box; the kill-switch remains for operators who want
/// zero probe traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleProbeConfig {
    /// Master kill-switch. `true` (the default, per issue #45's always-on
    /// mandate) allows a single gated probe per idle account; `false` disables
    /// all idle probing (on-demand AND the timer sweep). Cost when on is
    /// bounded by the per-account cooldown: at most one `max_tokens = 1`
    /// request per cold account per hour, and only while it has no window.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Minimum wall-clock gap between two probes of the SAME account, seconds.
    /// Once an account is probed, a second probe is suppressed until this
    /// elapses — so a hot ranking/display path that keeps asking about an idle
    /// account never bursts a probe per request. Default 3600 (1 hour).
    #[serde(default = "default_idle_probe_cooldown_secs")]
    pub per_account_cooldown_secs: u64,
    /// Timer-sweep cadence for keeping ALL cold accounts (any provider) warm
    /// (issue #45), seconds. Default 900 (15 min — one tick per default
    /// cooldown). `0` disables the sweep entirely; the probe then stays purely
    /// on-demand. When `> 0` and `enabled = true`, a background task probes
    /// every cold account every `sweep_secs` seconds, reusing the same
    /// kill-switch + per-account cooldown (so the cooldown — not this cadence
    /// — bounds cost: at most one probe per account per
    /// `per_account_cooldown_secs` regardless of how often the sweep ticks).
    #[serde(default = "default_idle_probe_sweep_secs")]
    pub sweep_secs: u64,
    /// Age (seconds) past which an account's freshest 5h/7d observation counts
    /// as STALE and the account becomes probe-eligible again (Z 2026-07-15:
    /// cold subscriptions must keep refreshing, not freeze at their last
    /// reading). Before this knob an account was probed only while it had NO
    /// window at all, so one successful probe froze its display forever unless
    /// real traffic or the oauth poller touched it — codex/apikey accounts
    /// have no poller, so their gauges went permanently stale. Default 900
    /// (15 min). `0` disables staleness re-probing (windowless-only, the old
    /// behavior).
    #[serde(default = "default_idle_probe_stale_after_secs")]
    pub stale_after_secs: u64,
}

impl Default for IdleProbeConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            per_account_cooldown_secs: default_idle_probe_cooldown_secs(),
            sweep_secs: default_idle_probe_sweep_secs(),
            stale_after_secs: default_idle_probe_stale_after_secs(),
        }
    }
}

/// Scheduler thresholds and polling cadence (FR3).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SchedulerConfig {
    /// Max 5h-window utilization before an account becomes ineligible. Default 0.90.
    #[serde(default = "default_five_hour_max")]
    pub five_hour_max: f64,
    /// Max 7d-window utilization before an account becomes ineligible. Default 0.99.
    #[serde(default = "default_seven_day_max")]
    pub seven_day_max: f64,
    /// `/api/oauth/usage` poll interval per account, seconds. Default 300.
    #[serde(default = "default_usage_poll_secs")]
    pub usage_poll_secs: u64,
    /// Usage data older than this is stale; stale accounts are ineligible
    /// (unless ALL accounts are stale — headers-only fallback). Default 600.
    #[serde(default = "default_usage_max_age_secs")]
    pub usage_max_age_secs: u64,
    /// Background token refresh threshold: oauth tokens whose remaining
    /// lifetime drops below this many seconds are refreshed by the server's
    /// background task, independent of client traffic. Default 7h (access
    /// tokens live ~8h).
    #[serde(default = "default_refresh_ahead_secs")]
    pub refresh_ahead_secs: u64,
    /// Max Fable-weekly (7d Fbl) utilization before the account is
    /// preemptively excluded for FABLE requests (non-Fable traffic is never
    /// gated by this). Default 0.98. Additive: older configs load 0.98.
    #[serde(default = "default_fable_weekly_max")]
    pub fable_weekly_max: f64,
    /// Which selection algorithm runs (see README "Schedulers"): `default`
    /// (quota-maximizing perishability score) or `round-robin` (sequential
    /// exhaust in roster order — the fewest-switches mode; each account
    /// switch invalidates the upstream prompt cache, so fewer switches =
    /// fewer re-read tokens). Toggle live from the TUI with `S`. Additive:
    /// older configs load `default`.
    #[serde(default)]
    pub mode: SchedulerMode,
}

/// Selection algorithm (config `scheduler.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SchedulerMode {
    /// Quota-maximizing: perishability-scored pick with damped proactive
    /// switching (the historical behavior).
    #[default]
    Default,
    /// Sequential exhaust: stay on the current account until it is hard
    /// ineligible, then move to the NEXT account in roster order (wrapping).
    /// Minimizes account switches (prompt-cache friendly).
    RoundRobin,
}

impl SchedulerMode {
    /// Stable wire/display label (matches the serde encoding).
    pub fn label(self) -> &'static str {
        match self {
            SchedulerMode::Default => "default",
            SchedulerMode::RoundRobin => "round-robin",
        }
    }
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            five_hour_max: default_five_hour_max(),
            seven_day_max: default_seven_day_max(),
            usage_poll_secs: default_usage_poll_secs(),
            usage_max_age_secs: default_usage_max_age_secs(),
            refresh_ahead_secs: default_refresh_ahead_secs(),
            fable_weekly_max: default_fable_weekly_max(),
            mode: SchedulerMode::default(),
        }
    }
}

/// One account entry. `name` is the user-facing identifier (unique within the
/// file); the credential variant carries the type-specific fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountConfig {
    pub name: String,
    #[serde(flatten)]
    pub credential: AccountCredential,
}

/// Credential payload, tagged by `"type": "oauth" | "apikey" | "codex"`
/// in JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AccountCredential {
    /// Claude subscription via PKCE OAuth.
    Oauth {
        /// `accountUuid` from `/api/oauth/profile`; dedup key across imports.
        /// Empty string = unknown (e.g. imported before any profile fetch).
        account_uuid: String,
        access_token: String,
        refresh_token: String,
        /// Access-token expiry, epoch milliseconds. Upstream may deliver
        /// seconds — normalize on ingest (`< 1e12` → ×1000).
        expires_at_ms: u64,
        /// Subscription tier when known (e.g. `max`); display only.
        /// Omitted from the file when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tier: Option<String>,
        /// Epoch ms of the last successful token refresh (initial login
        /// counts as a refresh). `None` on configs written before this
        /// field existed — rendered as "never". Additive: absent in JSON
        /// until the first refresh after upgrade.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_refresh_ms: Option<u64>,
    },
    /// Plain Anthropic API key.
    Apikey { api_key: String },
    /// OpenAI Codex subscription (ChatGPT OAuth, imported from
    /// `~/.codex/auth.json`). Served by the codex provider, not Anthropic.
    Codex {
        /// `tokens.account_id` from `~/.codex/auth.json`; dedup key.
        account_id: String,
        access_token: String,
        refresh_token: String,
        /// Access-token expiry, epoch milliseconds (decoded from the JWT
        /// `exp` claim). `0` = unknown.
        expires_at_ms: u64,
        /// Epoch ms of the last successful token refresh; see the `Oauth`
        /// variant's field of the same name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_refresh_ms: Option<u64>,
    },
    /// xAI Grok subscription (device-code OAuth against auth.x.ai,
    /// docs/grok/spec.md §R1). Served by the grok provider.
    ///
    /// NOTE downgrade contract: a config containing this variant does not
    /// parse under pre-grok binaries (internally-tagged enum) — remove
    /// `grok:*` accounts before downgrading (spec §Compatibility & rollback).
    Grok {
        /// `sub` claim from the id_token; dedup key across logins.
        /// Empty string = unknown.
        #[serde(default)]
        subject: String,
        access_token: String,
        refresh_token: String,
        /// Access-token expiry, epoch milliseconds. `0` = unknown.
        expires_at_ms: u64,
        /// OAuth token endpoint resolved via OIDC discovery at login time and
        /// persisted so refreshes don't re-discover (CLIProxyAPI parity).
        /// Empty string → re-discover on the next refresh.
        #[serde(default)]
        token_endpoint: String,
        /// Epoch ms of the last successful token refresh; see the `Oauth`
        /// variant's field of the same name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_refresh_ms: Option<u64>,
    },
    /// OpenRouter API key (`sk-or-v1-…`), minted by the OAuth PKCE flow
    /// (`llmux login --openrouter`) or pasted by hand. Served by the
    /// openrouter provider (docs/openrouter/spec.md §R1).
    ///
    /// NO refresh fields: OpenRouter's PKCE exchange returns a long-lived key
    /// rather than an expiring access token, so this credential is closer to
    /// [`Self::Apikey`] than to the oauth-style variants.
    ///
    /// NOTE downgrade contract: a config containing this variant does not
    /// parse under pre-openrouter binaries (internally-tagged enum) — remove
    /// `or:*` accounts before downgrading.
    OpenRouter {
        /// The `sk-or-v1-…` key.
        api_key: String,
        /// Key label from `GET /api/v1/key` (empty when unavailable). Used
        /// only for the account NAME; it is not a dedup key — OpenRouter
        /// labels are not unique per key.
        #[serde(default)]
        label: String,
    },
}

impl AccountCredential {
    /// Stable kind label for status output and logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Oauth { .. } => "oauth",
            Self::Apikey { .. } => "apikey",
            Self::Codex { .. } => "codex",
            Self::Grok { .. } => "grok",
            Self::OpenRouter { .. } => "openrouter",
        }
    }

    /// Epoch ms of the last successful token refresh. `None` for apikey
    /// accounts (nothing to refresh) and for oauth-style accounts that have
    /// not been refreshed since the field was introduced.
    pub fn last_refresh_ms(&self) -> Option<u64> {
        match self {
            Self::Oauth {
                last_refresh_ms, ..
            }
            | Self::Codex {
                last_refresh_ms, ..
            }
            | Self::Grok {
                last_refresh_ms, ..
            } => *last_refresh_ms,
            Self::Apikey { .. } | Self::OpenRouter { .. } => None,
        }
    }

    /// The dedup key for oauth-style accounts: a non-empty `account_uuid`
    /// (Anthropic) or `account_id` (Codex). `None` for apikey accounts and
    /// for accounts whose identity is not (yet) known.
    pub fn account_uuid(&self) -> Option<&str> {
        match self {
            Self::Oauth { account_uuid, .. } if !account_uuid.is_empty() => Some(account_uuid),
            Self::Codex { account_id, .. } if !account_id.is_empty() => Some(account_id),
            Self::Grok { subject, .. } if !subject.is_empty() => Some(subject),
            _ => None,
        }
    }
}

/// Result of [`Config::upsert_account`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Added,
    Updated,
}

impl Config {
    /// Find an account's index by identity: a non-empty oauth
    /// `account_uuid` wins, falling back to `name` (FR2 dedup order,
    /// mirroring teamclaude's `findConfigAccount`).
    pub fn find_account(&self, account: &AccountConfig) -> Option<usize> {
        if let Some(uuid) = account.credential.account_uuid() {
            if let Some(idx) = self
                .accounts
                .iter()
                .position(|a| a.credential.account_uuid() == Some(uuid))
            {
                return Some(idx);
            }
        }
        self.accounts.iter().position(|a| a.name == account.name)
    }

    /// Insert or replace an account, keyed by `account_uuid` then `name`.
    /// On a match the whole entry is replaced in place (a re-login may
    /// rename the account to its profile email).
    pub fn upsert_account(&mut self, account: AccountConfig) -> Upsert {
        match self.find_account(&account) {
            Some(idx) => {
                self.accounts[idx] = account;
                Upsert::Updated
            }
            None => {
                self.accounts.push(account);
                Upsert::Added
            }
        }
    }

    /// Remove an account by exact name. Returns `true` when one was removed.
    pub fn remove_account(&mut self, name: &str) -> bool {
        let before = self.accounts.len();
        self.accounts.retain(|a| a.name != name);
        self.accounts.len() != before
    }

    /// Persist refreshed oauth tokens onto the account identified by
    /// `ident` (matched against `account_uuid`/`account_id` first, then
    /// `name`). `refresh_token: None` preserves the stored refresh token
    /// (the token endpoint may omit a new one). `refreshed_at_ms` records
    /// WHEN the refresh happened (epoch ms) for the dashboard's
    /// "refreshed ago" display. Returns `false` when no oauth-style account
    /// matches. Covers Anthropic `Oauth` and `Codex` credentials alike —
    /// both rotate access/refresh tokens.
    pub fn update_oauth_tokens(
        &mut self,
        ident: &str,
        access_token: &str,
        refresh_token: Option<&str>,
        expires_at_ms: u64,
        refreshed_at_ms: u64,
    ) -> bool {
        let idx = self
            .accounts
            .iter()
            .position(|a| a.credential.account_uuid() == Some(ident))
            .or_else(|| self.accounts.iter().position(|a| a.name == ident));
        let Some(idx) = idx else {
            return false;
        };
        match &mut self.accounts[idx].credential {
            AccountCredential::Oauth {
                access_token: at,
                refresh_token: rt,
                expires_at_ms: exp,
                last_refresh_ms: lr,
                ..
            }
            | AccountCredential::Codex {
                access_token: at,
                refresh_token: rt,
                expires_at_ms: exp,
                last_refresh_ms: lr,
                ..
            }
            | AccountCredential::Grok {
                access_token: at,
                refresh_token: rt,
                expires_at_ms: exp,
                last_refresh_ms: lr,
                ..
            } => {
                *at = access_token.to_string();
                if let Some(new_rt) = refresh_token {
                    *rt = new_rt.to_string();
                }
                *exp = expires_at_ms;
                *lr = Some(refreshed_at_ms);
                true
            }
            // Nothing to rotate: an anthropic API key and an OpenRouter key
            // are long-lived secrets, not access/refresh pairs.
            AccountCredential::Apikey { .. } | AccountCredential::OpenRouter { .. } => false,
        }
    }
}

fn default_version() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

fn default_codex_upstream() -> String {
    DEFAULT_CODEX_UPSTREAM.to_string()
}

fn default_codex_token_url() -> String {
    DEFAULT_CODEX_TOKEN_URL.to_string()
}

/// Default codex model slug (the value `CODEX_MODEL` used to hardcode).
/// Must stay in sync with `provider::codex::CODEX_MODEL`.
fn default_codex_model() -> String {
    "gpt-5.6-sol".to_string()
}

/// Default grok chat upstream: the Grok-CLI chat proxy (subscription path,
/// CLIProxyAPI `internal/auth/xai/types.go:13`). Must stay in sync with
/// `provider::grok::GROK_CHAT_PROXY_UPSTREAM`.
fn default_grok_upstream() -> String {
    "https://cli-chat-proxy.grok.com/v1".to_string()
}

/// Default grok model slug. Must stay in sync with
/// `provider::grok::GROK_MODEL`.
fn default_grok_model() -> String {
    "grok-4.6".to_string()
}

/// Default OpenRouter base URL.
///
/// It is the host root **before** `/v1`, exactly like `upstream`
/// (`https://api.anthropic.com`) — because the proxy appends the CLIENT's
/// verbatim path (`/v1/messages`) to it, so the wire URL composes to
/// `https://openrouter.ai/api/v1/messages`, the real Anthropic-compatible
/// endpoint.
///
/// Getting this wrong is silent and total: with `…/api/v1` here the request
/// goes to `…/api/v1/v1/messages`, which live-probes **404** while the correct
/// URL probes 401 (2026-08-21). `openrouter_upstream_composes_the_real_endpoint`
/// in `provider::openrouter` pins the composition.
fn default_openrouter_upstream() -> String {
    "https://openrouter.ai/api".to_string()
}

/// Default free-model pin the bare `or` alias resolves to. Must stay in sync
/// with `provider::openrouter::OPENROUTER_DEFAULT_MODEL`.
fn default_openrouter_model() -> String {
    crate::catalog::OPENROUTER_DEFAULT_PIN.to_string()
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

/// Default forward-path idle timeout: 120 seconds of upstream silence
/// (post-connect) before the stream is aborted. The connect phase is covered
/// separately by the client's 10s `connect_timeout`.
fn default_forward_idle_timeout_secs() -> u64 {
    120
}

/// Default ingress request-body admission cap (64 MiB). Kept in sync with
/// [`DEFAULT_MAX_REQUEST_BYTES`] so a config that omits the field caps exactly
/// where the const-defined backstop does.
fn default_max_request_bytes() -> usize {
    DEFAULT_MAX_REQUEST_BYTES
}

fn default_upstream() -> String {
    DEFAULT_UPSTREAM.to_string()
}

pub fn default_fable_weekly_max() -> f64 {
    0.98
}

fn default_five_hour_max() -> f64 {
    0.90
}

fn default_seven_day_max() -> f64 {
    0.99
}

fn default_usage_poll_secs() -> u64 {
    300
}

fn default_usage_max_age_secs() -> u64 {
    600
}

fn default_refresh_ahead_secs() -> u64 {
    7 * 3600
}

/// Default per-account idle-probe cooldown: 15 min — one probe per tick of
/// the default sweep, so cold gauges refresh continuously (Z 2026-07-15)
/// while cost stays bounded at four 1-token probes per account per hour.
fn default_idle_probe_cooldown_secs() -> u64 {
    900
}

/// Default idle-probe timer-sweep cadence: 15 min (issue #45's always-on
/// mandate, tightened for the staleness re-probe) — one tick per default
/// `per_account_cooldown_secs`, so the steady state is at most one 1-token
/// probe per cold account per tick.
fn default_idle_probe_sweep_secs() -> u64 {
    900
}

/// Default idle-probe staleness horizon: 15 min. An account whose freshest
/// window observation is older than this is probed again on the next
/// sweep/on-demand trigger, so cold subscriptions keep live gauges.
fn default_idle_probe_stale_after_secs() -> u64 {
    900
}

/// Default raw-io retention window: 90 days (per Feature B).
fn default_raw_io_retention_days() -> u64 {
    90
}

/// Default raw-io per-body capture cap: 8 MiB
/// ([`crate::proxy::raw_io::RESPONSE_CAP_BYTES`]). Kept in sync with that
/// constant so a config that omits the field caps exactly where the code's
/// backstop does.
fn default_raw_io_max_body_bytes() -> usize {
    crate::proxy::raw_io::RESPONSE_CAP_BYTES
}

fn default_routing_group() -> String {
    "claude".to_string()
}

fn default_on_empty_group() -> String {
    "error".to_string()
}
