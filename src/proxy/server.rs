//! axum listener + routing: `/llmux/status`, raw `/v1/oauth/token`
//! relay, and a catch-all that forwards everything else upstream (FR1).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Query, State};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use axum::Router;
use http::{header, HeaderValue, StatusCode};

use super::idle_probe::ReqwestProber;
use super::logging::RequestLogger;
use super::{forward, ProxyError};
use crate::auth::oauth::RefreshCoalescer;
use crate::config::{AccountConfig, AccountCredential, Config};
use crate::dashboard::{self, DashboardHub};
use crate::logging::LogLine;
use crate::provider::anthropic::AnthropicPassthrough;
use crate::scheduler::idle_probe::IdleProber;
use crate::scheduler::select::SelectParams;
use crate::scheduler::usage::UsagePoller;
use crate::scheduler::{AccountId, AccountPool, PoolSnapshot};
use crate::tui::{ActivityEvent, ACTIVITY_CHANNEL_CAP};

/// Periodic scheduler re-evaluation (FR3: selection runs on a tick, never
/// per-request). Public so the TUI can show a next-evaluation countdown.
pub const EVALUATE_TICK: Duration = Duration::from_secs(60);

/// Background token-refresh cadence. Each tick refreshes every healthy
/// oauth account whose remaining token lifetime is under
/// `scheduler.refresh_ahead_secs` — so tokens stay fresh with ZERO client
/// traffic (the request-time 5-minute proactive refresh in `forward` stays
/// as defense in depth). The first tick fires immediately at startup.
const REFRESH_TICK: Duration = Duration::from_secs(600);

/// Per-account relayed-traffic totals, owned by the proxy (the scheduler
/// pool deliberately tracks quota windows only; src/scheduler is untouched).
#[derive(Debug, Default)]
pub struct UsageTotals {
    inner: Mutex<HashMap<String, AccountTotals>>,
}

/// Lifetime counters for one account (since proxy start).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AccountTotals {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl UsageTotals {
    pub fn record(
        &self,
        account: &AccountId,
        requests: u64,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = inner.entry(account.0.clone()).or_default();
        entry.requests = entry.requests.saturating_add(requests);
        entry.input_tokens = entry.input_tokens.saturating_add(input_tokens);
        entry.output_tokens = entry.output_tokens.saturating_add(output_tokens);
    }

    pub fn get(&self, account: &AccountId) -> AccountTotals {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&account.0)
            .copied()
            .unwrap_or_default()
    }
}

/// Live (no-restart) values of the runtime-tunable settings, following the
/// `email_anonymous`/`round_robin` holder convention: seeded from config at
/// boot, flipped by `POST /llmux/settings` (persist-then-apply), read by
/// their consumers each use — `AppState.config` is a per-clone snapshot, so
/// shared mutable state must ride an `Arc`. Ceilings are stored as f64 bits
/// in `AtomicU64`.
#[derive(Debug)]
pub struct SettingsLive {
    pub tui_effects: AtomicBool,
    pub show_fable_weekly: AtomicBool,
    /// `true` = quota gauges fill with REMAINING (config `quota_display`).
    pub quota_remaining: AtomicBool,
    /// Model→group routing gate (config `routing.enabled`) — read by the
    /// forward path per request.
    pub routing_enabled: AtomicBool,
    /// Raw-io capture gate (config `raw_io.enabled`) — read per request.
    pub raw_io_enabled: AtomicBool,
    pub five_hour_max: AtomicU64,
    pub seven_day_max: AtomicU64,
    pub fable_weekly_max: AtomicU64,
    pub usage_max_age_secs: AtomicU64,
}

impl SettingsLive {
    fn from_config(config: &Config) -> Self {
        Self {
            tui_effects: AtomicBool::new(config.tui_effects),
            show_fable_weekly: AtomicBool::new(config.show_fable_weekly),
            quota_remaining: AtomicBool::new(
                config.quota_display == crate::config::QuotaDisplay::Remaining,
            ),
            routing_enabled: AtomicBool::new(config.routing.enabled),
            raw_io_enabled: AtomicBool::new(config.raw_io.enabled),
            five_hour_max: AtomicU64::new(config.scheduler.five_hour_max.to_bits()),
            seven_day_max: AtomicU64::new(config.scheduler.seven_day_max.to_bits()),
            fable_weekly_max: AtomicU64::new(config.scheduler.fable_weekly_max.to_bits()),
            usage_max_age_secs: AtomicU64::new(config.scheduler.usage_max_age_secs),
        }
    }

    pub fn quota_display(&self) -> crate::config::QuotaDisplay {
        if self.quota_remaining.load(Ordering::Relaxed) {
            crate::config::QuotaDisplay::Remaining
        } else {
            crate::config::QuotaDisplay::Used
        }
    }
}

/// Shared per-request state. Cloning is cheap (`Arc` inside the pool,
/// `reqwest::Client` is internally reference-counted).
#[derive(Clone)]
pub struct AppState {
    pub pool: AccountPool,
    pub client: reqwest::Client,
    pub config: Config,
    /// `None` when request logging is disabled.
    pub logger: Option<Arc<RequestLogger>>,
    /// The Anthropic passthrough provider (byte-identity fast path), held
    /// concretely (the trait's async `auth` is not dyn-compatible). Provider
    /// choice is per-account-credential at forward time: codex credentials
    /// route through [`Self::codex`], everything else through this.
    pub provider: Arc<AnthropicPassthrough>,
    /// The OpenAI Codex provider (Responses API translation) for
    /// `type: "codex"` accounts. Holds the per-process session id.
    pub codex: Arc<crate::provider::codex::CodexProvider>,
    /// xAI grok provider (docs/grok/spec.md): live-mutable shape behind
    /// `POST /llmux/grok`, same contract as `codex`.
    pub grok: Arc<crate::provider::grok::GrokProvider>,
    /// OpenRouter provider (docs/openrouter/spec.md). A PASSTHROUGH like
    /// [`Self::provider`] — not a translator — differing only in base URL,
    /// credential shape, and the `or-…` → upstream-slug `model` rewrite.
    pub openrouter: Arc<crate::provider::openrouter::OpenRouterProvider>,
    /// On-demand idle-account usage probe (issue #21). Fires at most one
    /// gated `max_tokens = 1` ping for a windowless account so the scheduler's
    /// ranking/display has real 5h/7d data. Enabled by default (#45;
    /// `config.proxy.idle_probe.enabled`), with the background timer sweep in
    /// [`serve`] driving it; cooldown-gated per account so triggers never burst.
    pub idle_prober: Arc<IdleProber<ReqwestProber>>,
    /// Model→backend-group classifier, built from `config.routing`. When
    /// routing is disabled it is the builtin classifier and is never consulted
    /// for routing (forward passes `group = None`); it is still held so the
    /// status/eval paths can ask whether routing is on.
    pub classifier: Arc<crate::routing::Classifier>,
    /// Coalesces concurrent OAuth refreshes per account.
    pub refresher: Arc<RefreshCoalescer>,
    /// Per-account relayed-traffic totals for `/llmux/status`.
    pub totals: Arc<UsageTotals>,
    /// Where refreshed tokens are persisted (read-merge-write). `None`
    /// disables persistence (tests).
    pub config_path: Option<PathBuf>,
    /// Where finished requests are appended + replayed from on startup
    /// (req-persist A/C: stats survive restart, activity records kept with no
    /// retention). `None` disables activity persistence (unit tests; or no
    /// resolvable state dir). Defaults in [`Self::new`] to the state-dir
    /// `activity.jsonl`; e2e/integration callers override it to a tempdir so a
    /// driven request never touches the user's real log — same pattern as
    /// `config_path`.
    pub activity_log_path: Option<PathBuf>,
    /// Where raw request/response payloads are appended (Feature B) + pruned to
    /// `config.raw_io.retention_days` on startup. DISTINCT from
    /// [`Self::activity_log_path`] (which holds per-request metadata): this holds
    /// the actual payload bytes. `None` disables capture (unit tests; or no
    /// resolvable state dir). Defaults in [`Self::new`] to the state-dir
    /// `raw-io.jsonl`; e2e/integration callers override it to a tempdir so a
    /// driven request never touches the user's real log — same pattern as
    /// `activity_log_path`.
    pub raw_io_path: Option<PathBuf>,
    /// Activity feed emit side. The proxy / poller / refresher `try_send` and
    /// drop on full — best-effort observability, never backpressure (see
    /// `tui::event`). The matching receiver is folded into [`Self::hub`] by
    /// the `dashboard::fold` task `serve` spawns.
    pub events: Option<tokio::sync::mpsc::Sender<ActivityEvent>>,
    /// Server-owned dashboard fold (activity ring, totals, last switch,
    /// poller health, log console). The local TUI renders it directly; the
    /// `GET /llmux/dashboard` endpoint serializes it.
    pub hub: Arc<DashboardHub>,
    /// Activity-event receiver, taken by `serve` to spawn the fold task.
    /// `Mutex<Option<_>>` so `AppState` stays `Clone` (the receiver is a
    /// single-consumer resource — only the first `serve` takes it).
    pending_events: Arc<Mutex<Option<tokio::sync::mpsc::Receiver<ActivityEvent>>>>,
    /// Tracing-bridge receiver (TUI mode only — the `RUST_LOG` channel feed
    /// into the hub's log console). `None` in plain/daemon mode, where the
    /// fold re-traces activity events so `server.log` keeps the history.
    pending_logs: Arc<Mutex<Option<tokio::sync::mpsc::Receiver<LogLine>>>>,
    /// Per-process request id source for activity-event correlation.
    pub request_counter: Arc<AtomicU64>,
    /// Server start, for `/llmux/status` uptime.
    pub started: Instant,
    /// Actually bound port (config port until `serve` binds; the OS-assigned
    /// port afterwards — matters for `proxy.port = 0` test servers).
    pub bound_port: Arc<AtomicU16>,
    /// LIVE value of the `email_anonymous` display setting: seeded from
    /// `config.email_anonymous`, flipped at runtime by `POST /llmux/settings`
    /// (which also persists it read-merge-write), read by `/llmux/status`,
    /// the dashboard document, and the local TUI each frame — so a flip
    /// reflects everywhere without a restart. Atomic (not `config`) because
    /// `AppState.config` is a per-clone snapshot; shared mutable state must
    /// ride an `Arc`.
    pub email_anonymous: Arc<AtomicBool>,
    /// Live runtime-tunable settings (see [`SettingsLive`]).
    pub settings_live: Arc<SettingsLive>,
    /// Live scheduler mode (config `scheduler.mode`), same atomic convention
    /// as `email_anonymous`: `false` = default, `true` = round-robin. A TUI
    /// `S` / `POST /llmux/scheduler-mode` flip reflects in the very next
    /// `select_params()` without restart.
    pub round_robin: Arc<AtomicBool>,
    /// LIVE value of the top-of-dashboard event banners (config `events`), same
    /// live-holder convention as `email_anonymous`/`round_robin`: seeded from
    /// `config.events` at boot, updated by `POST /llmux/events` (via
    /// [`Self::upsert_event`] / [`Self::remove_event`]) after the persist, and
    /// read into the dashboard document each frame/poll — so a running daemon
    /// serves the new banner immediately, in BOTH TUI backends. `RwLock` (not an
    /// atomic) because the value is a `Vec`; the lock is only ever held for a
    /// short clone/store, never across an await.
    pub event_banners: Arc<RwLock<Vec<crate::config::EventBanner>>>,
    /// LIVE downstream client-key registry (multi-tenant keys, #22): the auth
    /// gate resolves every request against THIS, not the boot-time `config`
    /// snapshot, so issue/suspend/resume/rotate/revoke bite without a restart.
    /// Mutations persist to disk first, then [`crate::proxy::keys::KeyRegistry::reload`]
    /// swaps the snapshot (persist-then-swap).
    pub keys: Arc<crate::proxy::keys::KeyRegistry>,
    /// Serializes client-key mutations end-to-end (read-merge-write on disk +
    /// registry swap) so concurrent admin calls can't lose updates.
    key_mutation: Arc<Mutex<()>>,
    /// Graceful-shutdown trigger fired by `POST /llmux/shutdown`.
    pub shutdown: Arc<tokio::sync::Notify>,
    /// GUI-initiated OAuth login registry (FR4, `.prd/11-llmux-islands-spec.md`):
    /// the single in-flight browser login the daemon runs on behalf of an HTTP
    /// client (`llmux-islands`), behind `/llmux/login/{start,status,cancel}`.
    pub logins: Arc<super::login::LoginRegistry>,
}

impl AppState {
    /// Build the shared state. The activity-event channel is created here:
    /// the emit `Sender` lands in [`Self::events`] and the matching `Receiver`
    /// is parked in [`Self::pending_events`] for `serve` to fold into the hub.
    /// `logs_rx` is the optional tracing-bridge feed (TUI mode); its absence
    /// is what tells the fold to re-trace activity events into `server.log`
    /// (daemon parity).
    pub fn new(
        config: Config,
        pool: AccountPool,
        logger: Option<Arc<RequestLogger>>,
        logs_rx: Option<tokio::sync::mpsc::Receiver<LogLine>>,
    ) -> Result<Self, ProxyError> {
        let (events_tx, events_rx) = tokio::sync::mpsc::channel(ACTIVITY_CHANNEL_CAP);
        // Operator pause set from the loaded config — applied before the pool
        // serves its first selection so a paused account never gets picked at
        // boot (post-boot changes flow through `apply_roster`).
        pool.apply_paused(&config.paused_accounts);
        pool.apply_limits(&config.account_limits);
        // `connect_timeout` bounds the connect phase; `read_timeout` bounds
        // post-connect silence — a silent upstream that connects then stalls
        // would otherwise hang the session and pin the account. This is an
        // inactivity ceiling (it resets on every received byte), not a total
        // deadline, so long legitimate LLM streams are unaffected. Defense in
        // depth with the per-chunk timeout in `sse::passthrough_body`.
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(config.proxy.forward_idle_timeout_secs))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let provider = Arc::new(AnthropicPassthrough::new(config.upstream.clone()));
        let codex = Arc::new(crate::provider::codex::CodexProvider::with_shape(
            config.codex.upstream.clone(),
            crate::provider::codex::CodexShape::from_config(&config.codex),
        ));
        let grok = Arc::new(crate::provider::grok::GrokProvider::with_shape(
            config.grok.upstream.clone(),
            crate::provider::grok::GrokShape::from_config(&config.grok),
        ));
        let openrouter = Arc::new(crate::provider::openrouter::OpenRouterProvider::new(
            config.openrouter.upstream.clone(),
            config.openrouter.default_model.clone(),
        ));
        // On-demand idle probe (issue #21): reuses the same client + provider
        // hooks the forward path uses to send one gated `max_tokens = 1` ping.
        let idle_prober = Arc::new(IdleProber::new(
            pool.clone(),
            ReqwestProber::new(
                client.clone(),
                config.upstream.clone(),
                codex.clone(),
                config.codex.default_model.clone(),
            ),
            config.proxy.idle_probe,
        ));
        // The classifier is built from config.routing whether or not routing
        // is enabled (it is simply not consulted on the forward path while
        // disabled — forward passes group = None).
        let classifier = Arc::new(crate::routing::Classifier::from_config(
            &config.routing.claude_models,
            &config.routing.codex_models,
            &config.routing.grok_models,
            &config.routing.openrouter_models,
            &config.routing.default_group,
        ));
        // A non-default upstream (staging, e2e mock) must also receive the
        // proxy's OWN token refreshes — otherwise refresh traffic would leak
        // to the production endpoint while everything else is redirected.
        let refresher = if config.upstream == crate::config::schema::DEFAULT_UPSTREAM {
            RefreshCoalescer::new()
        } else {
            RefreshCoalescer::with_token_url(format!(
                "{}/v1/oauth/token",
                config.upstream.trim_end_matches('/')
            ))
        };
        Ok(Self {
            pool,
            client,
            logger,
            provider,
            codex,
            grok,
            openrouter,
            idle_prober,
            classifier,
            refresher: Arc::new(refresher),
            totals: Arc::new(UsageTotals::default()),
            config_path: crate::config::config_path().ok(),
            activity_log_path: crate::cli::daemon::activity_log_path(),
            raw_io_path: crate::cli::daemon::raw_io_path(),
            bound_port: Arc::new(AtomicU16::new(config.proxy.port)),
            email_anonymous: Arc::new(AtomicBool::new(config.email_anonymous)),
            settings_live: Arc::new(SettingsLive::from_config(&config)),
            round_robin: Arc::new(AtomicBool::new(
                config.scheduler.mode == crate::config::SchedulerMode::RoundRobin,
            )),
            event_banners: Arc::new(RwLock::new(config.events.clone())),
            keys: Arc::new(crate::proxy::keys::KeyRegistry::from_config(&config)),
            key_mutation: Arc::new(Mutex::new(())),
            config,
            events: Some(events_tx),
            hub: Arc::new(DashboardHub::default()),
            pending_events: Arc::new(Mutex::new(Some(events_rx))),
            pending_logs: Arc::new(Mutex::new(logs_rx)),
            request_counter: Arc::new(AtomicU64::new(0)),
            started: Instant::now(),
            shutdown: Arc::new(tokio::sync::Notify::new()),
            logins: Arc::new(super::login::LoginRegistry::default()),
        })
    }

    pub fn select_params(&self) -> SelectParams {
        let mut params = SelectParams::from(&self.config.scheduler);
        // The live atomics win over the boot-time config snapshot.
        params.mode = if self.round_robin.load(Ordering::Relaxed) {
            crate::config::SchedulerMode::RoundRobin
        } else {
            crate::config::SchedulerMode::Default
        };
        let live = &self.settings_live;
        params.five_hour_max = f64::from_bits(live.five_hour_max.load(Ordering::Relaxed));
        params.seven_day_max = f64::from_bits(live.seven_day_max.load(Ordering::Relaxed));
        params.fable_weekly_max = f64::from_bits(live.fable_weekly_max.load(Ordering::Relaxed));
        params.usage_max_age = Duration::from_secs(live.usage_max_age_secs.load(Ordering::Relaxed));
        params
    }

    /// Flip the scheduler mode live + persist it (config `scheduler.mode`,
    /// read-merge-write). Returns the newly effective mode.
    pub fn set_scheduler_mode(
        &self,
        mode: crate::config::SchedulerMode,
    ) -> Result<crate::config::SchedulerMode, crate::config::ConfigError> {
        // Persist FIRST (config-editor contract): a failed write must leave
        // the live scheduler untouched — never let disk and runtime diverge.
        // No config path = nothing to persist → refuse rather than diverge.
        let Some(path) = &self.config_path else {
            return Err(crate::config::ConfigError::PersistenceUnavailable);
        };
        crate::config::update_path(path, |c| c.scheduler.mode = mode)?;
        self.round_robin.store(
            mode == crate::config::SchedulerMode::RoundRobin,
            Ordering::Relaxed,
        );
        tracing::info!(mode = mode.label(), "scheduler mode updated");
        Ok(mode)
    }

    /// On-demand idle-account probe trigger (issue #21). For every account in
    /// `group` (or all accounts on the legacy `None` path) whose window data
    /// needs a probe — none at all, or every window stale past
    /// `stale_after_secs` (Z 2026-07-15 cold-refresh) — spawn a single gated
    /// `max_tokens = 1` probe so the next ranking/display has real data. Each
    /// spawn is fully gated inside `IdleProber::probe_if_idle` (kill-switch +
    /// per-account cooldown + needs-window re-check), so this is safe to call
    /// on every forwarded request: a no-op when probing is disabled, and at
    /// most one send per account per cooldown otherwise. Spawned, never
    /// awaited — the probe must not add latency to the request that triggered
    /// it.
    pub fn trigger_idle_probes(&self, group: Option<crate::routing::BackendGroup>) {
        if !self.config.proxy.idle_probe.enabled {
            return;
        }
        let now = SystemTime::now();
        let stale_after =
            std::time::Duration::from_secs(self.config.proxy.idle_probe.stale_after_secs);
        let snapshot = self.pool.snapshot();
        for account in snapshot.accounts.iter().filter(|a| {
            group.is_none_or(|g| a.group == g)
                && crate::scheduler::idle_probe::windows_need_probe(
                    a.five_hour.as_ref(),
                    a.seven_day.as_ref(),
                    now,
                    stale_after,
                )
        }) {
            let prober = self.idle_prober.clone();
            let id = account.id.clone();
            tokio::spawn(async move {
                prober.probe_if_idle(&id, SystemTime::now()).await;
            });
        }
    }

    /// Next activity-event correlation id (never leaves this process). 1-based
    /// to match [`RequestLogger::next_request_id`]: the first request is id 1,
    /// so the codex trace, the request log, and the dashboard feed all show the
    /// same ascending ids. A bare `fetch_add` would return 0 for the first
    /// request, which then surfaced as `"id":0` on every trace line in a
    /// single-request session.
    pub fn next_request_id(&self) -> u64 {
        self.request_counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Emit an activity event: `try_send`, dropped on a full channel.
    pub fn emit(&self, event: ActivityEvent) {
        if let Some(events) = &self.events {
            let _ = events.try_send(event);
        }
    }

    /// Add (upsert) an API-key account: read-merge-write the config, then swap
    /// the merged roster into the live pool and re-select so the running daemon
    /// serves it with no restart. The SINGLE in-process implementation behind
    /// both the local TUI `a`-key path and the `POST /llmux/add-account`
    /// endpoint. `name = None` assigns the next `api-N` on the FRESH on-disk
    /// state. The api key is never logged. Returns `(resolved_name, outcome)`.
    pub fn add_apikey_account(
        &self,
        name: Option<&str>,
        api_key: &str,
    ) -> Result<(String, crate::config::Upsert), crate::config::ConfigError> {
        let Some(path) = &self.config_path else {
            return Err(crate::config::ConfigError::NoConfigDir);
        };
        let mut resolved = String::new();
        let mut outcome = crate::config::Upsert::Added;
        let merged = crate::config::update_path(path, |c| {
            resolved = match name {
                Some(n) => n.to_string(),
                None => {
                    let next = c
                        .accounts
                        .iter()
                        .filter(|a| a.name.starts_with("api-"))
                        .count()
                        + 1;
                    format!("api-{next}")
                }
            };
            outcome = c.upsert_account(AccountConfig {
                name: resolved.clone(),
                credential: AccountCredential::Apikey {
                    api_key: api_key.to_string(),
                },
            });
        })?;
        self.apply_roster(&merged);
        // Names are not credentials; the api key never reaches a log line.
        tracing::info!(account = %resolved, action = ?outcome, "account added");
        Ok((resolved, outcome))
    }

    /// Remove an account by name: read-merge-write removal, then reload the
    /// live pool. The SINGLE in-process implementation behind both the local
    /// TUI `r`-key path and the `POST /llmux/remove-account` endpoint. Returns
    /// `Ok(true)` when an account was removed, `Ok(false)` when none matched.
    /// Set / clear the operator pause on one account (config
    /// `paused_accounts` — read-merge-write, then roster re-apply so the live
    /// pool honors it immediately, no restart). `Ok(false)` when no account
    /// with that name exists. A paused CURRENT account is moved off
    /// cooperatively by the next evaluate tick.
    pub fn set_account_paused(
        &self,
        name: &str,
        paused: bool,
    ) -> Result<bool, crate::config::ConfigError> {
        let Some(path) = &self.config_path else {
            return Err(crate::config::ConfigError::NoConfigDir);
        };
        let mut known = false;
        let merged = crate::config::update_path(path, |c| {
            known = c.accounts.iter().any(|a| a.name == name);
            if known {
                if paused {
                    c.paused_accounts.insert(name.to_string());
                } else {
                    c.paused_accounts.remove(name);
                }
            }
        })?;
        if known {
            self.apply_roster(&merged);
            tracing::info!(account = %name, paused, "account pause updated");
        }
        Ok(known)
    }

    /// Serialize one client-key mutation: load the on-disk config, apply
    /// `mutate`, and only if it succeeds write the file back and swap the
    /// live registry snapshot (persist-then-swap; multi-tenant #22 MUST-FIX
    /// 1). A domain error aborts BEFORE the write, so the file really is
    /// untouched on failure. The whole sequence runs under the key-mutation
    /// lock so concurrent admin calls can't lose updates.
    fn mutate_client_keys<T, F>(&self, mutate: F) -> Result<T, KeyAdminError>
    where
        F: FnOnce(&mut Config) -> Result<T, KeyAdminError>,
    {
        let Some(path) = &self.config_path else {
            return Err(KeyAdminError::Config(
                crate::config::ConfigError::NoConfigDir,
            ));
        };
        let _guard = self.key_mutation.lock().unwrap_or_else(|e| e.into_inner());
        let mut config = crate::config::load_path(path).map_err(KeyAdminError::Config)?;
        let value = mutate(&mut config)?;
        crate::config::save_path(path, &config).map_err(KeyAdminError::Config)?;
        self.keys.reload(&config);
        Ok(value)
    }

    /// Count of ACTIVE admin credentials in `config`: the legacy shared
    /// `proxy.api_key` (full capability, counts as one) plus non-suspended,
    /// non-revoked admin client keys.
    fn active_admin_credentials(config: &Config) -> usize {
        let legacy = usize::from(config.proxy.api_key.is_some());
        legacy
            + config
                .client_keys
                .iter()
                .filter(|k| {
                    matches!(k.kind, crate::config::ClientKeyKind::Admin)
                        && !k.suspended
                        && k.revoked_at_ms.is_none()
                })
                .count()
    }

    /// Issue a new downstream client key. Returns the stored row plus the
    /// plaintext secret — the ONLY surface that ever returns it.
    pub fn issue_client_key(
        &self,
        name: &str,
        email: Option<String>,
        kind: crate::config::ClientKeyKind,
    ) -> Result<(crate::config::ClientKey, String), KeyAdminError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(KeyAdminError::NameRequired);
        }
        let issued = crate::config::generate_client_key();
        let name = name.to_string();
        let prefix = issued.prefix.clone();
        let digest = issued.digest.clone();
        let stored = self.mutate_client_keys(move |c| {
            // Names must stay unique among non-revoked keys so the CLI can
            // resolve them unambiguously (mutations themselves are id-only).
            if c.client_keys
                .iter()
                .any(|k| k.revoked_at_ms.is_none() && k.name == name)
            {
                return Err(KeyAdminError::NameTaken(name));
            }
            // Attribution ids must be unique FOREVER (revoked rows included —
            // history joins on them), so regenerate on the astronomically
            // rare collision instead of silently merging two tenants.
            let mut id = crate::config::generate_client_key_id();
            while c.client_keys.iter().any(|k| k.id == id) {
                id = crate::config::generate_client_key_id();
            }
            let row = crate::config::ClientKey {
                id,
                name,
                email: email.filter(|e| !e.trim().is_empty()),
                kind,
                key_prefix: prefix,
                key_digest: digest,
                suspended: false,
                created_at_ms: now_epoch_ms(),
                revoked_at_ms: None,
            };
            c.client_keys.push(row.clone());
            Ok(row)
        })?;
        tracing::info!(id = %stored.id, name = %stored.name, "client key issued");
        Ok((stored, issued.secret))
    }

    /// Suspend / resume an issued key. Refuses to suspend the last active
    /// admin credential (self-lockout / fail-open guard).
    pub fn set_client_key_suspended(
        &self,
        id: &str,
        suspended: bool,
    ) -> Result<crate::config::ClientKey, KeyAdminError> {
        let id = id.to_string();
        let key = self.mutate_client_keys(move |c| {
            let i = c
                .client_keys
                .iter()
                .position(|k| k.id == id && k.revoked_at_ms.is_none())
                .ok_or(KeyAdminError::NotFound)?;
            let is_active_admin =
                matches!(c.client_keys[i].kind, crate::config::ClientKeyKind::Admin)
                    && !c.client_keys[i].suspended;
            if suspended && is_active_admin && Self::active_admin_credentials(c) <= 1 {
                return Err(KeyAdminError::LastAdmin);
            }
            c.client_keys[i].suspended = suspended;
            Ok(c.client_keys[i].clone())
        })?;
        tracing::info!(id = %key.id, name = %key.name, suspended, "client key suspend updated");
        Ok(key)
    }

    /// Soft-revoke an issued key: authentication stops immediately, but the
    /// row (id/name/email) is preserved forever so historical usage keeps its
    /// attribution. Refuses to revoke the last active admin credential.
    pub fn revoke_client_key(&self, id: &str) -> Result<crate::config::ClientKey, KeyAdminError> {
        let id = id.to_string();
        let key = self.mutate_client_keys(move |c| {
            let i = c
                .client_keys
                .iter()
                .position(|k| k.id == id && k.revoked_at_ms.is_none())
                .ok_or(KeyAdminError::NotFound)?;
            let is_active_admin =
                matches!(c.client_keys[i].kind, crate::config::ClientKeyKind::Admin)
                    && !c.client_keys[i].suspended;
            if is_active_admin && Self::active_admin_credentials(c) <= 1 {
                return Err(KeyAdminError::LastAdmin);
            }
            c.client_keys[i].revoked_at_ms = Some(now_epoch_ms());
            Ok(c.client_keys[i].clone())
        })?;
        tracing::info!(id = %key.id, name = %key.name, "client key revoked");
        Ok(key)
    }

    /// Rotate an issued key: a NEW secret under the SAME attribution id, so
    /// usage continuity is a property of the API, not a promise. Returns the
    /// updated row plus the new plaintext (shown once).
    pub fn rotate_client_key(
        &self,
        id: &str,
    ) -> Result<(crate::config::ClientKey, String), KeyAdminError> {
        let id = id.to_string();
        let issued = crate::config::generate_client_key();
        let prefix = issued.prefix.clone();
        let digest = issued.digest.clone();
        let key = self.mutate_client_keys(move |c| {
            let i = c
                .client_keys
                .iter()
                .position(|k| k.id == id && k.revoked_at_ms.is_none())
                .ok_or(KeyAdminError::NotFound)?;
            c.client_keys[i].key_prefix = prefix;
            c.client_keys[i].key_digest = digest;
            Ok(c.client_keys[i].clone())
        })?;
        tracing::info!(id = %key.id, name = %key.name, "client key rotated");
        Ok((key, issued.secret))
    }

    /// Set / clear the per-account ceiling overrides (config `account_limits`)
    /// — read-merge-write + live roster re-apply, like
    /// [`Self::set_account_paused`]. An all-`None` limits value removes the
    /// entry (global ceilings apply again). `Ok(false)` = unknown account.
    pub fn set_account_limits(
        &self,
        name: &str,
        limits: crate::config::AccountLimits,
    ) -> Result<bool, crate::config::ConfigError> {
        let Some(path) = &self.config_path else {
            return Err(crate::config::ConfigError::NoConfigDir);
        };
        let mut known = false;
        let merged = crate::config::update_path(path, |c| {
            known = c.accounts.iter().any(|a| a.name == name);
            if known {
                if limits.is_empty() {
                    c.account_limits.remove(name);
                } else {
                    c.account_limits.insert(name.to_string(), limits);
                }
            }
        })?;
        if known {
            self.apply_roster(&merged);
            tracing::info!(account = %name, ?limits, "account limits updated");
        }
        Ok(known)
    }

    /// Upsert one dashboard event banner (config `events`) by `id`: an entry
    /// with the same `id` is replaced in place, a new `id` is appended.
    /// IDEMPOTENT — an identical payload persists the same list. Read-merge-write
    /// persistence, mirroring [`Self::set_scheduler_mode`]. After the persist
    /// succeeds, the live [`Self::event_banners`] holder is replaced with the
    /// merged list, so a running daemon serves the change in the very next
    /// dashboard document — in BOTH TUI backends — without a restart or re-read.
    /// Returns the persisted list (post read-merge-write) for the endpoint to
    /// echo. `NoConfigDir` when persistence is disabled (a no-op then).
    pub fn upsert_event(
        &self,
        banner: crate::config::EventBanner,
    ) -> Result<Vec<crate::config::EventBanner>, crate::config::ConfigError> {
        self.persist_events(
            |events| match events.iter_mut().find(|e| e.id == banner.id) {
                Some(existing) => *existing = banner.clone(),
                None => events.push(banner.clone()),
            },
        )
    }

    /// Remove the dashboard event banner with `id` (config `events`).
    /// IDEMPOTENT — removing an absent id persists the unchanged list. Same
    /// read-merge-write + live-holder update as [`Self::upsert_event`]; returns
    /// the persisted list for the endpoint to echo.
    pub fn remove_event(
        &self,
        id: &str,
    ) -> Result<Vec<crate::config::EventBanner>, crate::config::ConfigError> {
        self.persist_events(|events| events.retain(|e| e.id != id))
    }

    /// Shared read-merge-write for the event-banner list: apply `mutate` to
    /// `config.events`, persist, then mirror the merged list into the live
    /// holder only after the write succeeds (so a failed write never leaves the
    /// in-memory banners ahead of disk).
    fn persist_events(
        &self,
        mutate: impl Fn(&mut Vec<crate::config::EventBanner>),
    ) -> Result<Vec<crate::config::EventBanner>, crate::config::ConfigError> {
        let Some(path) = &self.config_path else {
            return Err(crate::config::ConfigError::NoConfigDir);
        };
        let merged = crate::config::update_path(path, |c| mutate(&mut c.events))?;
        *self
            .event_banners
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = merged.events.clone();
        tracing::info!(count = merged.events.len(), "event banners updated");
        Ok(merged.events)
    }

    pub fn remove_account(&self, name: &str) -> Result<bool, crate::config::ConfigError> {
        let Some(path) = &self.config_path else {
            return Err(crate::config::ConfigError::NoConfigDir);
        };
        let mut removed = false;
        let merged = crate::config::update_path(path, |c| {
            removed = c.remove_account(name);
        })?;
        if removed {
            self.apply_roster(&merged);
            tracing::info!(account = %name, "account removed");
        }
        Ok(removed)
    }

    /// Inject (upsert) a fully-formed OAuth/Codex account: read-merge-write
    /// the config, then swap the merged roster into the live pool so the
    /// running daemon serves it with no restart. The SINGLE in-process
    /// implementation behind both the local TUI `n`-key path (login runs in
    /// the client, the resulting credential is injected in-process) and the
    /// `POST /llmux/inject-account` endpoint (attach mode: the client relays
    /// the credential it minted to the daemon). Dedup is by `account_uuid` /
    /// `account_id` then `name` (see [`Config::upsert_account`]), so a
    /// re-login updates the existing entry rather than duplicating it.
    ///
    /// The caller hands over an already-built [`AccountConfig`] carrying an
    /// `Oauth` or `Codex` credential; an `Apikey` credential is rejected (use
    /// [`Self::add_apikey_account`] for those). No token is ever logged.
    /// Returns `(resolved_name, outcome)`.
    pub fn inject_account(
        &self,
        account: AccountConfig,
    ) -> Result<(String, crate::config::Upsert), crate::config::ConfigError> {
        if matches!(account.credential, AccountCredential::Apikey { .. }) {
            return Err(crate::config::ConfigError::Invalid(
                "inject_account accepts only oauth/codex credentials".into(),
            ));
        }
        let Some(path) = &self.config_path else {
            return Err(crate::config::ConfigError::NoConfigDir);
        };
        let name = account.name.clone();
        let kind = account.credential.kind();
        let mut outcome = crate::config::Upsert::Added;
        let merged = crate::config::update_path(path, |c| {
            outcome = c.upsert_account(account.clone());
        })?;
        self.apply_roster(&merged);
        // Names/kinds are not credentials; no token reaches a log line.
        tracing::info!(account = %name, kind, action = ?outcome, "account injected");
        Ok((name, outcome))
    }

    /// Swap a freshly-merged config's roster into the live pool and re-select
    /// every backend group (a removed `current` is cleared by
    /// `reload_accounts`; the re-eval picks a replacement). Shared tail of
    /// [`Self::add_apikey_account`] / [`Self::remove_account`].
    fn apply_roster(&self, merged: &Config) {
        self.pool.reload_accounts(&merged.accounts);
        self.pool.apply_paused(&merged.paused_accounts);
        self.pool.apply_limits(&merged.account_limits);
        let params = self.select_params();
        let now = SystemTime::now();
        for group in eval_groups(&self.pool, self.config.routing.enabled) {
            self.pool.evaluate(group, &params, now);
        }
    }
}

/// Run the proxy until shutdown: bind `config.proxy.port`, spawn the usage
/// poller and the re-evaluation tick next to the listener, serve [`router`].
///
/// Binds all interfaces (teamclaude parity — the proxy api key with
/// loopback exemption exists precisely for non-local peers).
pub async fn run(
    config: Config,
    pool: AccountPool,
    log_dir: Option<PathBuf>,
    logs_rx: Option<tokio::sync::mpsc::Receiver<LogLine>>,
) -> Result<(), ProxyError> {
    let logger = match log_dir {
        Some(dir) => {
            let logger = RequestLogger::new(dir.clone())?;
            tracing::info!(dir = %dir.display(), "request logging enabled");
            Some(Arc::new(logger))
        }
        None => None,
    };
    let state = AppState::new(config, pool, logger, logs_rx)?;
    serve(state, None).await
}

/// [`run`] over a pre-built [`AppState`]: prime usage state, run the initial
/// selection, spawn the poller + evaluation tick, bind, and serve.
///
/// `ready` (when given) receives the actual bound address once listening —
/// the seam for `proxy.port = 0` callers (e2e tests) that need the
/// OS-assigned port.
pub async fn serve(
    state: AppState,
    ready: Option<tokio::sync::oneshot::Sender<SocketAddr>>,
) -> Result<(), ProxyError> {
    let params = state.select_params();

    // Arm activity persistence (req-persist A/C) — append target only, NO file
    // read. Done once here — the single production serve chokepoint — before
    // the fold task starts appending new finished requests. The returned cut
    // (the log's byte length right now, before any live append) bounds the
    // post-readiness hydration below so a request that finishes while history
    // is still loading is never double-counted. Resuming the cumulative stats
    // from the (possibly large) log is deliberately NOT done here: it moved to
    // a background task spawned after the listener is ready, so a big history
    // can no longer delay startup ("serve first, hydrate later").
    let hydrate_cut = state.hub.arm_persistence(state.activity_log_path.clone());

    // Dashboard fold: the single consumer of the activity-event channel (and,
    // in TUI mode, the tracing-bridge channel) into the hub. Spawned once —
    // the receiver is taken out of `pending_events`/`pending_logs`. Without a
    // bridge feed (plain/daemon mode) the fold also re-traces each activity
    // event so `server.log` keeps the request history the TUI would show.
    let fold_events = state
        .pending_events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let fold_logs = state
        .pending_logs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let fold_task = fold_events.map(|events| {
        let trace_events = fold_logs.is_none();
        tokio::spawn(dashboard::fold(
            state.hub.clone(),
            events,
            fold_logs,
            trace_events,
        ))
    });

    // Background: active usage polling (FR3) + periodic re-evaluation tick.
    // One priming pass runs BEFORE the initial selection so the very first
    // pick already ranks by real window state (soonest 7d reset) instead of
    // falling back to cold-account id order.
    let mut poller = UsagePoller::new(
        state.pool.clone(),
        state.client.clone(),
        state.config.upstream.clone(),
        state.config.scheduler,
    )
    .with_events(state.events.clone());
    poller.prime(SystemTime::now()).await;

    // Initial selection so the first request doesn't pay for it. Evaluate
    // every backend group that has at least one account (with routing
    // disabled this is just the legacy slot — `evaluate(None, ..)`).
    for group in eval_groups(&state.pool, state.config.routing.enabled) {
        state.pool.evaluate(group, &params, SystemTime::now());
    }
    // Announce each group's initial selection (req1 symmetry): claude and codex
    // both surface in the activity log, not just the representative slot.
    for current in state.pool.snapshot().current.values() {
        state.emit(ActivityEvent::AccountSwitched {
            from: None,
            to: current.0.clone(),
            reason: Some("initial selection".into()),
        });
    }

    let poller_task = tokio::spawn(poller.run());

    // Background token refresh (A2): first tick immediately, then every
    // REFRESH_TICK. Lives next to the usage poller, aborted on shutdown.
    let refresh_state = state.clone();
    let refresh_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(REFRESH_TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            background_refresh_pass(&refresh_state).await;
        }
    });

    // Background: keep ALL cold accounts warm (issue #45, generalized). A cold
    // account with NO client traffic is never probed by the request path. The
    // oauth-only usage poller covers cold *oauth* accounts, but cold Codex AND
    // cold api-key accounts have no other window source, so their 5h/7d windows
    // stay empty forever (and show no usage in `status`/dashboard/llmux-islands).
    // When the idle probe is enabled AND a positive sweep cadence is configured,
    // fire the probe trigger for EVERY backend group on a timer (`None` = all
    // groups). `trigger_idle_probes` is fully self-gated (kill-switch +
    // has-no-window + per-account cooldown inside `probe_if_idle`), so an oauth
    // account the poller already warmed is skipped (it has a window), and the
    // per-account cooldown — not this cadence — bounds cost: at most one probe
    // per account per `per_account_cooldown_secs`. Mirrors the usage poller's
    // `tick` + `sleep` loop; aborted on shutdown.
    let sweep_task = (state.config.proxy.idle_probe.enabled
        && state.config.proxy.idle_probe.sweep_secs > 0)
        .then(|| {
            let sweep_state = state.clone();
            let sweep_period = Duration::from_secs(state.config.proxy.idle_probe.sweep_secs);
            tokio::spawn(async move {
                loop {
                    sweep_state.trigger_idle_probes(None);
                    tokio::time::sleep(sweep_period).await;
                }
            })
        });

    let tick_pool = state.pool.clone();
    let tick_events = state.events.clone();
    let tick_routing_enabled = state.config.routing.enabled;
    let tick_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(EVALUATE_TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            for group in eval_groups(&tick_pool, tick_routing_enabled) {
                let slot = group.unwrap_or(crate::routing::BackendGroup::Claude);
                let before = tick_pool.snapshot().current.get(&slot).cloned();
                let decision = tick_pool.evaluate(group, &params, SystemTime::now());
                tracing::debug!(?group, ?decision, "evaluation tick");
                if let crate::scheduler::select::Decision::Switch { to } = decision {
                    if let Some(events) = &tick_events {
                        let _ = events.try_send(ActivityEvent::AccountSwitched {
                            from: before.map(|id| id.0),
                            to: to.0,
                            reason: Some("re-evaluation".into()),
                        });
                    }
                }
                // The Fable slot heals on the same cadence (issue #121):
                // `evaluate` above is NonFable-only, and before this the
                // `fable_current` slot was NEVER re-evaluated — a paused or
                // otherwise ineligible fable sticky current kept its pin
                // forever while the account-wide slot moved off within one
                // tick. Claude slot only (fable models route there); moves
                // are logged, not event-emitted: the AccountSwitched feed
                // row describes the primary current.
                if slot == crate::routing::BackendGroup::Claude {
                    let fable_decision = tick_pool.evaluate_scoped(
                        group,
                        &params,
                        SystemTime::now(),
                        crate::scheduler::select::RequestScope::Fable,
                    );
                    tracing::debug!(?group, ?fable_decision, "fable evaluation tick");
                }
            }
        }
    });

    let port = state.config.proxy.port;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(|source| ProxyError::Bind { port, source })?;
    let local_addr = listener.local_addr().map_err(ProxyError::Io)?;
    state.bound_port.store(local_addr.port(), Ordering::Relaxed);
    if let Some(ready) = ready {
        let _ = ready.send(local_addr);
    }
    tracing::info!(
        port = local_addr.port(),
        upstream = %state.config.upstream,
        accounts = state.config.accounts.len(),
        "proxy listening (ANTHROPIC_BASE_URL=http://localhost:{})",
        local_addr.port()
    );

    // Post-readiness background hydration ("serve first, hydrate later"): the
    // listener is bound and `/llmux/status` + proxy traffic are already being
    // served — only NOW resume the persisted history. Both tasks are
    // best-effort file work on the blocking pool; neither can fail or delay
    // startup, and both are crash-safe mid-flight (hydration only reads;
    // prune renames atomically at the very end).
    //
    // 1. Activity history (req-persist A/C): replay `activity.jsonl` up to the
    //    pre-bind cut into a fresh log, then merge it BEHIND live traffic
    //    (`DashboardHub::hydrate_persisted` — live rows stay in front, sums
    //    commute, no double-count past the cut). Dashboards fill in as it
    //    lands; a "history loaded" note marks completion.
    let hydrate_task = {
        let hub = state.hub.clone();
        let path = state.activity_log_path.clone();
        tokio::task::spawn_blocking(move || hub.hydrate_persisted(path.as_deref(), hydrate_cut))
    };
    // 2. Raw-io retention prune (Feature B): guarded by config (`enabled =
    //    false` skips it; `retention_days = 0` keeps everything). The scan is
    //    streaming and the commit preserves records appended while it ran (see
    //    `proxy::raw_io::prune`), so it is safe next to live traffic — and the
    //    multi-GB payload log no longer stands between restart and readiness.
    let prune_task = state.config.raw_io.enabled.then(|| {
        let path = state.raw_io_path.clone();
        let retention_days = state.config.raw_io.retention_days;
        tokio::task::spawn_blocking(move || {
            crate::proxy::raw_io::prune(
                path.as_deref(),
                retention_days,
                crate::proxy::raw_io::now_ms(),
            )
        })
    });

    let shutdown = state.shutdown.clone();
    let result = axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { shutdown.notified().await })
    .await;
    poller_task.abort();
    tick_task.abort();
    refresh_task.abort();
    if let Some(sweep_task) = sweep_task {
        sweep_task.abort();
    }
    if let Some(fold_task) = fold_task {
        fold_task.abort();
    }
    // Blocking-pool tasks cannot be interrupted once running; `abort` here only
    // cancels them if they have not started. Both are safe to leave finishing
    // (read-only hydration; atomic-rename prune) — the runtime waits for them
    // on shutdown.
    hydrate_task.abort();
    if let Some(prune_task) = prune_task {
        prune_task.abort();
    }
    result.map_err(ProxyError::Io)
}

/// The set of group filters to evaluate on each scheduler tick. With routing
/// DISABLED this is a single `[None]` — the legacy single-slot path, byte-for
/// -byte the old behavior. With routing ENABLED it is one `Some(group)` per
/// distinct backend group that has at least one account in the pool, so each
/// group's sticky slot is kept current independently. Groups with no accounts
/// are skipped (nothing to select).
pub fn eval_groups(
    pool: &AccountPool,
    routing_enabled: bool,
) -> Vec<Option<crate::routing::BackendGroup>> {
    if !routing_enabled {
        return vec![None];
    }
    let mut groups: Vec<crate::routing::BackendGroup> =
        pool.snapshot().accounts.iter().map(|a| a.group).collect();
    groups.sort();
    groups.dedup();
    groups.into_iter().map(Some).collect()
}

/// One background-refresh pass (A2): refresh every HEALTHY oauth-style
/// account (anthropic oauth AND codex) whose access token expires within
/// `scheduler.refresh_ahead_secs`. Reuses the request-time path
/// ([`forward::refresh_credential`]: coalescer for anthropic, direct token
/// grant for codex, pool update, persistence). Auth-failed accounts are
/// skipped — a dead refresh token must not be retried every tick (re-login
/// heals via `update_credential`); transient failures are simply retried on
/// the next tick.
pub async fn background_refresh_pass(state: &AppState) {
    let ahead_ms = state
        .config
        .scheduler
        .refresh_ahead_secs
        .saturating_mul(1000);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    // Refresh AT MOST ONE account per pass — the soonest-to-expire. Refreshing
    // every expiring token in one pass was a burst of token-endpoint calls
    // (worst on startup, when many tokens sit inside the 7h window at once),
    // which can trip the upstream's request-rate limit. One-per-pass spaces
    // them across REFRESH_TICK; tokens carry hours of headroom so the sweep has
    // plenty of time, and request-time forced refresh is the backstop.
    let mut candidate: Option<(AccountId, AccountCredential, u64)> = None;
    for account in state.pool.snapshot().accounts {
        if !account.healthy {
            continue;
        }
        let Some(credential) = state.pool.credential(&account.id) else {
            continue;
        };
        let expires_at_ms = match &credential {
            AccountCredential::Oauth { expires_at_ms, .. }
            | AccountCredential::Codex { expires_at_ms, .. }
            | AccountCredential::Grok { expires_at_ms, .. } => *expires_at_ms,
            // Neither an anthropic API key nor an OpenRouter key expires.
            AccountCredential::Apikey { .. } | AccountCredential::OpenRouter { .. } => continue,
        };
        if expires_at_ms.saturating_sub(now_ms) >= ahead_ms {
            continue;
        }
        if candidate
            .as_ref()
            .is_none_or(|(_, _, soonest)| expires_at_ms < *soonest)
        {
            candidate = Some((account.id.clone(), credential, expires_at_ms));
        }
    }
    let Some((account_id, credential, _)) = candidate else {
        return;
    };
    match forward::refresh_credential(state, &account_id, &credential).await {
        forward::RefreshOutcome::Refreshed(fresh) => {
            if let AccountCredential::Oauth { expires_at_ms, .. }
            | AccountCredential::Codex { expires_at_ms, .. } = fresh
            {
                let hours = expires_at_ms.saturating_sub(now_ms) as f64 / 3_600_000.0;
                tracing::info!(
                    account = %account_id,
                    "background token refresh: expires in {hours:.1}h"
                );
            }
        }
        forward::RefreshOutcome::Permanent => {
            state.pool.record_auth_failure(&account_id);
            state.emit(ActivityEvent::Error {
                context: Some("refresh".into()),
                message: format!("{account_id}: refresh token dead; re-login required"),
            });
        }
        forward::RefreshOutcome::Failed => {} // transient — next tick retries
    }
}

/// Build the router: `GET /llmux/status`, `POST /llmux/shutdown`,
/// `POST /v1/oauth/token` (raw relay), fallback → [`forward_any`]. Every
/// route sits behind the proxy api-key check (loopback peers exempt).
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/llmux/status", get(status))
        .route("/llmux/dashboard", get(dashboard_endpoint))
        .route("/llmux/raw-io", get(raw_io_endpoint))
        .route("/llmux/switch", post(switch_endpoint))
        .route("/llmux/codex", post(codex_config_endpoint))
        .route("/llmux/grok", post(grok_config_endpoint))
        .route("/llmux/settings", post(settings_endpoint))
        .route("/llmux/add-account", post(add_account_endpoint))
        .route("/llmux/inject-account", post(inject_account_endpoint))
        .route("/llmux/remove-account", post(remove_account_endpoint))
        .route("/llmux/pause-account", post(pause_account_endpoint))
        .route("/llmux/keys", get(keys_list_endpoint))
        .route("/llmux/keys/new", post(keys_new_endpoint))
        .route("/llmux/keys/suspend", post(keys_suspend_endpoint))
        .route("/llmux/keys/remove", post(keys_remove_endpoint))
        .route("/llmux/keys/rotate", post(keys_rotate_endpoint))
        .route("/llmux/account-limits", post(account_limits_endpoint))
        .route("/llmux/reset-usage", post(reset_usage_endpoint))
        .route("/llmux/scheduler-mode", post(scheduler_mode_endpoint))
        .route("/llmux/events", post(events_endpoint))
        .route("/llmux/login/start", post(login_start_endpoint))
        .route("/llmux/login/status", get(login_status_endpoint))
        .route("/llmux/login/cancel", post(login_cancel_endpoint))
        .route("/llmux/shutdown", post(shutdown))
        .route("/models", get(models_endpoint))
        .route("/llmux/models", get(models_endpoint))
        .route("/v1/oauth/token", post(oauth_token_relay))
        // Root reachability ping, answered LOCALLY (Z 2026-07-15, startup-set
        // bug): Claude Code fires `HEAD /` against its base URL as a
        // connectivity check on session start. The fallback used to forward
        // it upstream with a leased credential, which burned a scheduler pick
        // just to render a `[claude] 404` activity row per new session. A
        // proxy answers reachability itself. GET/HEAD ONLY (axum serves HEAD
        // from the GET handler, body elided) — other methods on `/` get an
        // honest 405 rather than a fake 200 (review MUST-FIX 2).
        .route("/", get(root_ping))
        .fallback(forward_any)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            client_auth,
        ))
        .with_state(state)
}

/// `HEAD|GET /` → 200 "llmux", answered locally (never forwarded, never
/// leased an account, never an activity row). Claude Code probes its base URL
/// with `HEAD /` on every session start; upstream Anthropic 404s that path,
/// so forwarding it only produced a misleading `[claude] 404` per session.
async fn root_ping() -> &'static str {
    "llmux"
}

/// Pure client-auth decision (FR1): when a proxy api key is configured,
/// non-loopback peers must present it as `x-api-key`; loopback peers are
/// exempt. An unknown peer address (no ConnectInfo) is NOT exempt.
/// Scope classification (multi-tenant #22): the control plane is every
/// `/llmux/*` surface except the model catalog alias — key management,
/// account mutation, shutdown, AND the dashboard/status reads (the dashboard
/// document contains every tenant's data). Everything else — the `/v1/*`
/// forwarding fallback, `/models`, the root ping — is the data plane.
pub fn is_control_plane(path: &str) -> bool {
    path == "/llmux" || (path.starts_with("/llmux/") && path != "/llmux/models")
}

fn auth_error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": "authentication_error", "message": message },
    });
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// Two-axis client auth (multi-tenant #22): the LIVE key registry resolves
/// the presented credential to a *tenant identity* (attribution axis), then
/// the route's scope decides whether that identity may pass (privilege axis).
///
/// - Identity: issued `lmk-` key → its tenant; legacy `proxy.api_key` →
///   `legacy` (admin); keyless loopback → `local` (data plane only). Keyless
///   remote is always denied — keyless is loopback-only.
/// - Privilege: control-plane routes (see [`is_control_plane`]) require an
///   ADMIN credential even from loopback — network position is not privilege
///   (an `ssh -L` peer looks loopback). Local CLI/TUI clients present the
///   config's own key automatically, so operator friction is zero.
///
/// The resolved [`Tenant`] rides the request as an extension; the forward
/// path records its `id` on the activity event, which is what makes
/// per-tenant metering land (P1 minimal attribution).
async fn client_auth(
    State(state): State<AppState>,
    mut req: axum::extract::Request,
    next: Next,
) -> Response {
    use crate::proxy::keys::Resolution;
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let loopback = peer.is_some_and(|ip| ip.is_loopback());
    let presented = req.headers().get("x-api-key").and_then(|v| v.to_str().ok());
    match state.keys.resolve(presented, loopback) {
        Resolution::Allowed(tenant) => {
            if is_control_plane(req.uri().path()) && !tenant.admin {
                return auth_error(
                    StatusCode::FORBIDDEN,
                    "admin credential required for llmux control endpoints",
                );
            }
            req.extensions_mut().insert(tenant);
            next.run(req).await
        }
        Resolution::Suspended => auth_error(StatusCode::UNAUTHORIZED, "client key suspended"),
        Resolution::Revoked => auth_error(StatusCode::UNAUTHORIZED, "client key revoked"),
        Resolution::Denied => auth_error(StatusCode::UNAUTHORIZED, "Invalid proxy API key"),
    }
}

/// Domain errors of the client-key admin surface (multi-tenant #22), mapped
/// to HTTP statuses in the `/llmux/keys/*` handlers.
#[derive(Debug)]
pub enum KeyAdminError {
    Config(crate::config::ConfigError),
    /// No non-revoked key with the given id.
    NotFound,
    /// A non-revoked key already carries this name (names must resolve
    /// unambiguously in the CLI).
    NameTaken(String),
    NameRequired,
    /// The mutation would leave ZERO active admin credentials — refused so a
    /// remote admin can't lock everyone out (recovery would then require the
    /// server-local CLI config path).
    LastAdmin,
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn key_admin_error_response(err: KeyAdminError) -> Response {
    match err {
        KeyAdminError::NotFound => relay_error(StatusCode::NOT_FOUND, "client key not found"),
        KeyAdminError::NameTaken(name) => relay_error(
            StatusCode::CONFLICT,
            &format!("a client key named {name:?} already exists"),
        ),
        KeyAdminError::NameRequired => relay_error(StatusCode::BAD_REQUEST, "name is required"),
        KeyAdminError::LastAdmin => relay_error(
            StatusCode::CONFLICT,
            "refusing to disable the last active admin credential",
        ),
        KeyAdminError::Config(crate::config::ConfigError::NoConfigDir) => relay_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "config persistence disabled; cannot mutate client keys",
        ),
        KeyAdminError::Config(err) => relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("failed to persist client keys: {err}"),
        ),
    }
}

/// Serializable metadata view of one client key — NEVER includes the secret
/// or its digest (the digest is not secret-reversible but has no client use;
/// the response surface stays secret-free by construction).
fn client_key_json(key: &crate::config::ClientKey) -> serde_json::Value {
    serde_json::json!({
        "id": key.id,
        "name": key.name,
        "email": key.email,
        "kind": match key.kind {
            crate::config::ClientKeyKind::Admin => "admin",
            crate::config::ClientKeyKind::Default => "default",
        },
        "key_prefix": key.key_prefix,
        "suspended": key.suspended,
        "created_at_ms": key.created_at_ms,
        "revoked_at_ms": key.revoked_at_ms,
    })
}

#[derive(serde::Deserialize)]
struct KeyNewRequest {
    name: String,
    #[serde(default)]
    email: Option<String>,
    /// `"default"` (the default) or `"admin"`.
    #[serde(default)]
    kind: Option<String>,
}

/// `POST /llmux/keys/new` — issue a downstream client key (admin only, like
/// every `/llmux/*` route). The plaintext secret appears in THIS response and
/// nowhere else, ever.
async fn keys_new_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<KeyNewRequest>,
) -> Response {
    let kind = match body.kind.as_deref() {
        None | Some("default") => crate::config::ClientKeyKind::Default,
        Some("admin") => crate::config::ClientKeyKind::Admin,
        Some(other) => {
            return relay_error(
                StatusCode::BAD_REQUEST,
                &format!("unknown key kind {other:?} (expected \"default\" or \"admin\")"),
            )
        }
    };
    match state.issue_client_key(&body.name, body.email.clone(), kind) {
        Ok((row, secret)) => {
            let mut json = client_key_json(&row);
            // Shown once: the caller must store it now (only the digest is kept).
            json["key"] = serde_json::Value::String(secret);
            json["ok"] = serde_json::Value::Bool(true);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                json.to_string(),
            )
                .into_response()
        }
        Err(err) => key_admin_error_response(err),
    }
}

/// `GET /llmux/keys` — list issued keys (metadata only; secrets are neither
/// stored nor returned). Usage summaries join in P2 via the dashboard doc.
async fn keys_list_endpoint(State(state): State<AppState>) -> Response {
    let keys: Vec<serde_json::Value> = state.keys.list().iter().map(client_key_json).collect();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "keys": keys }).to_string(),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct KeySuspendRequest {
    id: String,
    /// `true` to suspend, `false` to resume.
    suspended: bool,
}

/// `POST /llmux/keys/suspend` — suspend/resume by id, effective on the very
/// next request (live registry, no restart).
async fn keys_suspend_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<KeySuspendRequest>,
) -> Response {
    match state.set_client_key_suspended(&body.id, body.suspended) {
        Ok(row) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({ "ok": true, "key": client_key_json(&row) }).to_string(),
        )
            .into_response(),
        Err(err) => key_admin_error_response(err),
    }
}

#[derive(serde::Deserialize)]
struct KeyIdRequest {
    id: String,
}

/// `POST /llmux/keys/remove` — soft-revoke by id: authentication stops now,
/// attribution metadata is preserved forever.
async fn keys_remove_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<KeyIdRequest>,
) -> Response {
    match state.revoke_client_key(&body.id) {
        Ok(row) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({ "ok": true, "key": client_key_json(&row) }).to_string(),
        )
            .into_response(),
        Err(err) => key_admin_error_response(err),
    }
}

/// `POST /llmux/keys/rotate` — new secret, same attribution id. The new
/// plaintext appears in this response only.
async fn keys_rotate_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<KeyIdRequest>,
) -> Response {
    match state.rotate_client_key(&body.id) {
        Ok((row, secret)) => {
            let mut json = serde_json::json!({ "ok": true, "key": client_key_json(&row) });
            json["key"]["key"] = serde_json::Value::String(secret);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                json.to_string(),
            )
                .into_response()
        }
        Err(err) => key_admin_error_response(err),
    }
}

fn epoch_secs(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One model-scoped window (`limits[]` `weekly_scoped`, e.g. "Fable") as a
/// `/llmux/status` JSON object. Utilization uses `effective_utilization` — the
/// same expiry-collapsing convention this endpoint applies to `five_hour` /
/// `seven_day` (an already-reset window reads 0). `scope_label` is included
/// only for the generic `scoped_limits` list; `fable_weekly` surfaces the same
/// fields without it (`with_label = false`).
fn scoped_window_json(
    scoped: &crate::scheduler::window::ScopedQuotaWindow,
    now: SystemTime,
    with_label: bool,
) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    if with_label {
        obj.insert("scope_label".into(), serde_json::json!(scoped.scope_label));
    }
    obj.insert(
        "utilization".into(),
        serde_json::json!(scoped.window.effective_utilization(now)),
    );
    obj.insert(
        "resets_at".into(),
        serde_json::json!(epoch_secs(scoped.window.resets_at)),
    );
    obj.insert(
        "resets_in_secs".into(),
        serde_json::json!(scoped
            .window
            .resets_at
            .duration_since(now)
            .map(|d| d.as_secs())
            .unwrap_or(0)),
    );
    obj.insert(
        "severity".into(),
        serde_json::json!(scoped.severity.label()),
    );
    obj.insert("is_active".into(), serde_json::json!(scoped.is_active));
    serde_json::Value::Object(obj)
}

/// Server-process facts for `/llmux/status` that are not pool state.
#[derive(Debug, Clone, Copy)]
pub struct ServerMeta {
    pub pid: u32,
    pub uptime_secs: u64,
    pub port: u16,
    /// Live `email_anonymous` display setting (see [`AppState::email_anonymous`]).
    pub email_anonymous: bool,
}

/// Serializable `/llmux/status` document — pure function of a pool
/// snapshot + totals + select params + server meta so the shape is
/// unit-testable without a socket. Fields are additive only (the CLI parses
/// this across versions). The `accounts` array is emitted in the
/// scheduler's selection order (B1: current → eligible by rank →
/// ineligible) with a 1-based `order` field and, for ineligible accounts, a
/// `blocked` reason string.
pub fn status_json(
    snapshot: &PoolSnapshot,
    totals: &UsageTotals,
    params: &SelectParams,
    now: SystemTime,
    meta: &ServerMeta,
) -> serde_json::Value {
    let window = |w: &Option<crate::scheduler::window::QuotaWindow>| match w {
        Some(w) => serde_json::json!({
            "utilization": w.effective_utilization(now),
            "resets_at": epoch_secs(w.resets_at),
            "resets_in_secs": w.resets_at
                .duration_since(now)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }),
        None => serde_json::Value::Null,
    };
    let headers_only = crate::scheduler::select::headers_only_mode(snapshot, params, None, now);
    let accounts: Vec<serde_json::Value> =
        crate::scheduler::select::selection_order(snapshot, params, now)
            .into_iter()
            .enumerate()
            .map(|(order, idx)| {
                let account = &snapshot.accounts[idx];
                let cooling = account.cooldown_until.is_some_and(|until| until > now);
                let status = if !account.healthy {
                    "auth_failed"
                } else if cooling {
                    "cooldown"
                } else if snapshot.is_current(&account.id) {
                    "active"
                } else {
                    "ok"
                };
                let blocked =
                    crate::scheduler::select::eligibility(account, params, now, headers_only).map(
                        |reason| {
                            crate::scheduler::select::blocking_reason(account, reason, params, now)
                        },
                    );
                let lifetime = totals.get(&account.id);
                serde_json::json!({
                    "name": account.id.0,
                    "type": account.credential_kind,
                    // Backend group ("claude" / "codex") — the dashboard's
                    // group column; also the key space of `current_by_group`.
                    // Additive: absent in docs written before this existed.
                    "group": account.group.as_str(),
                    "status": status,
                    "order": order + 1,
                    "blocked": blocked,
                    "five_hour": window(&account.five_hour),
                    "seven_day": window(&account.seven_day),
                    // Model-scoped weekly windows (additive; null / empty when
                    // none seen). `fable_weekly` is the "Fable" entry surfaced
                    // for convenient reads; `scoped_limits` is the full generic
                    // list so future scoped models appear without a schema change.
                    "fable_weekly": account
                        .fable_weekly()
                        .map(|s| scoped_window_json(s, now, false)),
                    "scoped_limits": account
                        .scoped_limits
                        .iter()
                        .map(|s| scoped_window_json(s, now, true))
                        .collect::<Vec<_>>(),
                    "cooldown_until": account.cooldown_until.filter(|_| cooling).map(epoch_secs),
                    "in_flight": account.in_flight,
                    // Token health (additive): expiry + last refresh, epoch
                    // ms; null for apikey accounts / unknown expiry / never
                    // refreshed.
                    "token_expires_at_ms": account.token_expires_at_ms,
                    "last_refresh_ms": account.last_refresh_ms,
                    "totals": {
                        "requests": lifetime.requests,
                        "input_tokens": lifetime.input_tokens,
                        "output_tokens": lifetime.output_tokens,
                    },
                })
            })
            .collect();
    // Additive: `current` stays a representative scalar (claude slot if
    // present, else codex) for back-compat with older CLI parsers; the new
    // `current_by_group` object exposes the per-group sticky slots.
    let current_by_group: serde_json::Map<String, serde_json::Value> = snapshot
        .current
        .iter()
        .map(|(group, id)| (group.as_str().to_string(), serde_json::json!(id.0)))
        .collect();
    serde_json::json!({
        "version": crate::build_info::version_string(),
        "pid": meta.pid,
        "uptime_secs": meta.uptime_secs,
        "port": meta.port,
        // Live display setting (additive). Account names above stay REAL —
        // masking is a per-display-surface concern (the TUI render layer,
        // islands' pixelization); masking the API data would break clients'
        // OFF state.
        "email_anonymous": meta.email_anonymous,
        "current": snapshot.representative_current().map(|c| c.0.clone()),
        "current_by_group": current_by_group,
        "accounts": accounts,
    })
}

/// `GET /llmux/status` — JSON scheduler/account state (pool snapshot,
/// current account, cooldowns, build info, pid/uptime/port).
async fn status(State(state): State<AppState>) -> Response {
    let meta = ServerMeta {
        pid: std::process::id(),
        uptime_secs: state.started.elapsed().as_secs(),
        port: state.bound_port.load(Ordering::Relaxed),
        email_anonymous: state.email_anonymous.load(Ordering::Relaxed),
    };
    let body = status_json(
        &state.pool.snapshot(),
        &state.totals,
        &state.select_params(),
        SystemTime::now(),
        &meta,
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        format!("{body:#}"),
    )
        .into_response()
}

/// `GET /llmux/dashboard` — the [`crate::dashboard::DashboardDoc`]: a
/// strict superset of `/llmux/status` (same account fields and ordering)
/// plus scheduler / poller / totals / activity / log state. Behind the same
/// loopback + proxy-api-key gate as every route. The attach-mode client
/// (`llmux dashboard`) polls this; the local TUI builds the same document
/// in-process — one contract, one renderer.
async fn dashboard_endpoint(State(state): State<AppState>) -> Response {
    let doc = dashboard::build_doc(&state, SystemTime::now());
    match serde_json::to_string(&doc) {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(err) => relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("dashboard serialize failed: {err}"),
        ),
    }
}

/// Query for [`raw_io_endpoint`]: the activity id plus the entry's completion
/// timestamp (epoch ms) — the pair [`crate::proxy::raw_io::find_record`] needs
/// to identify ONE record across daemon restarts (ids are per-process).
#[derive(serde::Deserialize)]
struct RawIoQuery {
    id: u64,
    at_ms: u64,
}

/// `GET /llmux/raw-io?id=&at_ms=` — the raw request/response record for one
/// activity entry (TUI UI-7 raw viewer; the attach client has no local file).
/// Sits behind the same proxy-api-key gate as every route. 404 when capture is
/// disabled, the record was pruned, or the id+timestamp pair matches nothing.
/// The backwards scan runs on a blocking thread — the log can be tens of GB.
async fn raw_io_endpoint(State(state): State<AppState>, Query(q): Query<RawIoQuery>) -> Response {
    let path = state
        .config
        .raw_io
        .enabled
        .then(|| state.raw_io_path.clone())
        .flatten();
    let record = tokio::task::spawn_blocking(move || {
        crate::proxy::raw_io::find_record(path.as_deref(), q.id, q.at_ms)
    })
    .await
    .ok()
    .flatten();
    match record {
        Some(record) => match serde_json::to_string(&record) {
            Ok(body) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response(),
            Err(err) => relay_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("raw-io serialize failed: {err}"),
            ),
        },
        None => relay_error(
            StatusCode::NOT_FOUND,
            "no raw-io record for this request (capture disabled, pruned, or unknown id)",
        ),
    }
}

/// Request body for `POST /llmux/switch`.
#[derive(serde::Deserialize)]
struct SwitchRequest {
    account: String,
}

/// `POST /llmux/switch` `{"account":"<name>"}` — manual account switch,
/// the server-side of the dashboard's `s`-key path. Same gate as every route
/// (loopback exempt, otherwise the proxy api key). Runs the identical
/// `AccountPool::switch_to` the in-process TUI calls, emits the
/// `AccountSwitched` activity event on success, and answers `{"ok":true,
/// "current":"<name>"}`. A refused switch (ineligible / unknown account)
/// is a 409 with the scheduler's own refusal reason.
async fn switch_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<SwitchRequest>,
) -> Response {
    let target = AccountId(body.account.clone());
    let now = SystemTime::now();
    let from = state
        .pool
        .snapshot()
        .representative_current()
        .map(|c| c.0.clone());
    match state.pool.switch_to(&target, &state.select_params(), now) {
        Ok(()) => {
            state.emit(ActivityEvent::AccountSwitched {
                from,
                to: target.0.clone(),
                reason: Some("manual".into()),
            });
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::json!({ "ok": true, "current": target.0 }).to_string(),
            )
                .into_response()
        }
        Err(err) => relay_error(StatusCode::CONFLICT, &format!("switch refused: {err}")),
    }
}

/// Partial update for `POST /llmux/codex` (req8.1 — dashboard codex
/// settings). Every field is optional; an omitted field keeps its current
/// value. For `reasoning_effort`, an empty string or `"unset"` clears it
/// (back to the backend default). Applies to the LIVE provider immediately and
/// persists to the config file so it survives a restart.
#[derive(serde::Deserialize)]
struct CodexConfigRequest {
    fast: Option<bool>,
    default_model: Option<String>,
    reasoning_effort: Option<String>,
}

async fn codex_config_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<CodexConfigRequest>,
) -> Response {
    let mut shape = state.codex.shape();
    if let Some(fast) = body.fast {
        shape.fast = fast;
    }
    if let Some(model) = body.default_model.as_deref() {
        if !model.trim().is_empty() {
            shape.model = model.trim().to_string();
        }
    }
    if let Some(effort) = body.reasoning_effort.as_deref() {
        let e = effort.trim();
        shape.effort = if e.is_empty() || e.eq_ignore_ascii_case("unset") {
            None
        } else {
            Some(e.to_ascii_lowercase())
        };
    }
    // Persist FIRST (config-editor contract): a failed write leaves the
    // live shape untouched and surfaces the error — live-then-persist would
    // let disk and runtime diverge behind a 200. No config path = nothing to
    // persist → refuse rather than diverge.
    let Some(path) = &state.config_path else {
        return relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "config write failed: no config path — persistence unavailable",
        );
    };
    if let Err(err) = crate::config::update_path(path, |c| {
        c.codex.default_model = shape.model.clone();
        c.codex.fast = shape.fast;
        c.codex.reasoning_effort = shape.effort.clone();
    }) {
        return relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config write failed: {err}"),
        );
    }
    state.codex.set_shape(shape.clone());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "ok": true,
            "fast": shape.fast,
            "default_model": shape.model,
            "reasoning_effort": shape.effort,
        })
        .to_string(),
    )
        .into_response()
}

/// Partial update for `POST /llmux/grok` (docs/grok/spec.md §R4, C10 —
/// dashboard/islands grok settings). Same partial-update contract as
/// `POST /llmux/codex`; differences (intentional, spec §R1/T4): effort is
/// VALIDATED against the closed superset `none|low|medium|high` (per-model
/// clamping happens at request time), there is no `fast` (xAI has no service
/// tier), and the response reports whether the config write succeeded
/// (`persisted`) instead of hiding a failed persist.
#[derive(serde::Deserialize)]
struct GrokConfigRequest {
    default_model: Option<String>,
    reasoning_effort: Option<String>,
}

async fn grok_config_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<GrokConfigRequest>,
) -> Response {
    let mut shape = state.grok.shape();
    if let Some(model) = body.default_model.as_deref() {
        if !model.trim().is_empty() {
            shape.model = model.trim().to_string();
        }
    }
    if let Some(effort) = body.reasoning_effort.as_deref() {
        let e = effort.trim().to_ascii_lowercase();
        if e.is_empty() || e == "unset" {
            shape.effort = None;
        } else if crate::provider::grok::is_valid_config_effort(&e) {
            shape.effort = Some(e);
        } else {
            return relay_error(
                StatusCode::BAD_REQUEST,
                "reasoning_effort must be one of none|low|medium|high (or empty/'unset' to clear)",
            );
        }
    }
    // Persist FIRST (config-editor contract): a failed write leaves the
    // live shape untouched and surfaces the error; no config path refuses.
    let Some(path) = &state.config_path else {
        return relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "config write failed: no config path — persistence unavailable",
        );
    };
    if let Err(err) = crate::config::update_path(path, |c| {
        c.grok.default_model = shape.model.clone();
        c.grok.reasoning_effort = shape.effort.clone();
    }) {
        return relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config write failed: {err}"),
        );
    }
    let persisted = true;
    state.grok.set_shape(shape.clone());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "ok": true,
            "default_model": shape.model,
            "reasoning_effort": shape.effort,
            "persisted": persisted,
        })
        .to_string(),
    )
        .into_response()
}

/// `GET /models` and `GET /llmux/models` — the catalog of KNOWN models: a
/// curated set (id / display name / effort menu / context window / group) plus
/// the one live bit, grok's family alias `"grok"` resolving to the current grok
/// pin. Not an exhaustive list of everything routable — arbitrary `grok-*` ids
/// still pass through at request time, and an out-of-catalog pin surfaces as a
/// synthesized null-metadata row (see [`crate::catalog`]). Same payload on both
/// paths.
///
/// Registering root `/models` reserves a path that previously fell through to
/// the upstream proxy fallback; Anthropic exposes no root `/models`, and
/// `/v1/models` is untouched (still proxied), so nothing regresses.
async fn models_endpoint(State(state): State<AppState>) -> Response {
    let models = crate::catalog::catalog(
        &state.grok.model(),
        &state.codex.model(),
        state.openrouter.model(),
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "models": models }).to_string(),
    )
        .into_response()
}

/// Partial update for `POST /llmux/settings` — daemon-wide display settings.
/// Every field is optional; an omitted field keeps its current value (same
/// contract as `POST /llmux/codex`), so the endpoint stays additive as more
/// settings join it.
/// `POST /llmux/settings` body: every field optional — only the fields
/// present are validated + applied. Live fields flip a [`SettingsLive`]
/// holder after the persist; restart-only fields persist read-merge-write
/// and are reported back under `restart_required`. Config-editor v1
/// (trinity contract C6).
#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct SettingsRequest {
    /// Mask account emails on display surfaces (TUI render layer; islands
    /// mirrors it into its pixelization). See `Config::email_anonymous`.
    pub email_anonymous: Option<bool>,
    pub tui_effects: Option<bool>,
    pub show_fable_weekly: Option<bool>,
    /// `"used"` | `"remaining"`.
    pub quota_display: Option<String>,
    pub routing_enabled: Option<bool>,
    pub raw_io_enabled: Option<bool>,
    /// Scheduler ceilings, fractions in `[0.0, 1.0]`.
    pub five_hour_max: Option<f64>,
    pub seven_day_max: Option<f64>,
    pub fable_weekly_max: Option<f64>,
    /// Scheduler usage evidence max age, seconds in `[5, 3600]`.
    pub usage_max_age_secs: Option<u64>,
    // ---- persisted-only (effective on next daemon start) ----
    pub raw_io_retention_days: Option<u64>,
    pub raw_io_max_body_bytes: Option<u64>,
    /// `"claude"` | `"codex"` | `"grok"`.
    pub routing_default_group: Option<String>,
    /// `"error"` | `"fallback"`.
    pub routing_on_empty_group: Option<String>,
    pub tui_gradient_speed: Option<f32>,
    pub upstream: Option<String>,
    pub codex_upstream: Option<String>,
    pub proxy_port: Option<u16>,
    pub proxy_max_request_bytes: Option<u64>,
}

/// The `POST /llmux/settings` acknowledgment. TYPED on both sides: the TUI
/// parses exactly this shape and treats anything else as UNVERIFIED — a
/// malformed or empty 2xx must never read as a confirmed apply.
///
/// `email_anonymous` is the pre-editor wire contract: the Islands client
/// (llmux-islands-core `client.rs` `UpdateSettings`) requires the response to
/// echo the current live value and rejects the ack otherwise — it stays
/// always-serialized. It is `#[serde(default)]` on the parse side only so the
/// TUI can read acks from a daemon predating the echo... which cannot happen
/// forward, but keeps the field non-load-bearing for the TUI's verdict.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SettingsAck {
    pub ok: bool,
    pub applied: Vec<String>,
    pub restart_required: Vec<String>,
    #[serde(default)]
    pub email_anonymous: bool,
}

/// One validated settings change, applied to config (read-merge-write) and,
/// for live fields, to the [`SettingsLive`] holders. Returns the applied
/// field names split into live vs restart-required — the TUI status line and
/// config-tab labels are driven by this split, so a "wrote but won't apply
/// until restart" change can never masquerade as live (trinity contract C6:
/// no silent lies).
pub fn apply_settings(
    state: &AppState,
    req: &SettingsRequest,
) -> Result<(Vec<&'static str>, Vec<&'static str>), String> {
    use std::sync::atomic::Ordering;
    // ---- validate everything BEFORE writing anything (atomic apply). ----
    let quota = match req.quota_display.as_deref() {
        None => None,
        Some("used") => Some(crate::config::QuotaDisplay::Used),
        Some("remaining") => Some(crate::config::QuotaDisplay::Remaining),
        Some(other) => return Err(format!("quota_display: unknown value {other:?}")),
    };
    for (name, v) in [
        ("five_hour_max", req.five_hour_max),
        ("seven_day_max", req.seven_day_max),
        ("fable_weekly_max", req.fable_weekly_max),
    ] {
        if let Some(v) = v {
            if !(0.0..=1.0).contains(&v) || v.is_nan() {
                return Err(format!("{name}: must be a fraction in [0.0, 1.0]"));
            }
        }
    }
    if let Some(age) = req.usage_max_age_secs {
        if !(5..=3600).contains(&age) {
            return Err("usage_max_age_secs: must be in [5, 3600]".into());
        }
    }
    if let Some(days) = req.raw_io_retention_days {
        if days > 3650 {
            return Err("raw_io_retention_days: must be <= 3650".into());
        }
    }
    if let Some(bytes) = req.raw_io_max_body_bytes {
        if !(1024..=1_073_741_824).contains(&bytes) {
            return Err("raw_io_max_body_bytes: must be in [1 KiB, 1 GiB]".into());
        }
    }
    if let Some(group) = req.routing_default_group.as_deref() {
        if !["claude", "codex", "grok", "openrouter"].contains(&group) {
            return Err(format!("routing_default_group: unknown group {group:?}"));
        }
    }
    if let Some(mode) = req.routing_on_empty_group.as_deref() {
        if !["error", "fallback"].contains(&mode) {
            return Err(format!("routing_on_empty_group: unknown mode {mode:?}"));
        }
    }
    if let Some(speed) = req.tui_gradient_speed {
        if !(0.01..=10.0).contains(&speed) || speed.is_nan() {
            return Err("tui_gradient_speed: must be in [0.01, 10.0]".into());
        }
    }
    for (name, url) in [
        ("upstream", req.upstream.as_deref()),
        ("codex_upstream", req.codex_upstream.as_deref()),
    ] {
        if let Some(url) = url {
            // Structural validation, not a prefix check: a hostless
            // "http://" or an unparsable URL would break EVERY request.
            let parsed = reqwest::Url::parse(url).map_err(|e| format!("{name}: {e}"))?;
            if !(parsed.scheme() == "http" || parsed.scheme() == "https") {
                return Err(format!("{name}: must be an http(s) URL"));
            }
            if parsed.host_str().is_none() {
                return Err(format!("{name}: URL has no host"));
            }
        }
    }
    if let Some(port) = req.proxy_port {
        if port == 0 {
            return Err("proxy_port: must be non-zero".into());
        }
    }
    if let Some(bytes) = req.proxy_max_request_bytes {
        if !(65_536..=1_073_741_824).contains(&bytes) {
            return Err("proxy_max_request_bytes: must be in [64 KiB, 1 GiB]".into());
        }
    }

    // ---- persist all requested fields in ONE read-merge-write. ----
    // A persistent editor must not pretend: with no config path there is
    // nothing to write, and flipping holders anyway would silently diverge
    // runtime from disk (review MUST-FIX 4).
    if state.config_path.is_none() {
        return Err("config write failed: no config path — persistence unavailable".into());
    }
    if let Some(path) = &state.config_path {
        let req_clone = SettingsRequest {
            email_anonymous: req.email_anonymous,
            tui_effects: req.tui_effects,
            show_fable_weekly: req.show_fable_weekly,
            quota_display: req.quota_display.clone(),
            routing_enabled: req.routing_enabled,
            raw_io_enabled: req.raw_io_enabled,
            five_hour_max: req.five_hour_max,
            seven_day_max: req.seven_day_max,
            fable_weekly_max: req.fable_weekly_max,
            usage_max_age_secs: req.usage_max_age_secs,
            raw_io_retention_days: req.raw_io_retention_days,
            raw_io_max_body_bytes: req.raw_io_max_body_bytes,
            routing_default_group: req.routing_default_group.clone(),
            routing_on_empty_group: req.routing_on_empty_group.clone(),
            tui_gradient_speed: req.tui_gradient_speed,
            upstream: req.upstream.clone(),
            codex_upstream: req.codex_upstream.clone(),
            proxy_port: req.proxy_port,
            proxy_max_request_bytes: req.proxy_max_request_bytes,
        };
        let quota_for_write = quota;
        crate::config::update_path(path, move |c| {
            let r = &req_clone;
            if let Some(v) = r.email_anonymous {
                c.email_anonymous = v;
            }
            if let Some(v) = r.tui_effects {
                c.tui_effects = v;
            }
            if let Some(v) = r.show_fable_weekly {
                c.show_fable_weekly = v;
            }
            if let Some(q) = quota_for_write {
                c.quota_display = q;
            }
            if let Some(v) = r.routing_enabled {
                c.routing.enabled = v;
            }
            if let Some(v) = r.raw_io_enabled {
                c.raw_io.enabled = v;
            }
            if let Some(v) = r.five_hour_max {
                c.scheduler.five_hour_max = v;
            }
            if let Some(v) = r.seven_day_max {
                c.scheduler.seven_day_max = v;
            }
            if let Some(v) = r.fable_weekly_max {
                c.scheduler.fable_weekly_max = v;
            }
            if let Some(v) = r.usage_max_age_secs {
                c.scheduler.usage_max_age_secs = v;
            }
            if let Some(v) = r.raw_io_retention_days {
                c.raw_io.retention_days = v;
            }
            if let Some(v) = r.raw_io_max_body_bytes {
                c.raw_io.max_body_bytes = usize::try_from(v).unwrap_or(usize::MAX);
            }
            if let Some(v) = &r.routing_default_group {
                c.routing.default_group = v.clone();
            }
            if let Some(v) = &r.routing_on_empty_group {
                c.routing.on_empty_group = v.clone();
            }
            if let Some(v) = r.tui_gradient_speed {
                c.tui_gradient.speed = v;
            }
            if let Some(v) = &r.upstream {
                c.upstream = v.clone();
            }
            if let Some(v) = &r.codex_upstream {
                c.codex.upstream = v.clone();
            }
            if let Some(v) = r.proxy_port {
                c.proxy.port = v;
            }
            if let Some(v) = r.proxy_max_request_bytes {
                c.proxy.max_request_bytes = usize::try_from(v).unwrap_or(usize::MAX);
            }
        })
        .map_err(|err| format!("config write failed: {err}"))?;
    }

    // ---- flip live holders (only after the persist succeeded). ----
    let live = &state.settings_live;
    let mut applied: Vec<&'static str> = Vec::new();
    let mut restart: Vec<&'static str> = Vec::new();
    if let Some(v) = req.email_anonymous {
        state.email_anonymous.store(v, Ordering::Relaxed);
        applied.push("email_anonymous");
    }
    if let Some(v) = req.tui_effects {
        live.tui_effects.store(v, Ordering::Relaxed);
        applied.push("tui_effects");
    }
    if let Some(v) = req.show_fable_weekly {
        live.show_fable_weekly.store(v, Ordering::Relaxed);
        applied.push("show_fable_weekly");
    }
    if let Some(q) = quota {
        live.quota_remaining.store(
            q == crate::config::QuotaDisplay::Remaining,
            Ordering::Relaxed,
        );
        applied.push("quota_display");
    }
    if let Some(v) = req.routing_enabled {
        live.routing_enabled.store(v, Ordering::Relaxed);
        applied.push("routing_enabled");
    }
    if let Some(v) = req.raw_io_enabled {
        live.raw_io_enabled.store(v, Ordering::Relaxed);
        applied.push("raw_io_enabled");
    }
    if let Some(v) = req.five_hour_max {
        live.five_hour_max.store(v.to_bits(), Ordering::Relaxed);
        applied.push("five_hour_max");
    }
    if let Some(v) = req.seven_day_max {
        live.seven_day_max.store(v.to_bits(), Ordering::Relaxed);
        applied.push("seven_day_max");
    }
    if let Some(v) = req.fable_weekly_max {
        live.fable_weekly_max.store(v.to_bits(), Ordering::Relaxed);
        applied.push("fable_weekly_max");
    }
    if let Some(v) = req.usage_max_age_secs {
        live.usage_max_age_secs.store(v, Ordering::Relaxed);
        applied.push("usage_max_age_secs");
    }
    // Persisted-only fields: the running daemon keeps its boot value.
    for (name, present) in [
        ("raw_io_retention_days", req.raw_io_retention_days.is_some()),
        ("raw_io_max_body_bytes", req.raw_io_max_body_bytes.is_some()),
        ("routing_default_group", req.routing_default_group.is_some()),
        (
            "routing_on_empty_group",
            req.routing_on_empty_group.is_some(),
        ),
        ("tui_gradient_speed", req.tui_gradient_speed.is_some()),
        ("upstream", req.upstream.is_some()),
        ("codex_upstream", req.codex_upstream.is_some()),
        ("proxy_port", req.proxy_port.is_some()),
        (
            "proxy_max_request_bytes",
            req.proxy_max_request_bytes.is_some(),
        ),
    ] {
        if present {
            restart.push(name);
        }
    }
    if !applied.is_empty() || !restart.is_empty() {
        tracing::info!(?applied, restart_required = ?restart, "settings updated");
    }
    Ok((applied, restart))
}

/// `POST /llmux/settings` `{"email_anonymous":true|false}` — flip the
/// email-anonymous display setting remotely (SSOT E3). Same loopback /
/// proxy-api-key gate as every route (it sits on the shared `.route(...)`
/// chain behind `client_auth`). Persist-then-apply: the value is written
/// read-merge-write via [`crate::config::update_path`] FIRST (so a failed
/// write never leaves live state diverged from disk), then swapped into the
/// live [`AppState::email_anonymous`] atomic so status/dashboard/TUI reflect
/// it with no restart. The response echoes the effective value; an empty body
/// `{}` is a read-back no-op.
async fn settings_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<SettingsRequest>,
) -> Response {
    let (applied, restart_required) = match apply_settings(&state, &body) {
        Ok(result) => result,
        Err(err) => {
            // Validation errors are the caller's fault (400); a persist
            // failure is ours (500).
            let status = if err.starts_with("config write failed") {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::BAD_REQUEST
            };
            return relay_error(status, &err);
        }
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&SettingsAck {
            ok: true,
            applied: applied.iter().map(|s| s.to_string()).collect(),
            restart_required: restart_required.iter().map(|s| s.to_string()).collect(),
            email_anonymous: state.email_anonymous.load(Ordering::Relaxed),
        })
        .unwrap_or_else(|_| "{\"ok\":true}".into()),
    )
        .into_response()
}

/// Request body for `POST /llmux/add-account` — an API-key account
/// (issue #3; OAuth/codex login-from-dashboard is issue #4, out of scope).
/// `name` is optional: when omitted the server assigns the next `api-N` name,
/// mirroring `cli::login::login_api`.
#[derive(serde::Deserialize)]
struct AddAccountRequest {
    #[serde(default)]
    name: Option<String>,
    api_key: String,
}

/// `POST /llmux/add-account` `{"api_key":"...","name":"..."?}` — add (upsert)
/// an API-key account from the dashboard, in BOTH local and attach mode. Same
/// loopback / proxy-api-key gate as every route (it sits on the shared
/// `.route(...)` chain behind `client_auth`). The credential is written
/// read-merge-write via [`crate::config::update_path`] (never load/edit/save
/// around the running server) and the live pool is reloaded so the daemon
/// picks it up with no restart. The api key is NEVER logged and the response
/// echoes only a masked form (`crate::proxy::logging::mask_credentials`).
async fn add_account_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<AddAccountRequest>,
) -> Response {
    let api_key = body.api_key.trim();
    if api_key.is_empty() {
        return relay_error(StatusCode::BAD_REQUEST, "api_key is required");
    }
    let requested_name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());

    match state.add_apikey_account(requested_name, api_key) {
        Ok((name, outcome)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({
                "ok": true,
                "name": name,
                "type": "apikey",
                "added": matches!(outcome, crate::config::Upsert::Added),
                // Masked echo only — never the raw key (AGENTS.md credential rule).
                "api_key_masked": crate::proxy::logging::mask_credentials(api_key),
            })
            .to_string(),
        )
            .into_response(),
        Err(crate::config::ConfigError::NoConfigDir) => relay_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "config persistence disabled; cannot add account",
        ),
        Err(err) => relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config write failed: {err}"),
        ),
    }
}

/// Request body for `POST /llmux/inject-account` (issue #4) — a fully-formed
/// OAuth/Codex credential the CLIENT minted by running the browser login
/// locally, relayed to the daemon so the new account joins the pool with no
/// restart. The body deserializes straight into an [`AccountConfig`] (the
/// `type`-tagged credential enum), so `{"name":"claude:me","type":"oauth",
/// "account_uuid":"…","access_token":"…","refresh_token":"…",
/// "expires_at_ms":…}` and the `type:"codex"` shape both parse. An
/// `type:"apikey"` body is rejected by [`AppState::inject_account`] — api-key
/// accounts use `/llmux/add-account`, which never needs a browser.
#[derive(serde::Deserialize)]
struct InjectAccountRequest {
    #[serde(flatten)]
    account: AccountConfig,
}

/// `POST /llmux/inject-account` — inject an OAuth/Codex account from the
/// dashboard, in BOTH local and attach mode. This is the daemon side of the
/// issue #4 architecture: the CLIENT runs the OAuth browser+callback flow
/// (local = the daemon host; attach = the operator's machine) and POSTs the
/// resulting token here, making local and attach ONE code path. Same loopback
/// / proxy-api-key gate as every route (it sits on the shared `.route(...)`
/// chain behind `client_auth`). The credential is written read-merge-write via
/// [`crate::config::update_path`] and the live pool is reloaded so the daemon
/// picks it up with no restart. NO token is ever logged; the response echoes
/// only the account name, kind, and a MASKED access token
/// (`crate::proxy::logging::mask_credentials`).
async fn inject_account_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<InjectAccountRequest>,
) -> Response {
    let account = body.0.account;
    if account.name.trim().is_empty() {
        return relay_error(StatusCode::BAD_REQUEST, "account name is required");
    }
    // Capture a masked echo of the access token BEFORE moving the account into
    // the upsert — never the raw token (AGENTS.md credential rule).
    let access_token_masked = match &account.credential {
        AccountCredential::Oauth { access_token, .. }
        | AccountCredential::Codex { access_token, .. }
        | AccountCredential::Grok { access_token, .. } => {
            Some(crate::proxy::logging::mask_credentials(access_token))
        }
        AccountCredential::Apikey { .. } | AccountCredential::OpenRouter { .. } => None,
    };
    let kind = account.credential.kind();

    match state.inject_account(account) {
        Ok((name, outcome)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({
                "ok": true,
                "name": name,
                "type": kind,
                "added": matches!(outcome, crate::config::Upsert::Added),
                "access_token_masked": access_token_masked,
            })
            .to_string(),
        )
            .into_response(),
        Err(crate::config::ConfigError::Invalid(msg)) => relay_error(StatusCode::BAD_REQUEST, &msg),
        Err(crate::config::ConfigError::NoConfigDir) => relay_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "config persistence disabled; cannot inject account",
        ),
        Err(err) => relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config write failed: {err}"),
        ),
    }
}

/// Request body for `POST /llmux/login/start`.
#[derive(serde::Deserialize)]
struct LoginStartRequest {
    /// `"claude"` or `"codex"` (aliases accepted, see [`LoginProvider::parse`]).
    provider: String,
}

/// Query for `GET /llmux/login/status`.
#[derive(serde::Deserialize)]
struct LoginStatusQuery {
    state: String,
}

/// Request body for `POST /llmux/login/cancel`.
#[derive(serde::Deserialize)]
struct LoginCancelRequest {
    state: String,
}

/// `POST /llmux/login/start` `{"provider":"claude"|"codex"}` — begin a
/// GUI-initiated OAuth login (FR4, `.prd/11-llmux-islands-spec.md`). Unlike
/// `/llmux/inject-account` (where the CLIENT runs the browser flow and relays
/// the token), here the DAEMON runs the same PKCE browser flow the CLI uses —
/// `cli::login::oauth_login_to_account` (Claude) /
/// `auth::codex::login_codex_interactive` (Codex) — on the daemon host, then
/// [`AppState::inject_account`]s the result into the live pool with no restart.
///
/// Single in-flight login: the OAuth callback binds a fixed localhost port, so
/// a second `start` while one is pending is a `409`. Returns `{"state":"…"}`;
/// poll `GET /llmux/login/status?state=` for the outcome. NO provider token
/// crosses this boundary — only the opaque state id and, on success, the new
/// account name. Same loopback / proxy-api-key gate as every route.
async fn login_start_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<LoginStartRequest>,
) -> Response {
    let Some(provider) = super::login::LoginProvider::parse(&body.provider) else {
        return relay_error(
            StatusCode::BAD_REQUEST,
            "provider must be 'claude', 'codex', or 'grok'",
        );
    };
    if state.config_path.is_none() {
        return relay_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "config persistence disabled; cannot add account",
        );
    }
    let state_id = ulid::Ulid::new().to_string();
    if !state.logins.begin(state_id.clone()) {
        return relay_error(
            StatusCode::CONFLICT,
            "a login is already in progress; cancel it or wait for it to finish",
        );
    }

    // Run the browser flow off the request: `start` returns immediately and the
    // app polls `status`. The task injects the account on success and records
    // the terminal phase against this `state`.
    let app = state.clone();
    let task_state = state_id.clone();
    let handle = tokio::spawn(async move {
        let phase = match run_login(&app, provider, &task_state).await {
            Ok(account) => super::login::LoginPhase::Done { account },
            Err(message) => super::login::LoginPhase::Error { message },
        };
        app.logins.finish(&task_state, phase);
    });
    state.logins.attach_handle(&state_id, handle);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "ok": true,
            "state": state_id,
            "provider": provider.as_str(),
        })
        .to_string(),
    )
        .into_response()
}

/// Run the provider's browser login on the daemon and inject the resulting
/// account. Returns the resolved account name. Errors are stringified — the
/// `auth`/`cli` error types carry only status/shape, never the secret, so the
/// message is token-free (AGENTS.md credential rule).
async fn run_login(
    state: &AppState,
    provider: super::login::LoginProvider,
    state_id: &str,
) -> Result<String, String> {
    use super::login::LoginProvider;
    let account = match provider {
        LoginProvider::Claude => {
            crate::cli::login::oauth_login_to_account(&state.client, &state.config.upstream)
                .await
                .map_err(|e| e.to_string())?
        }
        LoginProvider::Codex => crate::auth::codex::login_codex_interactive(
            &state.client,
            &state.config.codex.token_url,
        )
        .await
        .map_err(|e| e.to_string())?,
        LoginProvider::Grok => {
            // Device-code flow (docs/grok/spec.md T2): no localhost
            // callback. Publish the verification URL so the GUI can open it
            // (and best-effort open it daemon-side too), then poll.
            let discovery = crate::auth::grok::discover(&state.client)
                .await
                .map_err(|e| e.to_string())?;
            let device = crate::auth::grok::request_device_code(&state.client, &discovery)
                .await
                .map_err(|e| e.to_string())?;
            state.logins.set_verification(
                state_id,
                device.open_url().to_string(),
                device.user_code.clone(),
            );
            crate::auth::oauth::open_browser(device.open_url());
            let bundle =
                crate::auth::grok::poll_token(&state.client, &discovery.token_endpoint, &device)
                    .await
                    .map_err(|e| e.to_string())?;
            crate::auth::grok::account_from_bundle(&bundle, &discovery.token_endpoint)
                .map_err(|e| e.to_string())?
        }
        LoginProvider::OpenRouter => {
            // OAuth PKCE with a localhost callback (docs/openrouter/spec.md
            // §R5). The exchange yields a long-lived API key, so there is no
            // token/expiry to publish — just the account.
            let api_key = crate::auth::openrouter::login_interactive(&state.client)
                .await
                .map_err(|e| e.to_string())?;
            let label = crate::auth::openrouter::fetch_key_label(
                &state.client,
                crate::auth::openrouter::KEY_INFO_URL,
                &api_key,
            )
            .await
            .unwrap_or_default();
            let trimmed = label.trim();
            // Same naming rule as `llmux login --openrouter` — one shared
            // helper so an unlabeled dashboard login gets `or:key-N` rather
            // than repeatedly overwriting a single `or:key`.
            let name = match crate::config::load_or_init() {
                Ok(cfg) => crate::cli::login::openrouter_account_name(&cfg, trimmed),
                // No readable config yet: fall back to the label-or-first-slot
                // name; `inject_account`'s read-merge-write still upserts.
                Err(_) => crate::cli::login::openrouter_account_name(
                    &crate::config::Config::default(),
                    trimmed,
                ),
            };
            crate::config::AccountConfig {
                name,
                credential: crate::config::AccountCredential::OpenRouter {
                    api_key,
                    label: trimmed.to_string(),
                },
            }
        }
    };
    state
        .inject_account(account)
        .map(|(name, _outcome)| name)
        .map_err(|e| e.to_string())
}

/// `GET /llmux/login/status?state=…` — poll a login started by
/// `/llmux/login/start`. `{"phase":"pending"}` while the browser flow runs,
/// `{"phase":"done","account":"…"}` once injected, `{"phase":"error","error":"…"}`
/// on failure/cancel, or `404` for an unknown/expired state.
async fn login_status_endpoint(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<LoginStatusQuery>,
) -> Response {
    let body = match state.logins.status(&query.state) {
        Some(super::login::LoginPhase::Pending) => {
            // Additive: `verification_uri`/`user_code` appear only once the
            // grok device flow mints them (docs/grok/spec.md T2, C11);
            // older clients ignore the extra fields.
            let (uri, code) = state.logins.verification(&query.state);
            match uri {
                Some(uri) => serde_json::json!({
                    "phase": "pending",
                    "verification_uri": uri,
                    "user_code": code,
                }),
                None => serde_json::json!({ "phase": "pending" }),
            }
        }
        Some(super::login::LoginPhase::Done { account }) => {
            serde_json::json!({ "phase": "done", "account": account })
        }
        Some(super::login::LoginPhase::Error { message }) => {
            serde_json::json!({ "phase": "error", "error": message })
        }
        None => return relay_error(StatusCode::NOT_FOUND, "unknown or expired login state"),
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// `POST /llmux/login/cancel` `{"state":"…"}` — abandon an in-progress login
/// (aborts the browser/callback wait). `{"cancelled":true}` if a pending login
/// was cancelled, `{"cancelled":false}` if it was already terminal/unknown.
async fn login_cancel_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<LoginCancelRequest>,
) -> Response {
    let cancelled = state.logins.cancel(&body.state);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "ok": true, "cancelled": cancelled }).to_string(),
    )
        .into_response()
}

/// Request body for `POST /llmux/remove-account`. `confirm` must be `true` —
/// a destructive delete requires explicit confirmation (matches the CLI's
/// `remove --yes` gate); the TUI supplies it via a second-key confirm.
#[derive(serde::Deserialize)]
struct RemoveAccountRequest {
    name: String,
    #[serde(default)]
    confirm: bool,
}

/// `POST /llmux/remove-account` `{"name":"...","confirm":true}` — remove an
/// account from the dashboard in BOTH local and attach mode. Same gate as
/// every route. Read-merge-write removal via [`crate::config::update_path`]
/// (preserves every other account) and a live pool reload so the change takes
/// effect with no restart. Refuses without `confirm: true` (a 400) so a
/// destructive delete is never silent.
async fn remove_account_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<RemoveAccountRequest>,
) -> Response {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return relay_error(StatusCode::BAD_REQUEST, "name is required");
    }
    if !body.confirm {
        return relay_error(
            StatusCode::BAD_REQUEST,
            &format!("refusing to remove {name:?} without confirmation; set confirm=true"),
        );
    }

    match state.remove_account(&name) {
        Ok(true) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({ "ok": true, "name": name, "removed": true }).to_string(),
        )
            .into_response(),
        Ok(false) => relay_error(
            StatusCode::NOT_FOUND,
            &format!("account {name:?} not found"),
        ),
        Err(crate::config::ConfigError::NoConfigDir) => relay_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "config persistence disabled; cannot remove account",
        ),
        Err(err) => relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config write failed: {err}"),
        ),
    }
}

#[derive(serde::Deserialize)]
struct PauseAccountRequest {
    /// Account name (real id, e.g. `claude:x@y`).
    account: String,
    /// `true` to pause (exclude from selection), `false` to resume.
    paused: bool,
}

/// `POST /llmux/pause-account` — set/clear the operator pause on one account
/// (TUI `p` in the switcher; llmux-islands context menu). Persisted to config
/// `paused_accounts` and applied to the live pool immediately.
async fn pause_account_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<PauseAccountRequest>,
) -> Response {
    let name = body.account.trim().to_string();
    if name.is_empty() {
        return relay_error(StatusCode::BAD_REQUEST, "account is required");
    }
    match state.set_account_paused(&name, body.paused) {
        Ok(true) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({ "ok": true, "account": name, "paused": body.paused }).to_string(),
        )
            .into_response(),
        Ok(false) => relay_error(
            StatusCode::NOT_FOUND,
            &format!("account {name:?} not found"),
        ),
        Err(crate::config::ConfigError::NoConfigDir) => relay_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "config persistence disabled; cannot pause account",
        ),
        Err(err) => relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config write failed: {err}"),
        ),
    }
}

#[derive(serde::Deserialize)]
struct AccountLimitsRequest {
    account: String,
    /// Fractions 0..=1; absent field = no override for that window. All three
    /// absent = clear the account's overrides entirely.
    five_hour_max: Option<f64>,
    seven_day_max: Option<f64>,
    fable_weekly_max: Option<f64>,
}

/// `POST /llmux/account-limits` — set/clear per-account utilization ceilings
/// (TUI `L` in the switcher). Values are fractions 0..=1.
async fn account_limits_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<AccountLimitsRequest>,
) -> Response {
    let name = body.account.trim().to_string();
    if name.is_empty() {
        return relay_error(StatusCode::BAD_REQUEST, "account is required");
    }
    for (label, v) in [
        ("five_hour_max", body.five_hour_max),
        ("seven_day_max", body.seven_day_max),
        ("fable_weekly_max", body.fable_weekly_max),
    ] {
        if let Some(v) = v {
            if !(0.0..=1.0).contains(&v) {
                return relay_error(
                    StatusCode::BAD_REQUEST,
                    &format!("{label} must be a fraction in 0..=1 (got {v})"),
                );
            }
        }
    }
    let limits = crate::config::AccountLimits {
        five_hour_max: body.five_hour_max,
        seven_day_max: body.seven_day_max,
        fable_weekly_max: body.fable_weekly_max,
    };
    match state.set_account_limits(&name, limits) {
        Ok(true) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({ "ok": true, "account": name }).to_string(),
        )
            .into_response(),
        Ok(false) => relay_error(
            StatusCode::NOT_FOUND,
            &format!("account {name:?} not found"),
        ),
        Err(crate::config::ConfigError::NoConfigDir) => relay_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "config persistence disabled; cannot set limits",
        ),
        Err(err) => relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config write failed: {err}"),
        ),
    }
}

#[derive(serde::Deserialize)]
struct SchedulerModeRequest {
    /// `"default"` or `"round-robin"`.
    mode: crate::config::SchedulerMode,
}

/// `POST /llmux/reset-usage` (issue #115) — force every account's usage
/// windows, scoped limits and cooldowns back to COLD. For the operator case
/// where the provider reset quota server-side: the in-memory gauges keep
/// overstating utilization until they age out, so the scheduler re-learns
/// from fresh polls/response headers instead. In-memory only — nothing is
/// persisted, and health/pause/ceilings are untouched.
async fn reset_usage_endpoint(State(state): State<AppState>) -> Response {
    let accounts = state.pool.reset_usage();
    tracing::info!(accounts, "usage force-reset to cold (operator command)");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "ok": true, "accounts": accounts }).to_string(),
    )
        .into_response()
}

/// `POST /llmux/scheduler-mode` — flip the selection algorithm live (TUI `S`;
/// persisted to config `scheduler.mode`).
async fn scheduler_mode_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<SchedulerModeRequest>,
) -> Response {
    match state.set_scheduler_mode(body.mode) {
        Ok(mode) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({ "ok": true, "mode": mode.label() }).to_string(),
        )
            .into_response(),
        Err(err) => relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config write failed: {err}"),
        ),
    }
}

#[derive(serde::Deserialize)]
struct EventsRequest {
    /// REMOVE directive: `{ "remove": "<id>" }` deletes the banner with that
    /// `id` (idempotent). When present, the upsert fields below are ignored.
    #[serde(default)]
    remove: Option<String>,
    /// Stable identifier / upsert key (required to upsert).
    #[serde(default)]
    id: Option<String>,
    /// Window start — RFC3339-with-offset or compact `YYYYMMDDHHMM` (local).
    #[serde(default)]
    from: Option<String>,
    /// Window end — same two forms as `from`; must parse strictly after it.
    #[serde(default)]
    to: Option<String>,
    /// Rendered banner text.
    #[serde(default)]
    content: Option<String>,
}

/// Build the shared `{ "ok": true, "events": [...] }` 200 response.
fn ok_events(events: Vec<crate::config::EventBanner>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "ok": true, "events": events }).to_string(),
    )
        .into_response()
}

/// `POST /llmux/events` — upsert or remove ONE top-of-dashboard event banner
/// (config `events`). The banner is display-only; this is the operator API that
/// replaces hand-editing `events` in `~/.config/llmux.json`. Body conventions
/// (matching the flat POST+JSON idiom of the other `/llmux/*` mutations):
///   - `{ "id", "from", "to", "content" }` → IDEMPOTENT upsert by `id` (same
///     id replaces that entry, a new id appends; an identical payload is a
///     no-op that still returns 200).
///   - `{ "remove": "<id>" }` → remove that banner by id (idempotent).
///
/// Both branches echo the stored list. Upsert validation: non-empty `id` and
/// `content`; `from`/`to` each parse via [`crate::event::parse_event_time`]
/// (the SAME parser the banner renders with, so an unparseable timestamp is
/// rejected here rather than silently rendering no banner later); `from` must
/// be strictly earlier than `to`. Persisted read-merge-write via
/// [`AppState::upsert_event`] / [`AppState::remove_event`].
async fn events_endpoint(
    State(state): State<AppState>,
    body: axum::extract::Json<EventsRequest>,
) -> Response {
    let req = body.0;

    // Remove branch: `{ "remove": "<id>" }`.
    if let Some(id) = req.remove {
        let id = id.trim();
        if id.is_empty() {
            return relay_error(StatusCode::BAD_REQUEST, "remove must be a non-empty id");
        }
        return match state.remove_event(id) {
            Ok(stored) => ok_events(stored),
            Err(crate::config::ConfigError::NoConfigDir) => relay_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "config persistence disabled; cannot remove event",
            ),
            Err(err) => relay_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("config write failed: {err}"),
            ),
        };
    }

    // Upsert branch: all four fields required.
    let (Some(id), Some(from), Some(to), Some(content)) = (req.id, req.from, req.to, req.content)
    else {
        return relay_error(
            StatusCode::BAD_REQUEST,
            "id, from, to and content are required to upsert an event \
             (or send {\"remove\": \"<id>\"} to remove one)",
        );
    };
    let (id, from, to, content) = (id.trim(), from.trim(), to.trim(), content.trim());
    if id.is_empty() {
        return relay_error(StatusCode::BAD_REQUEST, "id must be non-empty");
    }
    if content.is_empty() {
        return relay_error(StatusCode::BAD_REQUEST, "content must be non-empty");
    }
    let Some(from_at) = crate::event::parse_event_time(from) else {
        return relay_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "from must be an RFC3339 timestamp with an explicit offset or a \
                 compact YYYYMMDDHHMM local time; could not parse {from:?}"
            ),
        );
    };
    let Some(to_at) = crate::event::parse_event_time(to) else {
        return relay_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "to must be an RFC3339 timestamp with an explicit offset or a \
                 compact YYYYMMDDHHMM local time; could not parse {to:?}"
            ),
        );
    };
    if from_at >= to_at {
        return relay_error(StatusCode::BAD_REQUEST, "from must be earlier than to");
    }
    let banner = crate::config::EventBanner {
        id: id.to_string(),
        from: from.to_string(),
        to: to.to_string(),
        content: content.to_string(),
    };
    match state.upsert_event(banner) {
        Ok(stored) => ok_events(stored),
        Err(crate::config::ConfigError::NoConfigDir) => relay_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "config persistence disabled; cannot set event",
        ),
        Err(err) => relay_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("config write failed: {err}"),
        ),
    }
}

/// `POST /llmux/shutdown` — graceful server exit (same loopback /
/// proxy-api-key rules as every route, via the shared middleware). The 200
/// is delivered before the process exits: hyper's graceful shutdown stops
/// accepting new connections and completes in-flight responses first.
async fn shutdown(State(state): State<AppState>) -> Response {
    tracing::info!("shutdown requested via /llmux/shutdown");
    state.shutdown.notify_one();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"ok":true}"#.to_string(),
    )
        .into_response()
}

/// `POST /v1/oauth/token` relayed RAW to upstream — Claude Code's own token
/// refresh passes through untouched (no auth rewrite, no account lease;
/// intercepting client refreshes would cause token-rotation conflicts).
/// Like the Node reference, only `content-type` / `accept` / `user-agent`
/// travel upstream.
async fn oauth_token_relay(State(state): State<AppState>, req: axum::extract::Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, state.config.proxy.max_request_bytes).await {
        Ok(body) => body,
        Err(err) => {
            // Same ingress cap as the main forward path: an over-cap body is
            // rejected 413 before buffering; a genuine read failure stays 400.
            if forward::is_length_limit_error(&err) {
                return relay_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!(
                        "request body exceeds the {}-byte limit",
                        state.config.proxy.max_request_bytes
                    ),
                );
            }
            return relay_error(StatusCode::BAD_REQUEST, &format!("body read failed: {err}"));
        }
    };
    let path_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| parts.uri.path().to_string());
    let url = format!(
        "{}{}",
        state.config.upstream.trim_end_matches('/'),
        path_query
    );
    let mut builder = state.client.post(url);
    for name in [header::CONTENT_TYPE, header::ACCEPT, header::USER_AGENT] {
        if let Some(value) = parts.headers.get(&name) {
            builder = builder.header(name, value);
        }
    }
    if !body.is_empty() {
        builder = builder.body(body);
    }
    let upstream = match builder.send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!(error = %err, "oauth token relay failed");
            return relay_error(StatusCode::BAD_GATEWAY, "Upstream unreachable");
        }
    };
    let status = upstream.status();
    let mut headers = upstream.headers().clone();
    for name in ["transfer-encoding", "connection", "content-length"] {
        headers.remove(name);
    }
    let bytes = match upstream.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!(error = %err, "oauth token relay body failed");
            return relay_error(StatusCode::BAD_GATEWAY, "Upstream body read failed");
        }
    };
    let mut response = Response::new(axum::body::Body::from(bytes));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

fn relay_error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": "proxy_error", "message": message },
    });
    let mut response = Response::new(axum::body::Body::from(body.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// Catch-all: buffer, lease, rewrite, forward upstream, stream back
/// (see `forward`).
async fn forward_any(State(state): State<AppState>, req: axum::extract::Request) -> Response {
    forward::forward(&state, req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccountConfig, AccountCredential};
    use crate::scheduler::headers::{ParsedRateLimitHeaders, WindowReading};

    fn oauth_account(name: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Oauth {
                account_uuid: format!("uuid-{name}"),
                access_token: format!("at-{name}"),
                refresh_token: format!("rt-{name}"),
                expires_at_ms: 0,
                tier: None,
                last_refresh_ms: None,
            },
        }
    }

    fn apikey_account(name: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Apikey {
                api_key: format!("sk-ant-api03-{name}"),
            },
        }
    }

    #[test]
    fn scope_split_control_vs_data() {
        // Control plane = /llmux/* management + dashboard/status reads.
        assert!(is_control_plane("/llmux/keys"));
        assert!(is_control_plane("/llmux/keys/new"));
        assert!(is_control_plane("/llmux/dashboard"));
        assert!(is_control_plane("/llmux/status"));
        assert!(is_control_plane("/llmux/shutdown"));
        assert!(is_control_plane("/llmux/remove-account"));
        // Data plane = forwarding + the model catalog + the root ping.
        assert!(!is_control_plane("/v1/messages"));
        assert!(!is_control_plane("/models"));
        assert!(!is_control_plane("/llmux/models"));
        assert!(!is_control_plane("/"));
    }

    #[test]
    fn tenant_resolution_matrix() {
        use crate::proxy::keys::{KeyRegistry, Resolution, Tenant};
        // Legacy shared key + no issued keys: loopback keyless = local (data
        // only), remote keyless = denied, legacy key = admin from anywhere.
        let mut config = Config::default();
        config.proxy.api_key = Some("lm-secret".into());
        let reg = KeyRegistry::from_config(&config);
        assert_eq!(
            reg.resolve(None, true),
            Resolution::Allowed(Tenant::local())
        );
        assert_eq!(reg.resolve(None, false), Resolution::Denied);
        assert_eq!(reg.resolve(Some("wrong"), false), Resolution::Denied);
        match reg.resolve(Some("lm-secret"), false) {
            Resolution::Allowed(t) => assert!(t.admin && t.id == "legacy"),
            other => panic!("expected legacy admin, got {other:?}"),
        }
        // Keyless remote stays denied even with NO key configured at all —
        // keyless is loopback-only (issue #22 P0-B fix; old `None => true`
        // behavior is gone deliberately).
        let reg = KeyRegistry::from_config(&Config::default());
        assert_eq!(reg.resolve(None, false), Resolution::Denied);
    }

    fn params() -> SelectParams {
        SelectParams {
            five_hour_max: 0.90,
            seven_day_max: 0.99,
            fable_weekly_max: 0.98,
            mode: crate::config::SchedulerMode::Default,
            usage_max_age: Duration::from_secs(600),
        }
    }

    #[test]
    fn next_request_id_is_one_based_and_ascending() {
        let config = Config {
            accounts: vec![oauth_account("a")],
            ..Default::default()
        };
        let pool = AccountPool::new(&config.accounts);
        let state = AppState::new(config, pool, None, None).expect("state");
        // The first request must be id 1, not 0: the codex trace, the request
        // log, and the dashboard feed all key off this id, and a 0 surfaced as
        // `"id":0` on every trace line in a single-request session.
        assert_eq!(state.next_request_id(), 1, "first activity id is 1, not 0");
        assert_eq!(state.next_request_id(), 2);
        assert_eq!(state.next_request_id(), 3);
    }

    #[test]
    fn status_json_shape_covers_name_type_status_windows_and_totals() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let pool = AccountPool::new(&[oauth_account("a"), apikey_account("k")]);
        pool.evaluate(None, &params(), now);
        pool.record_headers(
            &AccountId("a".into()),
            &ParsedRateLimitHeaders {
                five_hour: Some(WindowReading {
                    utilization: 0.42,
                    resets_at: now + Duration::from_secs(3600),
                }),
                seven_day: Some(WindowReading {
                    utilization: 0.10,
                    resets_at: now + Duration::from_secs(86_400),
                }),
                ..Default::default()
            },
            now,
        );
        pool.record_429(&AccountId("k".into()), Some(Duration::from_secs(120)), now);
        let totals = UsageTotals::default();
        totals.record(&AccountId("a".into()), 3, 100, 50);

        let meta = ServerMeta {
            pid: 4321,
            uptime_secs: 7980,
            port: 3456,
            email_anonymous: false,
        };
        let doc = status_json(&pool.snapshot(), &totals, &params(), now, &meta);

        assert_eq!(doc["current"], "a");
        assert!(doc["version"]
            .as_str()
            .expect("version string")
            .starts_with("llmux "));
        assert_eq!(doc["pid"], 4321);
        assert_eq!(doc["uptime_secs"], 7980);
        assert_eq!(doc["port"], 3456);
        assert_eq!(doc["email_anonymous"], false, "live setting surfaced (E2)");
        let accounts = doc["accounts"].as_array().expect("accounts array");
        assert_eq!(accounts.len(), 2);

        let a = &accounts[0];
        assert_eq!(a["name"], "a");
        assert_eq!(a["type"], "oauth");
        assert_eq!(a["group"], "claude");
        assert_eq!(a["status"], "active");
        assert_eq!(a["order"], 1);
        assert_eq!(a["blocked"], serde_json::Value::Null);
        assert!((a["five_hour"]["utilization"].as_f64().expect("util") - 0.42).abs() < 1e-9);
        assert_eq!(a["five_hour"]["resets_at"], 1_000_000 + 3600);
        assert_eq!(a["five_hour"]["resets_in_secs"], 3600);
        assert_eq!(a["seven_day"]["resets_in_secs"], 86_400);
        assert_eq!(a["totals"]["requests"], 3);
        assert_eq!(a["totals"]["input_tokens"], 100);
        assert_eq!(a["totals"]["output_tokens"], 50);
        assert_eq!(a["in_flight"], 0);

        let k = &accounts[1];
        assert_eq!(k["type"], "apikey");
        assert_eq!(k["group"], "claude");
        assert_eq!(k["status"], "cooldown");
        assert_eq!(k["order"], 2);
        assert_eq!(k["blocked"], "cooldown 2m00s");
        assert_eq!(k["cooldown_until"], 1_000_000 + 120);
        assert_eq!(
            k["five_hour"],
            serde_json::Value::Null,
            "cold window is null"
        );
        assert_eq!(k["totals"]["requests"], 0);
    }

    #[test]
    fn status_json_surfaces_fable_weekly_and_scoped_limits() {
        // End-to-end proof that a model-scoped (`limits[]` weekly_scoped) window
        // reaches the `/llmux/status` JSON: parse the verbatim live DEV1 usage
        // body, record it, and assert `fable_weekly` + `scoped_limits` are
        // present with the fixture's 100% / critical / active Fable reading —
        // while `five_hour`/`seven_day` stay exactly as before. A second account
        // with no scoped rows proves the null/empty shape.
        //
        // `now` sits BEFORE the fixture's Fable reset (2026-07-03T21:59:59Z =
        // epoch 1_783_115_999) so `effective_utilization` keeps the 100% value.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_783_000_000);
        let pool = AccountPool::new(&[oauth_account("dev1"), oauth_account("plain")]);
        pool.evaluate(None, &params(), now);
        let usage = crate::scheduler::usage::parse_usage_body(
            crate::scheduler::usage::DEV1_USAGE_FIXTURE.as_bytes(),
        )
        .expect("fixture parses");
        pool.record_usage(&AccountId("dev1".into()), &usage, now);

        let totals = UsageTotals::default();
        let meta = ServerMeta {
            pid: 1,
            uptime_secs: 1,
            port: 3456,
            email_anonymous: false,
        };
        let doc = status_json(&pool.snapshot(), &totals, &params(), now, &meta);
        let accounts = doc["accounts"].as_array().expect("accounts array");
        let dev1 = accounts
            .iter()
            .find(|a| a["name"] == "dev1")
            .expect("dev1 account");
        let plain = accounts
            .iter()
            .find(|a| a["name"] == "plain")
            .expect("plain account");

        // fable_weekly: the "Fable" scoped window, surfaced without scope_label.
        let fable = &dev1["fable_weekly"];
        assert!(
            (fable["utilization"].as_f64().expect("util") - 1.0).abs() < 1e-9,
            "Fable at percent 100 → fraction 1.0"
        );
        assert_eq!(fable["severity"], "critical");
        assert_eq!(fable["is_active"], true);
        assert_eq!(fable["resets_at"], 1_783_115_999u64);
        assert_eq!(fable["resets_in_secs"], 115_999u64);
        assert!(
            fable.get("scope_label").is_none(),
            "fable_weekly omits scope_label (it IS the Fable entry)"
        );

        // scoped_limits: the generic list carries the same entry WITH its label.
        let scoped = dev1["scoped_limits"]
            .as_array()
            .expect("scoped_limits array");
        assert_eq!(scoped.len(), 1, "only the one weekly_scoped row");
        assert_eq!(scoped[0]["scope_label"], "Fable");
        assert!((scoped[0]["utilization"].as_f64().expect("util") - 1.0).abs() < 1e-9);
        assert_eq!(scoped[0]["severity"], "critical");
        assert_eq!(scoped[0]["is_active"], true);

        // Legacy windows stay EXACTLY as today: five_hour 0%, seven_day 58%.
        assert_eq!(dev1["five_hour"]["utilization"], 0.0);
        assert!(
            (dev1["seven_day"]["utilization"].as_f64().expect("util") - 0.58).abs() < 1e-9,
            "seven_day legacy parse unchanged"
        );

        // Account with no scoped rows → fable_weekly null, scoped_limits empty.
        assert_eq!(plain["fable_weekly"], serde_json::Value::Null);
        assert!(plain["scoped_limits"]
            .as_array()
            .expect("empty scoped array")
            .is_empty());
    }

    #[test]
    fn status_json_tags_codex_accounts_with_codex_group() {
        // A codex credential lands in the "codex" backend group; oauth/apikey
        // land in "claude" (covered by the shape test). The group field is what
        // `accounts --json` renders as the dashboard's group column.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let pool = AccountPool::new(&[codex_account("c", "acct-1")]);
        pool.evaluate(None, &params(), now);
        let meta = ServerMeta {
            pid: 1,
            uptime_secs: 0,
            port: 3456,
            email_anonymous: false,
        };
        let doc = status_json(
            &pool.snapshot(),
            &UsageTotals::default(),
            &params(),
            now,
            &meta,
        );
        let accounts = doc["accounts"].as_array().expect("accounts array");
        assert_eq!(accounts[0]["name"], "c");
        assert_eq!(accounts[0]["type"], "codex");
        assert_eq!(accounts[0]["group"], "codex");
    }

    #[test]
    fn status_json_carries_token_expiry_and_last_refresh() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000); // = 1_000_000_000 ms
        let mut account = oauth_account("a");
        if let AccountCredential::Oauth {
            expires_at_ms,
            last_refresh_ms,
            ..
        } = &mut account.credential
        {
            *expires_at_ms = 1_003_600_000; // 1h from `now`
            *last_refresh_ms = Some(999_820_000); // 3m before `now`
        }
        let pool = AccountPool::new(&[account, apikey_account("k")]);
        pool.evaluate(None, &params(), now);
        let meta = ServerMeta {
            pid: 1,
            uptime_secs: 0,
            port: 0,
            email_anonymous: false,
        };
        let doc = status_json(
            &pool.snapshot(),
            &UsageTotals::default(),
            &params(),
            now,
            &meta,
        );
        let accounts = doc["accounts"].as_array().expect("accounts");
        let a = accounts.iter().find(|a| a["name"] == "a").expect("a");
        assert_eq!(a["token_expires_at_ms"], 1_003_600_000u64);
        assert_eq!(a["last_refresh_ms"], 999_820_000u64);
        let k = accounts.iter().find(|a| a["name"] == "k").expect("k");
        assert_eq!(
            k["token_expires_at_ms"],
            serde_json::Value::Null,
            "apikey has no token"
        );
        assert_eq!(k["last_refresh_ms"], serde_json::Value::Null);
    }

    #[test]
    fn status_json_marks_auth_failed_accounts() {
        let now = SystemTime::now();
        let pool = AccountPool::new(&[oauth_account("a")]);
        pool.record_auth_failure(&AccountId("a".into()));
        let meta = ServerMeta {
            pid: 1,
            uptime_secs: 0,
            port: 0,
            email_anonymous: false,
        };
        let doc = status_json(
            &pool.snapshot(),
            &UsageTotals::default(),
            &params(),
            now,
            &meta,
        );
        assert_eq!(doc["accounts"][0]["status"], "auth_failed");
        assert_eq!(doc["accounts"][0]["blocked"], "auth failed");
        assert_eq!(doc["current"], serde_json::Value::Null);
    }

    /// B1: the `accounts` array is emitted in scheduler preference order —
    /// current first, then eligible accounts by rank (soonest 7d reset),
    /// then ineligible accounts with their blocking reason.
    #[test]
    fn status_json_orders_accounts_by_selection_preference() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let pool = AccountPool::new(&[
            oauth_account("parked"),
            oauth_account("later"),
            oauth_account("soon"),
            oauth_account("cur"),
        ]);
        let window = |resets_in: u64| {
            Some(WindowReading {
                utilization: 0.5,
                resets_at: now + Duration::from_secs(resets_in),
            })
        };
        pool.record_headers(
            &AccountId("later".into()),
            &ParsedRateLimitHeaders {
                seven_day: window(48 * 3600),
                ..Default::default()
            },
            now,
        );
        pool.record_headers(
            &AccountId("soon".into()),
            &ParsedRateLimitHeaders {
                seven_day: window(12 * 3600),
                ..Default::default()
            },
            now,
        );
        pool.record_429(
            &AccountId("parked".into()),
            Some(Duration::from_secs(60)),
            now,
        );
        pool.switch_to(&AccountId("cur".into()), &params(), now)
            .expect("test switch");

        let doc = status_json(
            &pool.snapshot(),
            &UsageTotals::default(),
            &params(),
            now,
            &ServerMeta {
                pid: 1,
                uptime_secs: 0,
                port: 0,
                email_anonymous: false,
            },
        );
        let names: Vec<&str> = doc["accounts"]
            .as_array()
            .expect("accounts array")
            .iter()
            .map(|a| a["name"].as_str().expect("name"))
            .collect();
        assert_eq!(names, vec!["cur", "soon", "later", "parked"]);
        let orders: Vec<u64> = doc["accounts"]
            .as_array()
            .expect("accounts array")
            .iter()
            .map(|a| a["order"].as_u64().expect("order"))
            .collect();
        assert_eq!(orders, vec![1, 2, 3, 4]);
        assert_eq!(doc["accounts"][3]["blocked"], "cooldown 1m00s");
    }

    #[test]
    fn usage_totals_accumulate_and_default_to_zero() {
        let totals = UsageTotals::default();
        let a = AccountId("a".into());
        assert_eq!(totals.get(&a), AccountTotals::default());
        totals.record(&a, 1, 10, 5);
        totals.record(&a, 1, 2, 3);
        assert_eq!(
            totals.get(&a),
            AccountTotals {
                requests: 2,
                input_tokens: 12,
                output_tokens: 8,
            }
        );
    }

    // --- account add/remove endpoints (issue #3) ---------------------------

    /// Self-cleaning unique temp dir (no tempfile dev-dependency), mirroring
    /// the pattern in `config::tests` / `forward::tests`.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "llmux-server-test-{}-{}",
                std::process::id(),
                ulid::Ulid::new()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Build an `AppState` whose config is persisted at `config_path` (seeded
    /// with `accounts`), so the add/remove handlers exercise the real
    /// read-merge-write path against a throwaway file — never the user config.
    fn endpoint_state(config_path: &std::path::Path, accounts: Vec<AccountConfig>) -> AppState {
        let config = Config {
            accounts,
            ..Default::default()
        };
        crate::config::save_path(config_path, &config).expect("seed config");
        let pool = AccountPool::new(&config.accounts);
        let mut state = AppState::new(config, pool, None, None).expect("state");
        state.config_path = Some(config_path.to_path_buf());
        state
            .pool
            .evaluate(None, &state.select_params(), SystemTime::now());
        state
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    // ---- C10: POST /llmux/grok ----
    #[tokio::test]
    async fn c10_grok_config_partial_update_persists_and_validates() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("keep")]);

        // Partial update: model + effort (superset value "none" accepted).
        let response = grok_config_endpoint(
            State(state.clone()),
            axum::extract::Json(GrokConfigRequest {
                default_model: Some("grok-4.3".into()),
                reasoning_effort: Some("none".into()),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["default_model"], "grok-4.3");
        assert_eq!(body["reasoning_effort"], "none");
        assert_eq!(body["persisted"], true, "config write reported (C10)");
        // Applied live + persisted.
        assert_eq!(state.grok.model(), "grok-4.3");
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert_eq!(on_disk.grok.default_model, "grok-4.3");
        assert_eq!(on_disk.grok.reasoning_effort.as_deref(), Some("none"));

        // Omitted field keeps current value; "unset" clears effort.
        let response = grok_config_endpoint(
            State(state.clone()),
            axum::extract::Json(GrokConfigRequest {
                default_model: None,
                reasoning_effort: Some("unset".into()),
            }),
        )
        .await;
        let body = response_json(response).await;
        assert_eq!(body["default_model"], "grok-4.3", "omitted model kept");
        assert!(body["reasoning_effort"].is_null(), "unset clears");

        // Garbage effort → 400, nothing changed.
        let response = grok_config_endpoint(
            State(state.clone()),
            axum::extract::Json(GrokConfigRequest {
                default_model: None,
                reasoning_effort: Some("turbo".into()),
            }),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "turbo not in grok superset"
        );
        assert_eq!(state.grok.model(), "grok-4.3");
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert_eq!(on_disk.grok.default_model, "grok-4.3");
        assert_eq!(
            on_disk.grok.reasoning_effort, None,
            "rejected effort leaves state unchanged"
        );

        // "xhigh" IS in the grok superset → accepted and persisted.
        let response = grok_config_endpoint(
            State(state.clone()),
            axum::extract::Json(GrokConfigRequest {
                default_model: None,
                reasoning_effort: Some("xhigh".into()),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["default_model"], "grok-4.3", "omitted model kept");
        assert_eq!(body["reasoning_effort"], "xhigh");
        assert_eq!(body["persisted"], true, "config write reported (C10)");
        assert_eq!(state.grok.model(), "grok-4.3");
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert_eq!(on_disk.grok.default_model, "grok-4.3");
        assert_eq!(on_disk.grok.reasoning_effort.as_deref(), Some("xhigh"));
    }

    // ---- GET /models and /llmux/models ----
    #[tokio::test]
    async fn models_endpoint_returns_catalog_with_live_grok_pin_alias() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("keep")]);

        // Pin grok to 4.3 so the family alias must follow the live pin.
        let mut shape = state.grok.shape();
        shape.model = "grok-4.3".into();
        state.grok.set_shape(shape);

        let response = models_endpoint(State(state.clone())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let models = body["models"].as_array().expect("models array");
        // 26 curated (8 claude + 6 codex + 2 grok + 10 openrouter) + 1
        // synthesized (grok-4.3 is out-of-catalog now).
        assert_eq!(models.len(), 27);

        let by_id = |id: &str| {
            models
                .iter()
                .find(|m| m["id"] == id)
                .unwrap_or_else(|| panic!("{id} present"))
        };
        // The live pin (grok-4.3) carries the "grok" alias via a synthesized
        // row; the curated grok-4.6 / grok-4.5 do not.
        assert_eq!(by_id("grok-4.3")["aliases"], serde_json::json!(["grok"]));
        assert_eq!(by_id("grok-4.6")["aliases"], serde_json::json!([]));
        assert_eq!(by_id("grok-4.5")["aliases"], serde_json::json!([]));
        // Static codex alias and context survive serialization.
        assert_eq!(
            by_id("gpt-5.6-sol")["aliases"],
            serde_json::json!(["sol", "gpt-5.6"])
        );
        assert_eq!(by_id("gpt-5.6-sol")["max_context"], 372_000);
        // The codex `[1m]` opt-in rows ride the same serialization: 1M window,
        // no aliases (those stay on the base rows), base sol unchanged.
        for id in ["gpt-5.6-sol[1m]", "gpt-5.6-terra[1m]"] {
            assert_eq!(by_id(id)["max_context"], 1_000_000, "{id}");
            assert_eq!(by_id(id)["aliases"], serde_json::json!([]), "{id}");
        }
    }

    #[tokio::test]
    async fn models_routes_registered_and_v1_models_still_proxies() {
        // Drive the REAL router on an ephemeral loopback port. Zero accounts,
        // so the proxy fallback answers `/v1/models` with an error
        // synchronously (no upstream network call) — proving that path is NOT
        // intercepted by the catalog handler.
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, Vec::new());
        let app = router(state).into_make_service_with_connect_info::<SocketAddr>();

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        // Both catalog paths → 200 with byte-identical bodies.
        let r1 = client
            .get(format!("{base}/models"))
            .send()
            .await
            .expect("GET /models");
        assert_eq!(r1.status(), reqwest::StatusCode::OK);
        let b1 = r1.text().await.expect("body");
        let r2 = client
            .get(format!("{base}/llmux/models"))
            .send()
            .await
            .expect("GET /llmux/models");
        assert_eq!(r2.status(), reqwest::StatusCode::OK);
        let b2 = r2.text().await.expect("body");
        assert_eq!(b1, b2, "both paths serve identical catalog bodies");
        assert!(b1.contains("\"models\""), "catalog shape present");
        assert!(b1.contains("gpt-5.6-sol"), "curated ids present");

        // `/v1/models` is not intercepted: it reaches the proxy fallback, which
        // with no accounts returns an error — never the catalog shape.
        let rv = client
            .get(format!("{base}/v1/models"))
            .send()
            .await
            .expect("GET /v1/models");
        let bv = rv.text().await.expect("body");
        assert!(
            !bv.contains("gpt-5.6-sol"),
            "/v1/models must reach the fallback, not return the catalog"
        );
    }

    // ---- multi-tenant client keys (#22) ----

    /// The P1 receipt gate: issue → suspend → 401 → resume → 200 → revoke →
    /// 401, all against ONE running server process with NO restart — proving
    /// mutations reach the live auth gate through the registry, not just the
    /// config file. Also pins the two-axis scope split end-to-end and that the
    /// disk config mirrors every mutation (persist-then-swap).
    #[tokio::test]
    async fn client_key_lifecycle_bites_live_without_restart() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let mut state = endpoint_state(&path, Vec::new());
        // Give the config a legacy admin key (the local CLI's credential).
        state.config.proxy.api_key = Some("lm-admin".into());
        crate::config::save_path(&path, &state.config).expect("seed key");
        state.keys.reload(&state.config);
        let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let base = format!("http://{addr}");
        let client = reqwest::Client::new();

        // Scope split: keyless loopback reaches the DATA plane…
        let r = client
            .get(format!("{base}/llmux/models"))
            .send()
            .await
            .expect("send");
        assert_eq!(r.status().as_u16(), 200, "keyless loopback data plane");
        // …but NOT the control plane (network position is not privilege).
        let r = client
            .get(format!("{base}/llmux/keys"))
            .send()
            .await
            .expect("send");
        assert_eq!(r.status().as_u16(), 403, "keyless loopback control plane");

        // Admin (legacy key) issues a default-kind key.
        let r = client
            .post(format!("{base}/llmux/keys/new"))
            .header("x-api-key", "lm-admin")
            .json(&serde_json::json!({ "name": "pc-b", "email": "b@x.com" }))
            .send()
            .await
            .expect("send");
        assert_eq!(r.status().as_u16(), 200);
        let issued: serde_json::Value = r.json().await.expect("json");
        let secret = issued["key"].as_str().expect("plaintext once").to_string();
        let id = issued["id"].as_str().expect("id").to_string();
        assert!(secret.starts_with("lmk-"), "issued namespace is lmk-");

        // The issued key unlocks the data plane… (200 on the catalog route)
        let data = |key: &str| {
            client
                .get(format!("{base}/llmux/models"))
                .header("x-api-key", key.to_string())
                .send()
        };
        assert_eq!(data(&secret).await.expect("send").status().as_u16(), 200);
        // …but its default kind is refused on the control plane.
        let r = client
            .get(format!("{base}/llmux/keys"))
            .header("x-api-key", &secret)
            .send()
            .await
            .expect("send");
        assert_eq!(r.status().as_u16(), 403, "default kind is data-plane only");

        // Suspend — SAME process, no restart — next request must 401.
        let r = client
            .post(format!("{base}/llmux/keys/suspend"))
            .header("x-api-key", "lm-admin")
            .json(&serde_json::json!({ "id": id, "suspended": true }))
            .send()
            .await
            .expect("send");
        assert_eq!(r.status().as_u16(), 200);
        let r = data(&secret).await.expect("send");
        assert_eq!(r.status().as_u16(), 401, "suspend bites live");
        let body = r.text().await.expect("body");
        assert!(
            body.contains("suspended"),
            "explicit suspended message: {body}"
        );

        // Resume — next request passes again.
        let r = client
            .post(format!("{base}/llmux/keys/suspend"))
            .header("x-api-key", "lm-admin")
            .json(&serde_json::json!({ "id": id, "suspended": false }))
            .send()
            .await
            .expect("send");
        assert_eq!(r.status().as_u16(), 200);
        assert_eq!(
            data(&secret).await.expect("send").status().as_u16(),
            200,
            "resume bites live"
        );

        // Revoke — 401 forever, but the row (attribution metadata) survives
        // on disk with its name/email (soft-delete).
        let r = client
            .post(format!("{base}/llmux/keys/remove"))
            .header("x-api-key", "lm-admin")
            .json(&serde_json::json!({ "id": id }))
            .send()
            .await
            .expect("send");
        assert_eq!(r.status().as_u16(), 200);
        assert_eq!(
            data(&secret).await.expect("send").status().as_u16(),
            401,
            "revoke bites live"
        );
        let on_disk = crate::config::load_path(&path).expect("reload");
        let row = on_disk
            .client_keys
            .iter()
            .find(|k| k.id == id)
            .expect("soft-deleted row preserved");
        assert!(row.revoked_at_ms.is_some());
        assert_eq!(row.name, "pc-b");
        assert_eq!(row.email.as_deref(), Some("b@x.com"));
    }

    /// Secret-never-returned sweep (D1): after issuance, the plaintext must
    /// not appear on ANY response surface — the keys list, the dashboard
    /// document, or the persisted config (which stores only the digest).
    #[tokio::test]
    async fn client_key_secret_never_returned_after_issuance() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, Vec::new());
        let (row, secret) = state
            .issue_client_key("pc-c", None, crate::config::ClientKeyKind::Default)
            .expect("issue");
        // List endpoint: no secret, no digest.
        let response = keys_list_endpoint(State(state.clone())).await;
        let body = response_json(response).await.to_string();
        assert!(!body.contains(&secret), "list must not return the secret");
        assert!(!body.contains("digest"), "list must not return digests");
        // Dashboard document (the fan-out surface: TUI/attach/islands).
        let doc = crate::dashboard::build_doc(&state, SystemTime::now());
        let doc_json = serde_json::to_string(&doc).expect("doc json");
        assert!(
            !doc_json.contains(&secret),
            "dashboard must not carry the secret"
        );
        assert!(
            doc_json.contains(&row.id),
            "dashboard lists the key metadata"
        );
        // On disk: digest only.
        let on_disk = std::fs::read_to_string(&path).expect("config");
        assert!(!on_disk.contains(&secret), "config stores no plaintext");
        assert!(on_disk.contains("sha256:"), "config stores the digest");
        // Rotation returns a NEW secret exactly once and the old stops resolving.
        let (_, rotated) = state.rotate_client_key(&row.id).expect("rotate");
        use crate::proxy::keys::Resolution;
        assert!(matches!(
            state.keys.resolve(Some(&rotated), false),
            Resolution::Allowed(_)
        ));
        assert_eq!(state.keys.resolve(Some(&secret), false), Resolution::Denied);
    }

    /// Last-active-admin guard: with no legacy key configured, the final
    /// admin client key can be neither suspended nor revoked (fail-open /
    /// self-lockout guard); a second admin unblocks the first.
    #[tokio::test]
    async fn last_admin_credential_cannot_be_disabled() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, Vec::new());
        assert!(
            state.config.proxy.api_key.is_none(),
            "no legacy admin in this fixture"
        );
        let (a, _) = state
            .issue_client_key("admin-a", None, crate::config::ClientKeyKind::Admin)
            .expect("issue a");
        assert!(matches!(
            state.set_client_key_suspended(&a.id, true),
            Err(KeyAdminError::LastAdmin)
        ));
        assert!(matches!(
            state.revoke_client_key(&a.id),
            Err(KeyAdminError::LastAdmin)
        ));
        let (b, _) = state
            .issue_client_key("admin-b", None, crate::config::ClientKeyKind::Admin)
            .expect("issue b");
        state
            .set_client_key_suspended(&a.id, true)
            .expect("now suspendable");
        // …and the guard moves to the remaining admin.
        assert!(matches!(
            state.revoke_client_key(&b.id),
            Err(KeyAdminError::LastAdmin)
        ));
    }

    /// Duplicate names among ACTIVE keys are refused (CLI resolves names to
    /// ids, so names must be unambiguous); a revoked key frees its name.
    #[tokio::test]
    async fn client_key_names_unique_among_active() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, Vec::new());
        // A legacy admin exists so the guard doesn't interfere.
        crate::config::update_path(&path, |c| {
            c.proxy.api_key = Some("lm-admin".into());
        })
        .expect("seed");
        let (first, _) = state
            .issue_client_key("pc-a", None, crate::config::ClientKeyKind::Default)
            .expect("issue");
        assert!(matches!(
            state.issue_client_key("pc-a", None, crate::config::ClientKeyKind::Default),
            Err(KeyAdminError::NameTaken(_))
        ));
        state.revoke_client_key(&first.id).expect("revoke");
        state
            .issue_client_key("pc-a", None, crate::config::ClientKeyKind::Default)
            .expect("revoked name is reusable");
    }

    // ---- C11: login status carries device-flow verification fields ----
    #[tokio::test]
    async fn c11_login_status_carries_verification_uri_while_pending() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("keep")]);
        assert!(state.logins.begin("st-1".to_string()));
        state.logins.set_verification(
            "st-1",
            "https://x.ai/device?code=ABCD".to_string(),
            "ABCD-EFGH".to_string(),
        );
        let response = login_status_endpoint(
            State(state.clone()),
            axum::extract::Query(LoginStatusQuery {
                state: "st-1".to_string(),
            }),
        )
        .await;
        let body = response_json(response).await;
        assert_eq!(body["phase"], "pending");
        assert_eq!(body["verification_uri"], "https://x.ai/device?code=ABCD");
        assert_eq!(body["user_code"], "ABCD-EFGH");
        // Non-grok pending logins (no verification published) stay minimal.
        assert!(!state.logins.begin("st-1b".to_string()), "single slot");
    }

    #[tokio::test]
    async fn add_account_persists_apikey_masks_response_and_reloads_pool() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("keep")]);

        let response = add_account_endpoint(
            State(state.clone()),
            axum::extract::Json(AddAccountRequest {
                name: Some("api-mine".into()),
                api_key: "sk-ant-api03-SUPERSECRETVALUE".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["name"], "api-mine");
        assert_eq!(body["type"], "apikey");
        assert_eq!(body["added"], true);
        // Response echoes ONLY a masked key — never the raw secret.
        assert_eq!(body["api_key_masked"], "sk-ant-api03-SU...");
        let masked = body["api_key_masked"].as_str().expect("masked");
        assert!(
            !masked.contains("SUPERSECRET"),
            "raw key must not leak: {masked}"
        );

        // Persisted via read-merge-write: the seeded account is preserved and
        // the new apikey account is on disk with the real (unmasked) key.
        let on_disk = crate::config::load_path(&path).expect("reload");
        let names: Vec<&str> = on_disk.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["keep", "api-mine"]);
        match &on_disk.accounts[1].credential {
            AccountCredential::Apikey { api_key } => {
                assert_eq!(api_key, "sk-ant-api03-SUPERSECRETVALUE")
            }
            other => panic!("expected apikey, got {other:?}"),
        }

        // Live pool reflects the add with no restart.
        let live: Vec<String> = state
            .pool
            .snapshot()
            .accounts
            .iter()
            .map(|a| a.id.0.clone())
            .collect();
        assert!(
            live.contains(&"api-mine".to_string()),
            "live pool reloaded: {live:?}"
        );
    }

    #[tokio::test]
    async fn add_account_assigns_default_name_when_omitted() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![apikey_account("api-1")]);

        let response = add_account_endpoint(
            State(state),
            axum::extract::Json(AddAccountRequest {
                name: None,
                api_key: "sk-ant-api03-ANOTHERONE".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        // Next free `api-N`, computed off the fresh on-disk state.
        assert_eq!(body["name"], "api-2");
    }

    #[tokio::test]
    async fn add_account_rejects_empty_key() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![]);

        let response = add_account_endpoint(
            State(state),
            axum::extract::Json(AddAccountRequest {
                name: None,
                api_key: "   ".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        // Nothing written.
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert!(on_disk.accounts.is_empty());
    }

    #[tokio::test]
    async fn remove_account_preserves_others_and_reloads_pool() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(
            &path,
            vec![oauth_account("a"), apikey_account("b"), oauth_account("c")],
        );

        let response = remove_account_endpoint(
            State(state.clone()),
            axum::extract::Json(RemoveAccountRequest {
                name: "b".into(),
                confirm: true,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["removed"], true);

        // Read-merge-write removal preserves the other accounts.
        let on_disk = crate::config::load_path(&path).expect("reload");
        let names: Vec<&str> = on_disk.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["a", "c"]);

        // Live pool dropped the removed account with no restart.
        let live: Vec<String> = state
            .pool
            .snapshot()
            .accounts
            .iter()
            .map(|a| a.id.0.clone())
            .collect();
        assert_eq!(live, vec!["a".to_string(), "c".to_string()]);
    }

    #[tokio::test]
    async fn remove_account_requires_confirmation() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("a")]);

        let response = remove_account_endpoint(
            State(state),
            axum::extract::Json(RemoveAccountRequest {
                name: "a".into(),
                confirm: false,
            }),
        )
        .await;
        // No confirm → 400, and the account is left untouched (never silent).
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert_eq!(on_disk.accounts.len(), 1);
        assert_eq!(on_disk.accounts[0].name, "a");
    }

    #[tokio::test]
    async fn remove_account_unknown_name_is_404() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("a")]);

        let response = remove_account_endpoint(
            State(state),
            axum::extract::Json(RemoveAccountRequest {
                name: "ghost".into(),
                confirm: true,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // --- issue #4: inject an OAuth/Codex account from the dashboard ---------

    fn codex_account(name: &str, account_id: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            // A realistic single-token Bearer access token (no `:`/whitespace
            // inside) so the mask span covers the whole secret.
            credential: AccountCredential::Codex {
                account_id: account_id.to_string(),
                access_token: "Bearer eyJhbGciLONGSECRETACCESSTOKENPART".to_string(),
                refresh_token: format!("crt-{name}"),
                expires_at_ms: 0,
                last_refresh_ms: None,
            },
        }
    }

    /// An oauth account whose access token looks like a real Anthropic OAuth
    /// token so `mask_credentials` (which keys off `sk-ant-`) actually masks it.
    fn oauth_account_realistic(name: &str, uuid: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Oauth {
                account_uuid: uuid.to_string(),
                access_token: "sk-ant-oat01-SUPERSECRETACCESSTOKENVALUE".to_string(),
                refresh_token: "sk-ant-ort01-SECRETREFRESH".to_string(),
                expires_at_ms: 1_700_000_000_000,
                tier: Some("max".into()),
                last_refresh_ms: Some(1_699_990_000_000),
            },
        }
    }

    #[tokio::test]
    async fn inject_oauth_account_persists_masks_and_reloads_pool() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        // Seed an existing account that MUST survive the inject.
        let state = endpoint_state(&path, vec![apikey_account("keep")]);

        let injected = oauth_account_realistic("claude:me@example.com", "uuid-new");
        let response = inject_account_endpoint(
            State(state.clone()),
            axum::extract::Json(InjectAccountRequest {
                account: injected.clone(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["name"], "claude:me@example.com");
        assert_eq!(body["type"], "oauth");
        assert_eq!(body["added"], true);
        // The access token is echoed ONLY masked — never the raw secret.
        let masked = body["access_token_masked"].as_str().expect("masked");
        assert_eq!(masked, "sk-ant-oat01-SU...");
        assert!(
            !masked.contains("SUPERSECRETACCESSTOKENVALUE"),
            "raw token leaked: {masked}"
        );

        // Read-merge-write: the seeded account is preserved and the oauth
        // credential is on disk with its real (unmasked) tokens.
        let on_disk = crate::config::load_path(&path).expect("reload");
        let names: Vec<&str> = on_disk.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["keep", "claude:me@example.com"]);
        match &on_disk.accounts[1].credential {
            AccountCredential::Oauth {
                account_uuid,
                access_token,
                tier,
                ..
            } => {
                assert_eq!(account_uuid, "uuid-new");
                assert_eq!(access_token, "sk-ant-oat01-SUPERSECRETACCESSTOKENVALUE");
                assert_eq!(tier.as_deref(), Some("max"));
            }
            other => panic!("expected oauth, got {other:?}"),
        }

        // Live pool reflects the inject with no restart.
        let live: Vec<String> = state
            .pool
            .snapshot()
            .accounts
            .iter()
            .map(|a| a.id.0.clone())
            .collect();
        assert!(
            live.contains(&"claude:me@example.com".to_string()),
            "live pool reloaded: {live:?}"
        );
    }

    #[tokio::test]
    async fn inject_codex_account_persists_and_masks() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("a")]);

        let response = inject_account_endpoint(
            State(state.clone()),
            axum::extract::Json(InjectAccountRequest {
                account: codex_account("codex:me@example.com", "chatgpt-acct-1"),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["type"], "codex");
        assert_eq!(body["name"], "codex:me@example.com");
        // `Bearer …` is masked to the first 20 chars + `...`.
        let masked = body["access_token_masked"].as_str().expect("masked");
        assert_eq!(masked, "Bearer eyJhbGciLONGS...");
        assert!(
            !masked.contains("SECRETACCESSTOKENPART"),
            "raw token leaked: {masked}"
        );

        let on_disk = crate::config::load_path(&path).expect("reload");
        // Other account preserved; codex added.
        let names: Vec<&str> = on_disk.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["a", "codex:me@example.com"]);
        assert!(matches!(
            on_disk.accounts[1].credential,
            AccountCredential::Codex { .. }
        ));
    }

    #[tokio::test]
    async fn inject_oauth_relogin_updates_by_uuid_not_duplicates() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        // Existing oauth account with the SAME uuid the re-login will carry.
        let state = endpoint_state(&path, vec![oauth_account_realistic("claude:old", "uuid-x")]);

        // Re-login: same uuid, new name (profile email changed) — must UPDATE
        // the existing entry, not add a second one (dedup by account_uuid).
        let mut relogin = oauth_account_realistic("claude:new", "uuid-x");
        if let AccountCredential::Oauth { access_token, .. } = &mut relogin.credential {
            *access_token = "sk-ant-oat01-ROTATEDTOKENVALUE".to_string();
        }
        let response = inject_account_endpoint(
            State(state.clone()),
            axum::extract::Json(InjectAccountRequest { account: relogin }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["added"], false, "re-login updates, never adds");

        let on_disk = crate::config::load_path(&path).expect("reload");
        assert_eq!(on_disk.accounts.len(), 1, "no duplicate from re-login");
        assert_eq!(on_disk.accounts[0].name, "claude:new");
    }

    #[tokio::test]
    async fn inject_rejects_apikey_credential() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![]);

        // An apikey credential is the wrong endpoint (/add-account handles it,
        // no browser needed) — inject must refuse with a 400 and write nothing.
        let response = inject_account_endpoint(
            State(state),
            axum::extract::Json(InjectAccountRequest {
                account: apikey_account("api-1"),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert!(on_disk.accounts.is_empty(), "nothing persisted");
    }

    #[tokio::test]
    async fn inject_rejects_empty_name() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![]);

        let mut acct = oauth_account_realistic("", "uuid-z");
        acct.name = "   ".into();
        let response = inject_account_endpoint(
            State(state),
            axum::extract::Json(InjectAccountRequest { account: acct }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // --- email_anonymous: status surface + settings endpoint ----------------

    #[test]
    fn status_json_reports_email_anonymous_but_keeps_names_real() {
        // T1 (SSOT): the flag is surfaced, but the API data itself is NEVER
        // masked — islands' OFF state and its mosaic both need real names.
        let now = SystemTime::now();
        let pool = AccountPool::new(&[oauth_account("me@real-domain.com")]);
        pool.evaluate(None, &params(), now);
        let doc = status_json(
            &pool.snapshot(),
            &UsageTotals::default(),
            &params(),
            now,
            &ServerMeta {
                pid: 1,
                uptime_secs: 0,
                port: 0,
                email_anonymous: true,
            },
        );
        assert_eq!(doc["email_anonymous"], true);
        assert_eq!(
            doc["accounts"][0]["name"], "me@real-domain.com",
            "API names stay real; masking is a display concern"
        );
    }

    #[tokio::test]
    async fn settings_ack_round_trips_typed() {
        // The TUI parses the endpoint's body as a TYPED SettingsAck and
        // refuses to act on anything else — pin the wire contract here so
        // the two sides cannot drift silently. An empty `{}` must FAIL the
        // typed parse (unverified, never a confirmed no-change).
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("a")]);
        let response = settings_endpoint(
            State(state.clone()),
            axum::extract::Json(SettingsRequest {
                tui_effects: Some(false),
                raw_io_retention_days: Some(7),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let ack: SettingsAck = serde_json::from_slice(&bytes).expect("typed ack parses");
        assert!(ack.ok);
        assert_eq!(ack.applied, vec!["tui_effects"]);
        assert_eq!(ack.restart_required, vec!["raw_io_retention_days"]);
        assert!(
            serde_json::from_str::<SettingsAck>("{}").is_err(),
            "an empty object must not read as a verified ack"
        );
    }

    #[tokio::test]
    async fn apply_settings_validates_flips_live_and_reports_restart() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("a")]);

        // Invalid values are rejected BEFORE any write (atomic apply).
        for bad in [
            SettingsRequest {
                five_hour_max: Some(1.5),
                ..Default::default()
            },
            SettingsRequest {
                quota_display: Some("sideways".into()),
                ..Default::default()
            },
            SettingsRequest {
                usage_max_age_secs: Some(4),
                ..Default::default()
            },
            SettingsRequest {
                routing_default_group: Some("frontier".into()),
                ..Default::default()
            },
            SettingsRequest {
                upstream: Some("ftp://nope".into()),
                ..Default::default()
            },
        ] {
            apply_settings(&state, &bad).expect_err("invalid value rejected");
        }
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert_eq!(
            on_disk.scheduler.five_hour_max,
            crate::config::SchedulerConfig::default().five_hour_max,
            "rejected request wrote nothing (default intact)"
        );

        // A mixed live + restart-required request applies both classes.
        let req = SettingsRequest {
            tui_effects: Some(false),
            routing_enabled: Some(true),
            five_hour_max: Some(0.5),
            raw_io_retention_days: Some(7),
            ..Default::default()
        };
        let (applied, restart) = apply_settings(&state, &req).expect("valid");
        assert_eq!(
            applied,
            vec!["tui_effects", "routing_enabled", "five_hour_max"]
        );
        assert_eq!(restart, vec!["raw_io_retention_days"]);
        // Live holders flipped (no restart)...
        assert!(!state.settings_live.tui_effects.load(Ordering::Relaxed));
        assert!(state.settings_live.routing_enabled.load(Ordering::Relaxed));
        assert_eq!(
            state.select_params().five_hour_max,
            0.5,
            "scheduler reads the live ceiling immediately"
        );
        // ...and everything persisted read-merge-write.
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert!(!on_disk.tui_effects);
        assert!(on_disk.routing.enabled);
        assert_eq!(on_disk.scheduler.five_hour_max, 0.5);
        assert_eq!(on_disk.raw_io.retention_days, 7);
        assert_eq!(on_disk.accounts.len(), 1, "accounts survive the merge");
    }

    #[tokio::test]
    async fn settings_endpoint_flips_live_state_and_persists() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("a")]);
        assert!(!state.email_anonymous.load(Ordering::Relaxed), "seed off");

        let response = settings_endpoint(
            State(state.clone()),
            axum::extract::Json(SettingsRequest {
                email_anonymous: Some(true),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["applied"][0], "email_anonymous");
        assert_eq!(
            body["email_anonymous"], true,
            "Islands contract: ack echoes the live value"
        );

        // Live effect, no restart (E3).
        assert!(state.email_anonymous.load(Ordering::Relaxed));
        // Persisted read-merge-write: the flag is on disk AND the seeded
        // account survived the write.
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert!(on_disk.email_anonymous);
        assert_eq!(on_disk.accounts.len(), 1);

        // Flip back off through the same endpoint.
        let response = settings_endpoint(
            State(state.clone()),
            axum::extract::Json(SettingsRequest {
                email_anonymous: Some(false),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!state.email_anonymous.load(Ordering::Relaxed));
        assert!(
            !crate::config::load_path(&path)
                .expect("reload")
                .email_anonymous
        );
    }

    /// Issue #115: the operator endpoint returns every account to cold —
    /// verified through the same snapshot `/llmux/status` serves.
    #[tokio::test]
    async fn reset_usage_endpoint_returns_every_account_to_cold() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("a")]);
        state
            .pool
            .record_429(&AccountId("a".into()), None, SystemTime::now());
        assert!(
            state.pool.snapshot().accounts[0].cooldown_until.is_some(),
            "seed took"
        );

        let response = reset_usage_endpoint(State(state.clone())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["accounts"], 1, "reports the accounts reset");

        let snap = &state.pool.snapshot().accounts[0];
        assert!(snap.cooldown_until.is_none(), "cooldown cleared");
        assert!(snap.five_hour.is_none() && snap.seven_day.is_none(), "cold");
    }

    #[tokio::test]
    async fn settings_endpoint_empty_body_is_a_readback_noop() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![]);
        state.email_anonymous.store(true, Ordering::Relaxed);

        let response = settings_endpoint(
            State(state.clone()),
            axum::extract::Json(SettingsRequest::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["email_anonymous"], true, "reads back current value");
        // Nothing written: the on-disk config still says false.
        assert!(
            !crate::config::load_path(&path)
                .expect("reload")
                .email_anonymous
        );
    }

    /// The settings route sits on the shared `.route(...)` chain behind the
    /// `client_auth` middleware. Since the two-axis gate (multi-tenant #22)
    /// it is CONTROL plane: an admin credential is required even from
    /// loopback — keyless loopback resolves to the `local` tenant, which is
    /// data-plane only.
    #[test]
    fn settings_mutation_auth_follows_shared_gate() {
        use crate::proxy::keys::{KeyRegistry, Resolution};
        assert!(is_control_plane("/llmux/settings"));
        let mut config = Config::default();
        config.proxy.api_key = Some("lm-secret".into());
        let reg = KeyRegistry::from_config(&config);
        // Remote without key: denied outright.
        assert_eq!(reg.resolve(None, false), Resolution::Denied);
        // Remote with the legacy key: admin → control plane passes.
        match reg.resolve(Some("lm-secret"), false) {
            Resolution::Allowed(t) => assert!(t.admin),
            other => panic!("expected admin, got {other:?}"),
        }
        // Keyless loopback: allowed as `local` but NOT admin → the gate
        // 403s it on control routes.
        match reg.resolve(None, true) {
            Resolution::Allowed(t) => assert!(!t.admin),
            other => panic!("expected local tenant, got {other:?}"),
        }
    }

    /// An `EventsRequest` upsert body (all four fields present).
    fn upsert_req(id: &str, from: &str, to: &str, content: &str) -> EventsRequest {
        EventsRequest {
            remove: None,
            id: Some(id.into()),
            from: Some(from.into()),
            to: Some(to.into()),
            content: Some(content.into()),
        }
    }

    /// An `EventsRequest` remove body.
    fn remove_req(id: &str) -> EventsRequest {
        EventsRequest {
            remove: Some(id.into()),
            id: None,
            from: None,
            to: None,
            content: None,
        }
    }

    #[tokio::test]
    async fn events_endpoint_upserts_and_persists() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![oauth_account("keep")]);

        // The example entry, with the compact local-time format and padded
        // whitespace to prove trimming.
        let response = events_endpoint(
            State(state.clone()),
            axum::extract::Json(upsert_req(
                "  20260712-fable5  ",
                "202607080000",
                "202607130000",
                "  Fable 5 Available until 7/12  ",
            )),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["ok"], true);
        // The echoed list carries the trimmed, stored entry.
        assert_eq!(body["events"][0]["id"], "20260712-fable5");
        assert_eq!(body["events"][0]["from"], "202607080000");
        assert_eq!(body["events"][0]["to"], "202607130000");
        assert_eq!(body["events"][0]["content"], "Fable 5 Available until 7/12");

        // Persisted read-merge-write: the banner is on disk AND the seeded
        // account survived the write.
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert_eq!(on_disk.events.len(), 1, "event persisted");
        assert_eq!(on_disk.events[0].id, "20260712-fable5");
        assert_eq!(on_disk.accounts.len(), 1, "seeded account preserved");

        // Live: the running daemon serves the new banner on the very next
        // dashboard document — no restart, no config reload. Both TUI backends
        // render this.
        let doc = crate::dashboard::build_doc(&state, SystemTime::now());
        assert_eq!(doc.events.len(), 1, "build_doc carries the event");
        assert_eq!(doc.events[0].content, "Fable 5 Available until 7/12");
    }

    #[tokio::test]
    async fn events_endpoint_upsert_is_idempotent_and_appends_new_ids() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![]);

        // Same id twice with different content → ONE entry, replaced.
        events_endpoint(
            State(state.clone()),
            axum::extract::Json(upsert_req("a", "202607080000", "202607130000", "first")),
        )
        .await;
        let response = events_endpoint(
            State(state.clone()),
            axum::extract::Json(upsert_req("a", "202607080000", "202607130000", "second")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["events"].as_array().expect("array").len(), 1);
        assert_eq!(body["events"][0]["content"], "second", "same id replaced");

        // An IDENTICAL payload is a no-op that still 200s and keeps one entry.
        let response = events_endpoint(
            State(state.clone()),
            axum::extract::Json(upsert_req("a", "202607080000", "202607130000", "second")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["events"].as_array().expect("array").len(), 1);

        // A NEW id appends.
        let response = events_endpoint(
            State(state),
            axum::extract::Json(upsert_req("b", "202607080000", "202607130000", "other")),
        )
        .await;
        let body = response_json(response).await;
        assert_eq!(body["events"].as_array().expect("array").len(), 2);
        // On-disk order: replaced-in-place "a" first, appended "b" second.
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert_eq!(on_disk.events[0].id, "a");
        assert_eq!(on_disk.events[1].id, "b");
    }

    #[tokio::test]
    async fn events_endpoint_removes_by_id_idempotently() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![]);
        state
            .upsert_event(crate::config::EventBanner {
                id: "a".into(),
                from: "202607080000".into(),
                to: "202607130000".into(),
                content: "keep".into(),
            })
            .expect("seed a");
        state
            .upsert_event(crate::config::EventBanner {
                id: "b".into(),
                from: "202607080000".into(),
                to: "202607130000".into(),
                content: "drop".into(),
            })
            .expect("seed b");

        // Remove "b" → only "a" remains, on disk and live.
        let response =
            events_endpoint(State(state.clone()), axum::extract::Json(remove_req("b"))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["events"].as_array().expect("array").len(), 1);
        assert_eq!(body["events"][0]["id"], "a");
        let on_disk = crate::config::load_path(&path).expect("reload");
        assert_eq!(on_disk.events.len(), 1);
        assert_eq!(on_disk.events[0].id, "a");

        // Removing an absent id is idempotent — still 200, unchanged list.
        let response = events_endpoint(State(state), axum::extract::Json(remove_req("b"))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["events"].as_array().expect("array").len(), 1);
    }

    #[test]
    fn build_doc_seeds_events_from_config_at_boot() {
        // The live holder is seeded from `config.events` in `AppState::new`, so
        // a daemon booted with configured banners serves them in the FIRST
        // dashboard document — before any `POST /llmux/events`.
        let config = Config {
            accounts: vec![oauth_account("a")],
            events: vec![crate::config::EventBanner {
                id: "20260712-fable5".into(),
                from: "202607080000".into(),
                to: "202607130000".into(),
                content: "Fable 5 Available until 7/12".into(),
            }],
            ..Default::default()
        };
        let pool = AccountPool::new(&config.accounts);
        let state = AppState::new(config, pool, None, None).expect("state");
        let doc = crate::dashboard::build_doc(&state, SystemTime::now());
        assert_eq!(doc.events.len(), 1, "config events seeded into doc");
        assert_eq!(doc.events[0].id, "20260712-fable5");
    }

    #[tokio::test]
    async fn events_endpoint_rejects_invalid_upserts() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let state = endpoint_state(&path, vec![]);

        // Unparseable `from`.
        let bad_from = events_endpoint(
            State(state.clone()),
            axum::extract::Json(upsert_req("a", "not-a-time", "202607130000", "c")),
        )
        .await;
        assert_eq!(bad_from.status(), StatusCode::BAD_REQUEST);

        // `from` >= `to`.
        let inverted = events_endpoint(
            State(state.clone()),
            axum::extract::Json(upsert_req("a", "202607130000", "202607080000", "c")),
        )
        .await;
        assert_eq!(inverted.status(), StatusCode::BAD_REQUEST);

        // Blank id and blank content.
        let blank_id = events_endpoint(
            State(state.clone()),
            axum::extract::Json(upsert_req("  ", "202607080000", "202607130000", "c")),
        )
        .await;
        assert_eq!(blank_id.status(), StatusCode::BAD_REQUEST);
        let blank_content = events_endpoint(
            State(state.clone()),
            axum::extract::Json(upsert_req("a", "202607080000", "202607130000", "  ")),
        )
        .await;
        assert_eq!(blank_content.status(), StatusCode::BAD_REQUEST);

        // Missing fields (only id) → 400. Blank remove → 400.
        let missing = events_endpoint(
            State(state.clone()),
            axum::extract::Json(EventsRequest {
                remove: None,
                id: Some("a".into()),
                from: None,
                to: None,
                content: None,
            }),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
        let blank_remove =
            events_endpoint(State(state), axum::extract::Json(remove_req("  "))).await;
        assert_eq!(blank_remove.status(), StatusCode::BAD_REQUEST);

        // Nothing written across all rejections.
        assert!(crate::config::load_path(&path)
            .expect("reload")
            .events
            .is_empty());
    }
}
