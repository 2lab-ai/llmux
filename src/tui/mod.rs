//! ratatui dashboard (FR6): per-account quota gauges (5h/7d) with reset
//! countdowns, active/cooldown status, activity log, totals.
//!
//! IA (issue #5): a wall-clock MAIN view that is ALWAYS rendered (header ·
//! account quota table · scheduler/totals summary · compact model strip ·
//! in-flight/activity) plus three summoned [`Overlay`]s drawn OVER it —
//! `a`ccounts (account detail + add/remove/login affordances), `g` stats (the
//! detailed per-model table + drill-down), `l`ogs (full-screen tail). `Esc`
//! closes any overlay back to MAIN. MAIN-level keys: `q`uit, `R`eload config,
//! `f`/`m`/`e` codex, `↑↓` scroll. The account interactions (`s`witch, `a`dd,
//! `r`emove, `n`ew browser login) run within the Accounts overlay as [`Mode`]s.
//!
//! Two entry points, ONE renderer:
//! - [`run_local`] — in-process mode (`llmux server` on a TTY): renders
//!   live `AppState` (pool + dashboard hub) directly.
//! - [`run_remote`] — attach mode (`llmux dashboard`, or `llmux
//!   server` when a daemon already owns the port): polls
//!   `GET /llmux/dashboard` every second and renders the fetched
//!   document. Manual switch goes through `POST /llmux/switch`; account
//!   add/remove (issue #3) go through `POST /llmux/add-account` /
//!   `POST /llmux/remove-account`, so they work in attach mode too. Only `R`
//!   (reload from the local config file) stays local-mode-only.
//!
//! Both paths build the same [`view::DashboardView`] (local: from an
//! in-process [`crate::dashboard::DashboardDoc`]; remote: from the fetched
//! JSON) — the draw code in [`ui`] is never forked.

pub(crate) mod activity;
mod clip;
mod event;
// pub(crate): `cli::status` reuses the token/age formatters so the plain
// `llmux status` output and the dashboard agree on the display.
pub(crate) mod format;
pub(crate) mod logs;
// pub(crate): `Chrome` (and `DashboardView::display_order`) carry
// `triage::AccountSort` across the crate-visible TUI surface, so the enum must
// be at least as visible as they are.
pub(crate) mod triage;
mod ui;
pub(crate) mod view;

pub use event::{ActivityEvent, Endpoint, TokenCounts};

/// Bound for the proxy→dashboard activity channel (`try_send` +
/// drop-on-full on the sender side, so a stalled dashboard never
/// backpressures the request path).
///
/// Activity events are tiny (a few enum fields). The previous bound of 256 was
/// small enough that a burst of concurrent codex requests could fill it between
/// dashboard folds, and a *dropped* `RequestFinished` leaks its in-flight row
/// forever (BUG: zombie 25,000s+ rows while the daemon reports `in_flight=0`).
/// 4096 removes drops under realistic codex load; the stale-sweep in
/// [`activity::ActivityLog::prune_stale_in_flight`] is the backstop that
/// guarantees a dropped finish can never leak.
pub const ACTIVITY_CHANNEL_CAP: usize = 4096;

/// Whether the dashboard draws without colour: `NO_COLOR` set to anything
/// non-empty when the process started (https://no-color.org). Read once —
/// the frame loop asks every tick.
fn no_color() -> bool {
    static NO_COLOR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NO_COLOR.get_or_init(|| twolab_tui::Theme::from_env().is_mono())
}

/// Whether the dashboard's twolab-tui animations hold still:
/// `LLMUX_REDUCED_MOTION` set to anything non-empty when the process started.
/// The attach connecting screen's spinner keeps its first glyph and its
/// label is plain bold; the clock still counts. The loading labels (sessions,
/// keys title, raw viewer) drop their shimmer band for plain bold, and their
/// spinners hold their first glyph. Read once.
fn motion() -> twolab_tui::Motion {
    static MOTION: std::sync::OnceLock<twolab_tui::Motion> = std::sync::OnceLock::new();
    *MOTION.get_or_init(|| match std::env::var_os("LLMUX_REDUCED_MOTION") {
        Some(v) if !v.is_empty() => twolab_tui::Motion::Reduced,
        _ => twolab_tui::Motion::Full,
    })
}

/// Whether the terminal draws East-Asian Ambiguous glyphs two cells wide
/// (common with Korean, Japanese and Chinese locales): `LLMUX_AMBIGUOUS_WIDE=1`
/// when the process started. Then the dashboard's section rules use their
/// ASCII fallback (`-- accounts ---`), because `─` would take two cells and
/// push each rule past the right edge. Read once, like `NO_COLOR`.
fn ambiguous_wide() -> bool {
    static WIDE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WIDE.get_or_init(|| ambiguous_wide_from(std::env::var_os("LLMUX_AMBIGUOUS_WIDE").as_deref()))
}

/// `LLMUX_AMBIGUOUS_WIDE`'s value as the setting: on for `1`, off for
/// anything else (unset, empty, `0`).
fn ambiguous_wide_from(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| v == "1")
}

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::style::{Attribute, ResetColor, SetAttribute};
use crossterm::terminal::{enable_raw_mode, Clear, ClearType, EnterAlternateScreen};
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use twolab_tui::composer::{ComposerEvent, ComposerState, Mask};
use twolab_tui::modal::{ModalOutcome, ModalState};
use twolab_tui::scrollbar::ScrollPos;

use crate::config::AccountConfig;
use crate::dashboard::{CodexSettingsDoc, DashboardDoc};
use view::DashboardView;

/// Codex models the dashboard cycles through with `m` (req8.1), newest
/// generation first. Any model can still be set via config / the control
/// endpoint; this is the quick-pick set. `gpt-6-astra` added 2026-09-07: it is
/// in [`crate::provider::codex`]'s passthrough list, so a pin set from here is
/// forwarded upstream VERBATIM rather than rewritten to the configured pin.
const CODEX_MODELS: &[&str] = &[
    "gpt-6.1-sol",
    "gpt-6-astra",
    "gpt-6-sol",
    "gpt-6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.5",
    "gpt-5.5-codex",
    "gpt-5-codex",
];
/// Reasoning-effort levels cycled with `e` (and the group-settings bar);
/// "" = BYPASS — the client's `output_config.effort` rides through (UI-3
/// U12). A concrete value OVERRIDES every request. `max` is native on the
/// gpt-5.6 / gpt-6 / gpt-6.1 families, and clamps to `xhigh` on older models.
const CODEX_EFFORTS: &[&str] = &["", "minimal", "low", "medium", "high", "xhigh", "max"];
/// Grok effort rotation for the group-settings bar (UI-3 U12); "" = bypass.
/// Values are the config superset (`none|low|medium|high|xhigh`) — per-model
/// clamping happens at request time in the provider (`grok-4.6` keeps
/// `xhigh`, `grok-4.5` clamps it to `high`).
const GROK_EFFORTS: &[&str] = &["", "none", "low", "medium", "high", "xhigh"];

/// One-line summary of codex settings for the status bar.
fn codex_status_line(c: &CodexSettingsDoc) -> String {
    format!(
        "codex {} · fast {} · effort {}",
        c.model,
        if c.fast { "on" } else { "off" },
        c.effort.as_deref().unwrap_or("bypass"),
    )
}

/// Distinct buckets of one granularity on the view (usage-stats): the Usage
/// overlay's scroll bound. Rows arrive grouped by bucket (newest first), so
/// counting key CHANGES equals counting distinct buckets — the same grouping
/// the renderer applies.
fn usage_bucket_count(view: &DashboardView, gran: activity::UsageGran) -> usize {
    let mut count = 0;
    let mut last: Option<u64> = None;
    for r in view.usage_stats.iter().filter(|r| r.gran == gran.tag()) {
        if last != Some(r.bucket) {
            count += 1;
            last = Some(r.bucket);
        }
    }
    count
}

/// The usage bucket the table's scrollbar points at for table row `row`
/// (a bucket row plus one row per model, as the overlay draws them): the
/// first bucket that starts at or after `row`, the last when none does.
fn usage_bucket_at_row(view: &DashboardView, gran: activity::UsageGran, row: usize) -> usize {
    let mut starts: Vec<usize> = Vec::new();
    let (mut rows, mut last) = (0usize, None);
    for r in view.usage_stats.iter().filter(|r| r.gran == gran.tag()) {
        if last != Some(r.bucket) {
            starts.push(rows);
            rows += 1;
            last = Some(r.bucket);
        }
        rows += 1;
    }
    starts
        .iter()
        .position(|&start| start >= row)
        .unwrap_or(starts.len().saturating_sub(1))
}

/// Can this client open a browser for an OAuth flow? The login dance (browser
/// plus localhost callback) runs in the CLIENT, so this gates the `n` new-login
/// key; when it returns false the picker is replaced by the `llmux login`
/// fallback rather than starting a flow that would hang on the callback.
///
/// macOS/Windows: `open`/`start` hand the URL to the windowing system, which
/// launches the default browser on the HOST's GUI session. This works even when
/// invoked from an SSH/tmux session (the browser opens on the host's console,
/// where the daemon — and the localhost callback — live). Critically, `SSH_*`
/// env vars routinely LEAK into long-lived tmux sessions, so gating macOS on
/// `SSH_CONNECTION` produced false "headless" negatives for a user sitting at
/// their Mac inside tmux (the bug this fixes). Only Linux's `xdg-open` genuinely
/// needs a reachable display server, so that is the only platform we gate.
fn can_open_browser() -> bool {
    let gui_platform = cfg!(any(target_os = "macos", target_os = "windows"));
    let has_display =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    can_open_browser_decide(gui_platform, has_display)
}

/// Pure decision for [`can_open_browser`], split out so it is testable without
/// mutating process env (which would race other tests). GUI platforms
/// (macOS/Windows) always can; Linux needs a display server. SSH is
/// deliberately NOT an input — it gave false negatives via tmux env leakage.
fn can_open_browser_decide(gui_platform: bool, has_display: bool) -> bool {
    gui_platform || has_display
}

/// Fallback message for a headless client that cannot open a browser. Tells
/// the user to run `llmux login` where the browser is; when attached, that is
/// the daemon host, so name it.
fn headless_login_hint(remote: bool) -> String {
    if remote {
        "new login needs a browser — run `llmux login` on the daemon host, or attach from a \
         machine with a browser"
            .to_string()
    } else {
        "new login needs a browser — run `llmux login` on this host from a desktop session"
            .to_string()
    }
}

/// Render cadence — also the cadence at which a fetched remote document is
/// re-rendered between polls (countdowns keep ticking). 120ms (~8fps) so the
/// status/spinner animations (see `anim`) step smoothly rather than the choppy
/// 4fps the original 250ms FR6 tick gave; still trivial CPU for a glance TUI.
const RENDER_TICK: Duration = Duration::from_millis(120);
/// Remote poll cadence for `GET /llmux/dashboard`.
const FETCH_TICK: Duration = Duration::from_secs(1);
/// How long a transient status-line message stays on screen — twolab-tui's
/// flash lifetime, which the raw viewer's action flash shares (it was 3 s).
const STATUS_TTL: Duration = twolab_tui::banner::FLASH_TTL;

/// One status message and how it went. The outcome is drawn as a glyph and a
/// word (`✓ ok`, `! warn`, `✗ fail` — twolab-tui `Flash`), so a failure never
/// reads like a success without colour. `lines` are the facts behind a
/// summary, one per failed account: a message that carries them is shown as
/// a toast card that can be read line by line, instead of the one-row flash
/// that cut them at the terminal's edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusMsg {
    pub outcome: twolab_tui::banner::Outcome,
    pub text: String,
    pub lines: Vec<String>,
}

impl StatusMsg {
    pub(crate) fn new(outcome: twolab_tui::banner::Outcome, text: impl Into<String>) -> Self {
        Self {
            outcome,
            text: text.into(),
            lines: Vec::new(),
        }
    }

    /// It worked — or, for `…` messages, it was accepted and is under way.
    pub(crate) fn ok(text: impl Into<String>) -> Self {
        Self::new(twolab_tui::banner::Outcome::Ok, text)
    }

    /// Nothing happened, or it worked only in part: cancelled, refused
    /// before trying, saved but not live yet, a stale read on a success.
    pub(crate) fn warn(text: impl Into<String>) -> Self {
        Self::new(twolab_tui::banner::Outcome::Warn, text)
    }

    /// It failed.
    pub(crate) fn fail(text: impl Into<String>) -> Self {
        Self::new(twolab_tui::banner::Outcome::Fail, text)
    }

    /// The facts behind the summary, one per line (shown as a toast).
    pub(crate) fn with_lines(mut self, lines: Vec<String>) -> Self {
        self.lines = lines;
        self
    }
}

/// Last committed account switch, persisted for the scheduler pane (the
/// activity ring forgets; the WHY line must not).
#[derive(Debug, Clone)]
pub(crate) struct LastSwitch {
    pub from: Option<String>,
    pub to: String,
    pub reason: Option<String>,
    pub at: SystemTime,
}

/// Poller health for one oauth account, folded from `UsagePolled` events.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PollHealth {
    /// When the last successful poll finished.
    pub last_ok: Option<SystemTime>,
    /// Consecutive failures (0 = healthy).
    pub consecutive_failures: u32,
    /// When the next poll attempt is scheduled.
    pub next_at: SystemTime,
}

/// Which browser login flow the "new login" picker (`n`) kicks off. The flow
/// runs in the CLIENT (this process) — `login_interactive` for Anthropic,
/// `login_codex_interactive` for ChatGPT/Codex, the xAI device-code flow for
/// Grok, PKCE-to-API-key for OpenRouter, `login_gemini_interactive` for
/// Gemini — then the minted credential is injected into the daemon
/// (in-process locally, `POST /llmux/inject-account` when attached). One code
/// path for local and attach (issue #4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginKind {
    /// Anthropic Claude PKCE OAuth (`login_interactive`).
    Anthropic,
    /// ChatGPT / Codex OAuth (`login_codex_interactive`).
    Codex,
    /// xAI Grok device-code flow — no localhost callback, so the picker prints
    /// the verification URL + user code and polls the token endpoint.
    Grok,
    /// OpenRouter PKCE login whose exchange yields a long-lived API key
    /// (no token refresh to schedule).
    OpenRouter,
    /// Google Antigravity OAuth (`login_gemini_interactive`) with a
    /// `localhost:51121` callback — the only port Google registered for the
    /// client, so a bind failure surfaces instead of retrying elsewhere.
    Gemini,
}

impl LoginKind {
    /// Quick-pick rows for `Mode::NewLogin`, in display order.
    pub(crate) const ALL: [LoginKind; 5] = [
        LoginKind::Anthropic,
        LoginKind::Codex,
        LoginKind::Grok,
        LoginKind::OpenRouter,
        LoginKind::Gemini,
    ];

    /// How the sign-in runs, beside the label in the picker.
    pub(crate) fn flow(self) -> &'static str {
        match self {
            LoginKind::Anthropic | LoginKind::Codex => "browser sign-in",
            LoginKind::Grok => "device code: a URL and a code to enter",
            LoginKind::OpenRouter => "browser sign-in, mints an sk-or-v1 key",
            LoginKind::Gemini => "browser sign-in; terms-of-service note first",
        }
    }

    /// One-line label for the picker + status line.
    pub(crate) fn label(self) -> &'static str {
        match self {
            LoginKind::Anthropic => "Claude (Anthropic OAuth)",
            LoginKind::Codex => "Codex (ChatGPT OAuth)",
            LoginKind::Grok => "Grok (xAI OAuth)",
            LoginKind::OpenRouter => "OpenRouter (OAuth key)",
            LoginKind::Gemini => "Gemini (Google OAuth)",
        }
    }
}

/// Input mode: normal keybar vs. account-selection (the `s` key) vs. the
/// add-account key entry (`a`) vs. the remove confirmation (`r`) vs. the
/// new-login provider picker (`n`).
///
/// Deliberately `Copy` (no owned buffer inside): the text of the open field
/// (API key, limits, config value) lives in [`App::field`], a twolab-tui
/// composer, so no secret ever travels through this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    /// Cursor row for the pending switch.
    Select {
        idx: usize,
    },
    /// Entering an API key for a new account (the `a` key). The typed text is
    /// held in [`App::field`], a masked composer: drawn as `∙` glyphs, read
    /// only by `take_secret` on Enter.
    AddKey,
    /// Confirming a destructive account removal (the `r` key). `idx` is the
    /// display row being removed; the name is resolved at confirm time.
    ConfirmRemove {
        idx: usize,
    },
    /// Confirming ONE rate-limit reset redemption (the `R` key, .prd/16).
    /// `idx` is the display row; the account is resolved at confirm time and
    /// named in the status line, so the gate is never a subject-less y/N.
    ConfirmReset {
        idx: usize,
    },
    /// Picking the provider for a new browser login (the `n` key). `idx` is the
    /// cursor row into [`LoginKind::ALL`]. Enter starts the OAuth flow in this
    /// client; the minted credential is injected into the daemon.
    NewLogin {
        idx: usize,
    },
    /// Editing per-account ceiling overrides for the switcher's highlighted
    /// row (`L`). The typed text lives in [`App::field`]; format
    /// `5h,7d,fbl` percents, empty = back to the global ceilings.
    EditLimits {
        idx: usize,
    },
    /// Right-click context menu on an accounts row (UI-3 U11): `idx` is the
    /// display row, `item` the highlighted menu entry. The anchor cell lives
    /// in [`App::menu_anchor`] (Mode stays `Copy`).
    ContextMenu {
        idx: usize,
        item: usize,
    },
    /// Typing a new value for a config-editor row (config-editor v1). The
    /// value lives in [`App::field`] (Mode stays `Copy`).
    ConfigEdit {
        action: ui::ConfigAction,
    },
    /// y/n gate before a blast-radius config change (config-editor v1). The
    /// human-readable description lives in [`App::config_input`].
    ConfigConfirm {
        action: ui::ConfigAction,
    },
}

/// A summoned surface drawn OVER the always-rendered MAIN view (issue #5). MAIN
/// keeps updating every frame underneath; an overlay only covers its own rect
/// (cleared with [`ratatui::widgets::Clear`] in `ui.rs`). Direct shortcuts —
/// `a`/`g`/`l` open, `Esc` returns to MAIN — with no ordered carousel.
///
/// `Copy` so it threads through `Chrome` without allocation. The in-overlay
/// interactions (Select/AddKey/ConfirmRemove/NewLogin) still live in [`Mode`]
/// and operate WITHIN the Accounts overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Overlay {
    /// MAIN only — no overlay.
    #[default]
    None,
    /// Account detail + the add/remove/login affordances (issues #3/#4).
    Accounts,
    /// The detailed per-model usage table + drill-down (req13; was `show_models`).
    Stats,
    /// Calendar usage table (usage-stats): hourly/daily/monthly buckets ×
    /// model with token breakdown + API-equivalent cost.
    Usage,
    /// Full-screen log tail (was the `l` log-panel size cycle).
    Logs,
    /// Session timeline (issue #34): persisted raw-io grouped by
    /// `metadata.user_id` into confidence-labeled per-session aggregates.
    Sessions,
    /// Observed-performance surface (perf telemetry v1): daily
    /// tokens/sec chart + provider health matrix + per-(model, fast) table.
    Perf,
    /// Everything-else surface (UI-3 U6 "기타"): keybindings, build info,
    /// daemon facts — the glance answers that fit no other tab.
    Misc,
    /// Read-only config surface (UI-3 U6): the live daemon settings the
    /// dashboard knows (scheduler / codex / display), with their toggles.
    Config,
    /// Client-key panel (multi-tenant #22): issued keys joined with their
    /// per-tenant usage (requests/tokens/cost/span) and per-model breakdown.
    Keys,
}

/// Keys-panel state (`docs/keys-history/spec.md` K-0): WHICH durable-usage
/// question the `K` overlay is asking, and the answer it is showing.
///
/// The panel no longer renders the in-memory lifetime aggregates that ride on
/// the dashboard document — it renders the answer to an explicit
/// (window, models) query against the durable store, fetched in the
/// background. That is why `loading`/`error` are first-class here: a pending
/// or failed query must SAY so, never fall back to unfiltered data wearing a
/// filtered label.
#[derive(Debug, Clone, Default)]
pub(crate) struct KeysPanel {
    /// Trailing window (`w` cycles); `all` by default — the pre-existing
    /// behavior.
    pub window: crate::key_usage::UsageWindow,
    /// Selected models (`f` picker). EMPTY = every model.
    pub models: std::collections::BTreeSet<String>,
    /// Scroll offset in FLATTENED rows (tenant rows + their model rows).
    pub scroll: usize,
    /// A query is in flight.
    pub loading: bool,
    /// The dashboard clock when the panel started loading: the title's
    /// `loading…` shimmer sweeps from there. A re-query while one is still
    /// in flight keeps it — the label has not changed.
    pub loading_since: Duration,
    /// The last query's failure, shown instead of rows.
    pub error: Option<String>,
    /// The last successful answer (shared, so a Chrome clone is cheap).
    /// DROPPED the moment a different question is asked — rows that answered
    /// the previous window/filter must never sit under the new label.
    pub doc: Option<std::sync::Arc<crate::key_usage::KeysUsageDoc>>,
    /// Models observed by the last successful answer, cached SEPARATELY from
    /// it so dropping stale rows never empties the `f` picker.
    pub available: Vec<String>,
    /// The open model picker, if any.
    pub picker: Option<KeysPicker>,
    /// Newest dispatched query id — older deliveries are dropped, so a fast
    /// `w w w` can never render an out-of-order answer.
    pub generation: u64,
}

/// The `f` multi-select model picker (keys-history K-0) — twolab-tui's
/// `multi_select`, which owns the offered models, their checkboxes (the
/// working selection, applied to the panel only on Enter), the cursor and
/// the filter.
#[derive(Debug, Clone)]
pub(crate) struct KeysPicker {
    pub list: twolab_tui::multi_select::MultiSelect,
    /// `/` opened the filter: printable keys type into it (so `a`, `c`, `j`,
    /// `k`, `f` and `q` stay picker keys until then).
    pub filtering: bool,
}

impl KeysPicker {
    /// Rows the picker shows before it scrolls.
    const ROWS: usize = 10;

    /// A picker offering `options` (the models observed in the current
    /// window, UNIONED with anything already selected, so a selection made
    /// under a wider window stays visible and deselectable), `selected`
    /// checked.
    pub(crate) fn new(
        options: Vec<String>,
        selected: &std::collections::BTreeSet<String>,
    ) -> KeysPicker {
        KeysPicker {
            list: Self::list(options, selected),
            filtering: false,
        }
    }

    fn list(
        options: Vec<String>,
        selected: &std::collections::BTreeSet<String>,
    ) -> twolab_tui::multi_select::MultiSelect {
        use twolab_tui::multi_select::{MultiItem, MultiSelect};
        let items = options
            .into_iter()
            .map(|m| {
                let checked = selected.contains(&m);
                MultiItem::new(m.clone(), m).checked(checked)
            })
            .collect();
        MultiSelect::new(items).max_rows(Self::ROWS)
    }

    /// The offered model names, in order.
    pub(crate) fn options(&self) -> Vec<String> {
        self.list.items().iter().map(|i| i.id.clone()).collect()
    }

    /// The working selection.
    pub(crate) fn selected(&self) -> std::collections::BTreeSet<String> {
        self.list.checked_ids().into_iter().collect()
    }

    /// Offer `options` instead, keeping what is checked, the filter and the
    /// row under the cursor (the list has no way to set items or the cursor
    /// in place — library gap — so it is rebuilt and moved back).
    fn rebuild(&mut self, options: Vec<String>, selected: &std::collections::BTreeSet<String>) {
        use twolab_tui::{Key, KeyInput};
        let cursor = self
            .list
            .selected()
            .and_then(|i| self.list.items().get(i))
            .map(|i| i.id.clone());
        let mut list = Self::list(options, selected).with_query(self.list.search_query());
        if let Some(id) = cursor {
            for _ in 0..list.visible_len() {
                if list
                    .selected()
                    .and_then(|i| list.items().get(i))
                    .map(|i| &i.id)
                    == Some(&id)
                {
                    break;
                }
                list.key(KeyInput::new(Key::Down));
            }
        }
        self.list = list;
    }

    /// `a` / `c`: check or clear every offered model.
    fn set_all(&mut self, checked: bool) {
        let all: std::collections::BTreeSet<String> = if checked {
            self.options().into_iter().collect()
        } else {
            Default::default()
        };
        self.rebuild(self.options(), &all);
    }
}

/// One dispatched keys-usage query, drained by the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeysQuery {
    pub generation: u64,
    pub window: crate::key_usage::UsageWindow,
    pub models: Vec<String>,
}

/// One keys-usage answer delivered back to the event loop.
#[derive(Debug)]
pub(crate) struct KeysLoad {
    pub generation: u64,
    pub result: Result<crate::key_usage::KeysUsageDoc, String>,
}

/// Rows one PageUp/PageDown moves the keys panel.
const KEYS_PAGE: usize = 10;

/// Sort order of the Sessions overlay (`o` cycles): most-recent first (the
/// timeline default), most tokens (in+out), or most requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SessionSort {
    #[default]
    Recent,
    Tokens,
    Requests,
}

impl SessionSort {
    pub(crate) fn next(self) -> Self {
        match self {
            SessionSort::Recent => SessionSort::Tokens,
            SessionSort::Tokens => SessionSort::Requests,
            SessionSort::Requests => SessionSort::Recent,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            SessionSort::Recent => "recent",
            SessionSort::Tokens => "tokens",
            SessionSort::Requests => "requests",
        }
    }

    /// Apply this order to a session list (stable within equal keys).
    pub(crate) fn apply(self, sessions: &mut [crate::session::Session]) {
        match self {
            SessionSort::Recent => sessions.sort_by_key(|s| std::cmp::Reverse(s.last_ms)),
            SessionSort::Tokens => sessions
                .sort_by_key(|s| std::cmp::Reverse(s.tokens_in.saturating_add(s.tokens_out))),
            SessionSort::Requests => sessions.sort_by_key(|s| std::cmp::Reverse(s.requests)),
        }
    }
}

/// Session-local pane-height overrides (UI-3 U7/U8): `None` = the pane's
/// automatic height (content-derived / fixed). Set by dragging the separator
/// row (the NEXT pane's top border) with the mouse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PaneHeights {
    pub accounts: Option<u16>,
    pub middle: Option<u16>,
    pub strip: Option<u16>,
}

/// UI-local state the renderer needs besides the data view: cursor, panes,
/// spinner frame, status line, attach banner.
pub(crate) struct Chrome {
    pub frame: usize,
    pub mode: Mode,
    /// Which summoned surface (if any) is drawn over MAIN this frame (issue #5).
    pub overlay: Overlay,
    /// The live status message (the app drops it once [`STATUS_TTL`] has
    /// passed), drawn on the footer's status row with its outcome.
    pub status_line: Option<StatusMsg>,
    /// Status messages with several facts, as toast cards (twolab-tui
    /// `ToastQueue`); what is up is decided at [`Chrome::clock`].
    pub toasts: twolab_tui::toast::ToastQueue,
    /// The dashboard's own clock this frame: time since it started. The draw
    /// reads no clock; toasts (and anything that ages) go by this.
    pub clock: Duration,
    /// How long the attach connection has been in its current state —
    /// connecting, retrying — as the connecting screen counts it.
    pub attach_wait: Duration,
    /// Full or reduced motion (`LLMUX_REDUCED_MOTION`) for the twolab-tui
    /// widgets that animate.
    pub motion: twolab_tui::Motion,
    /// Activity-log scroll offset: number of newest completed entries skipped
    /// (0 = live tail). Lets the panel page through the full history (req6).
    pub activity_scroll: usize,
    /// The activity entry (if any) currently click-expanded to show its detail
    /// lines (Feature B). Keyed by a STABLE identity (`ActivityKey` = at_ms +
    /// method + path + status) so it survives new rows prepending — never a
    /// list index.
    pub expanded_activity: Option<activity::ActivityKey>,
    /// The folded `count` run (if any) currently click-opened (UI-5). Distinct
    /// from `expanded_activity` so a member row inside an open run can show
    /// its OWN detail without the click reading as "collapse the group"
    /// (Z 2026-07-15). Keyed by any member's `ActivityKey`.
    pub expanded_run: Option<activity::ActivityKey>,
    /// Keys-panel state (keys-history K): window/model filter, scroll, the
    /// fetched usage document, and the open model picker. The panel renders
    /// from THIS, not from the dashboard document's lifetime aggregates.
    pub keys: KeysPanel,
    /// Cursor row in the Stats overlay's model table.
    pub model_cursor: usize,
    /// Trailing window the Stats heatmap aggregates over (issue #23), cycled
    /// with `w` while the Stats overlay is open.
    pub stats_window: activity::StatsWindow,
    /// Folded session timeline for the Sessions overlay (issue #34), snapshotted
    /// from the persisted raw-io log when the overlay was opened. Empty otherwise.
    pub sessions: Vec<crate::session::Session>,
    /// True while a background `stream_sessions` load is in flight (issue: `s`
    /// froze the TUI ~10s). The overlay shows a full-screen spinner only while
    /// this is set AND no partial has arrived yet; once partials land the table
    /// renders with a `loading… N%` title until the final delivery clears this.
    pub sessions_loading: bool,
    /// Percent of the raw-io file consumed by the in-flight streaming load
    /// (`bytes_read*100/file_len`), shown in the overlay title. 100 at rest.
    pub sessions_pct: u8,
    /// The dashboard clock when the in-flight load began: its loading label's
    /// shimmer sweeps from there.
    pub sessions_since: Duration,
    /// Cursor row in the Sessions overlay's session list.
    pub session_cursor: usize,
    /// Sessions overlay sort order (`o` cycles).
    pub session_sort: SessionSort,
    /// Within-group order of the accounts table (`o` toggles it on MAIN and
    /// the accounts overlay). Drives BOTH the render and the row cursors.
    pub account_sort: triage::AccountSort,
    /// Config-editor cursor row + the edit/confirm prompt text.
    pub config_cursor: usize,
    pub config_input: String,
    /// The marked option of the config confirm gate (0 = apply, 1 = cancel):
    /// Enter answers it, ↑↓ move it. The account gates (remove / reset) keep
    /// their mark on Cancel, so Enter there never spends or deletes anything.
    pub confirm_choice: usize,
    /// The redemption this client still holds, if any — the reset gate names
    /// it (and its request id) for as long as the gate is open.
    pub reset_held: Option<PendingRedemption>,
    /// Restart-only values saved THIS session (action → new value): the row
    /// keeps showing the effective boot value, and this map drives an
    /// explicit "saved: X (restart)" note — pending must never masquerade
    /// as applied, and applied must never hide a pending save.
    pub config_saved: std::collections::HashMap<ui::ConfigAction, String>,
    /// `Some` in attach mode.
    pub attach: Option<Attach>,
    /// The open text field (`Mode::AddKey` / `EditLimits` / `ConfigEdit`; an
    /// empty composer otherwise): the footer and the config prompt draw it
    /// with twolab-tui's composer. In `AddKey` it is masked — the renderer
    /// can only draw `∙` glyphs, and `text()` reads empty — never the key.
    pub field: twolab_tui::composer::ComposerState,
    /// Session-local `u`-key override of the quota-gauge fill direction;
    /// `None` = the config default carried on the view applies.
    pub quota_display_override: Option<crate::config::QuotaDisplay>,
    /// `t`-key session toggle: absolute UTC reset stamps in the quota bars.
    pub reset_absolute: bool,
    /// `z`-key session toggle: MAIN packs the accounts whose 7d window is
    /// over its ceiling into a text block under the table (off by default).
    pub fold_exhausted: bool,
    /// `v`-key session toggle on the accounts tab: the compact account list
    /// (one row per account with its state glyph) instead of the table.
    /// MAIN switches to that list by itself when the table would show
    /// fewer than five rows.
    pub compact_accounts: bool,
    /// Drag-set pane heights (UI-3 U7/U8); `None` entries keep the automatic
    /// layout.
    pub pane_heights: PaneHeights,
    /// Anchor cell of the open right-click context menu (UI-3 U11).
    pub menu_anchor: Option<(u16, u16)>,
    /// The pinned account id the open context menu targets — the SINGLE
    /// source of truth for both the menu's rendering and its execution
    /// (display indexes reorder every frame; review R2 MUST-FIX).
    pub menu_account: Option<String>,
    /// Tokens-per-day chart span in days (`d` cycles, UI-3 U14).
    pub chart_days: u64,
    /// Perf-overlay chart/table span in days (perf telemetry v1), cycled with
    /// `d` while the Perf overlay is open.
    pub perf_days: u64,
    /// Cursor row in the Perf overlay's series table.
    pub perf_cursor: usize,
    /// Selected day for the series table, as an offset back from today
    /// (`None` = whole-span aggregate; `h`/`l` or ←/→ move it).
    pub perf_day_off: Option<u64>,
    /// Usage-tab granularity (`g` cycles hour/day/month, usage-stats).
    pub usage_gran: activity::UsageGran,
    /// Usage-tab scroll offset: number of newest BUCKETS skipped (0 = most
    /// recent at the top).
    pub usage_scroll: usize,
    /// Logs-overlay position: `None` follows the newest line (the pager
    /// sits at the bottom), `Some(top)` holds the view at wrapped row `top`.
    pub logs_scroll: Option<usize>,
    /// The Logs pager's drag selection (content rows and cells), drawn
    /// reversed; `None` when nothing is selected.
    pub logs_selection: Option<twolab_tui::pager::Selection>,
    /// Copy mode (`v`): the mouse is released to the terminal for its own
    /// selection and the footer's key bar is the banner that says so.
    pub copy_mode: bool,
    /// The click-opened input-text modal (UI-6 item 3), or `None` when closed.
    /// Drawn last over MAIN; its content is looked up from `view.completed` by
    /// the stored key every frame, so it works identically in local and attach
    /// mode and closes gracefully when the entry ages out of the ring.
    pub input_modal: Option<InputModal>,
    /// The click-opened raw request/response viewer (UI-7), or `None` when
    /// closed. Content-owning (cheap to clone — the body lines sit behind an
    /// `Arc`), drawn last like the input modal.
    pub raw_modal: Option<RawModal>,
    /// The click-opened account detail modal (.prd/19 rules 6–7), or `None`
    /// when closed. Like the input modal it stores only an identity; the body
    /// is rebuilt from the snapshot every frame.
    pub account_modal: Option<AccountModal>,
    /// `LLMUX_AMBIGUOUS_WIDE=1` at startup: the terminal draws East-Asian
    /// Ambiguous glyphs two cells wide, so section rules draw in ASCII.
    pub ambiguous_wide: bool,
}

/// The click-opened full-input modal (UI-6 item 3). Holds only the clicked
/// entry's STABLE identity (never a list index — rows prepend) plus the
/// twolab-tui modal state: the scroll offset in wrapped rows, what the last
/// frame drew (keys page by it) and a held scrollbar thumb. The excerpt text
/// itself is re-read from `view.completed` each frame, so nothing goes stale
/// and no wire field is added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputModal {
    pub key: activity::ActivityKey,
    pub state: ModalState,
}

impl InputModal {
    /// Opened on `key`, scrolled to the top.
    pub(crate) fn new(key: activity::ActivityKey) -> InputModal {
        InputModal {
            key,
            state: ModalState::new(),
        }
    }
}

/// The click-opened account detail modal (.prd/19 rules 6–7). Holds the
/// account's REAL id — never a display index, because the table reorders every
/// frame (`display_order`) — plus the modal state (as [`InputModal`]). The
/// body is rebuilt from `view.snapshot` each frame, so it never goes stale
/// and the render pass closes it when the account leaves the pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountModal {
    pub account: String,
    pub state: ModalState,
}

impl AccountModal {
    /// Opened on `account`, scrolled to the top.
    pub(crate) fn new(account: String) -> AccountModal {
        AccountModal {
            account,
            state: ModalState::new(),
        }
    }
}

/// A key on an open text modal (input, account): Esc, `q` and Enter close
/// it (`true`); ↑↓ `j` `k` PgUp PgDn Home End scroll it from what the last
/// frame drew. Every other key is swallowed.
fn modal_key(state: &mut ModalState, code: KeyCode) -> bool {
    let code = match code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('k') => KeyCode::Up,
        KeyCode::Char('j') => KeyCode::Down,
        other => other,
    };
    key_input(code).is_some_and(|key| state.key(key) == ModalOutcome::Closed)
}

/// A mouse event on an open text modal: the wheel scrolls `step` rows, a
/// press on the scrollbar jumps there or grabs the thumb, a drag moves it,
/// and a press on `esc close` closes the modal (`true`). Every event is
/// swallowed, so nothing reaches the rows beneath.
fn modal_mouse(state: &mut ModalState, mouse: &crossterm::event::MouseEvent, step: isize) -> bool {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            state.scroll_by(-step);
        }
        MouseEventKind::ScrollDown => {
            state.scroll_by(step);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            return state.press(mouse.column, mouse.row) == ModalOutcome::Closed;
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            state.drag(mouse.column, mouse.row);
        }
        MouseEventKind::Up(MouseButton::Left) => state.release(),
        _ => {}
    }
    false
}

/// Content state of the raw viewer: the fetch is asynchronous (a backwards
/// scan of a possibly-huge `raw-io.jsonl`, or an HTTP round-trip in attach
/// mode), so the modal opens Loading and resolves to Ready/Failed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RawModalState {
    Loading,
    Failed(String),
    /// Prebuilt per-tab lines behind an `Arc` — `Chrome` is cloned every
    /// frame, and a Ready body can be megabytes of styled lines.
    Ready(std::sync::Arc<ui::RawContent>),
}

/// The click-opened raw request/response viewer (UI-7). Unlike [`InputModal`]
/// it owns its content (fetched once) — it never goes stale and survives the
/// entry aging out of the activity ring.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawModal {
    pub key: activity::ActivityKey,
    /// Activity id, used by the save-file names (`llmux-raw-<id>…`).
    pub id: u64,
    /// Monotonic open-generation (UI-8): bumped every time a modal opens, so a
    /// stale background delivery (a slow raw fetch or a queued export) for a
    /// PRIOR open can't land on the modal the user reopened. Keyed on this,
    /// not just [`ActivityKey`] — the key omits the activity id and a
    /// close→reopen of the SAME row would otherwise accept the old fetch.
    pub generation: u64,
    pub title: String,
    /// Index into the Ready content's tab list (2 or 4 tabs, UI-8); clamped
    /// at draw/use time so a stale index can never panic.
    pub tab: usize,
    /// First body row in view, in rows wrapped at the modal's width
    /// (clamped by the frame that draws it).
    pub scroll: usize,
    /// The body view's cache across frames (measured rows, long lines
    /// wrapped at the width) — one per opened modal.
    pub view: ui::RawBodyView,
    /// Animation frame for the Loading spinner (advances with the shared tick).
    pub spin: usize,
    /// The dashboard clock when it opened: the Loading label's shimmer sweeps
    /// from there.
    pub since: Duration,
    /// Action feedback ("copied … → pbcopy" / "saved → …") with its outcome
    /// and when it was set, shown in place of the key legend. The app drops
    /// it from the frame's chrome once [`STATUS_TTL`] has passed, so the draw
    /// never reads a clock.
    pub flash: Option<(StatusMsg, Instant)>,
    pub state: RawModalState,
}

/// A queued raw-record fetch: the clicked entry's stable key (its `at_ms` is
/// the timestamp half of the raw-io lookup), the activity id, and what the
/// content builder needs from open time (general lines + curl context).
struct RawFetchReq {
    generation: u64,
    id: u64,
    general: ui::RawGeneral,
    /// The lookup key's timestamp half (the record's `at_ms`).
    at_ms: u64,
}

/// Result of a background raw fetch, delivered on the raw channel.
struct RawLoad {
    generation: u64,
    result: Result<std::sync::Arc<ui::RawContent>, String>,
}

/// A queued export (copy/save) action (UI-8): run on the blocking pool so a
/// wedged clipboard tool or a slow Downloads mount can never freeze the TUI
/// event loop. Carries the modal generation so a late result flashes only on
/// the modal that requested it.
struct ClipReq {
    /// Who asked, so the outcome lands where they look.
    origin: ClipOrigin,
    generation: u64,
    button: ui::RawButton,
    id: u64,
    /// Prebuilt payload for the chosen action (body / curl / all / record).
    payload: String,
    /// File-name label for the save actions (tab slug, empty for save-all).
    label: String,
    /// Whether this action writes a file (vs. copies to the clipboard).
    is_save: bool,
    /// File extension for a save action.
    ext: &'static str,
}

/// Where an export was asked for — and so where its outcome shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipOrigin {
    /// A raw-viewer button: the flash line of the modal of that generation.
    Raw,
    /// A drag in the Logs pager: the status line, saying how many lines.
    Logs { lines: usize },
}

/// Outcome of a background export, delivered on the clip channel.
struct ClipResult {
    origin: ClipOrigin,
    generation: u64,
    message: StatusMsg,
}

/// A usage-control operation queued by a key and executed OFF the event loop
/// (.prd/16). Each one can wait on an upstream round-trip, so none of them may
/// run inline: the loop keeps rendering and taking input while a background
/// task performs the call and delivers a [`ControlResult`] on the control
/// channel.
///
/// The same enum drives BOTH backends: local calls the daemon's in-process
/// service, attach POSTs the same operation to the daemon. There is exactly
/// one semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControlOp {
    /// Explicit usage refresh. `None` = every supported account.
    Refresh { account: Option<String> },
    /// Redeem ONE reset. The idempotency key is minted by the CLIENT before
    /// the send and reused verbatim on every retry — a new key could spend a
    /// second reset.
    Reset {
        account: String,
        request_id: String,
        credit_id: Option<String>,
    },
    /// Manual switch, which refreshes the target's usage first (including an
    /// already-active target) and may come back with a refresh warning.
    Switch { account: String },
}

/// A redemption whose outcome this client is still holding: the account, the
/// key that must be reused, and the credit it named. Kept until a TERMINAL
/// outcome arrives (or the daemon reports none pending) — closing the dialog
/// does not clear it, and no new id may be minted for the account while it is
/// set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingRedemption {
    pub account: String,
    pub request_id: String,
    pub credit_id: Option<String>,
}

/// Outcome of a background control operation.
struct ControlResult {
    /// Operator-facing status message, with its outcome.
    message: StatusMsg,
    /// A redemption to hold (uncertain outcome / a conflicting pending id the
    /// daemon reported), replacing whatever the client held.
    hold: Option<PendingRedemption>,
    /// A terminal redemption outcome: stop holding the key.
    resolved: Option<String>,
}

/// Attach-mode banner state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Attach {
    /// Daemon pid, from the probe or the last fetched document.
    pub pid: Option<u32>,
    /// False while the poller cannot reach the daemon (reconnect banner).
    pub connected: bool,
}

/// Options for [`run_remote`].
#[derive(Debug, Clone)]
pub struct RemoteOptions {
    /// `http://localhost:<port>` — same base the CLI probes.
    pub base_url: String,
    /// Proxy api key, sent as `x-api-key` (loopback is exempt; harmless).
    pub api_key: Option<String>,
    /// Daemon pid from the probe, for the header marker before the first
    /// document arrives.
    pub pid: Option<u32>,
}

/// Where the dashboard data comes from.
enum Backend {
    /// In-process: live `AppState` (pool + hub) — the document is built
    /// locally each frame. Boxed to keep the two variants size-balanced
    /// (`AppState` is an Arc-heavy 300+ byte struct).
    Local(Box<crate::proxy::server::AppState>),
    /// Attached to a daemon over HTTP. Boxed — it carries a reqwest client +
    /// the last fetched document.
    Remote(Box<Remote>),
}

struct Remote {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    /// Pid from the probe (the fetched document refreshes it).
    pid: Option<u32>,
    /// Last successfully fetched document (kept through reconnects).
    doc: Option<DashboardDoc>,
    connected: bool,
    /// Codex settings change (fast/model/effort) queued by a key, performed by
    /// the event loop via `POST /llmux/codex` (req8.1).
    pending_codex: Option<crate::dashboard::CodexSettingsDoc>,
    /// Config-editor settings change awaiting POST (attach mode), with
    /// the originating row + display value so a restart-only save can pin
    /// its pending note after the daemon confirms it.
    pending_settings: Option<(
        crate::proxy::server::SettingsRequest,
        Option<(ui::ConfigAction, String)>,
    )>,
    /// Pause/resume queued by the switcher's `p` key, performed by the event
    /// loop via `POST /llmux/pause-account`.
    pending_pause: Option<(String, bool)>,
    /// Limits change queued by the switcher's `L` editor, performed by the
    /// event loop via `POST /llmux/account-limits`.
    pending_limits: Option<(String, crate::config::AccountLimits)>,
    /// Scheduler-mode change queued by `S`, performed by the event loop via
    /// `POST /llmux/scheduler-mode`.
    pending_mode: Option<crate::config::SchedulerMode>,
    /// API key for a new account, queued by the `a` flow and performed by the
    /// event loop via `POST /llmux/add-account`. Held only until the POST
    /// fires; never logged or rendered raw.
    pending_add: Option<String>,
    /// Account name queued for removal (`r` confirm), performed by the event
    /// loop via `POST /llmux/remove-account`.
    pending_remove: Option<String>,
    /// Grok effort change queued by the group-settings bar (UI-3 U12),
    /// performed by the event loop via `POST /llmux/grok`. Inner `None` =
    /// clear to bypass.
    pending_grok: Option<Option<String>>,
}

/// One message from the remote fetch task.
enum FetchMsg {
    Doc(Box<DashboardDoc>),
    Lost,
}

/// Dashboard state, re-rendered each tick from a fresh view-model.
struct App {
    backend: Backend,
    mode: Mode,
    /// Monotonic frame counter driving the spinner.
    frame: usize,
    should_quit: bool,
    /// The last status message and when it was set (it shows for
    /// [`STATUS_TTL`]).
    status: Option<(StatusMsg, Instant)>,
    /// When the dashboard started: the origin of its own clock
    /// ([`Chrome::clock`]), which the toast queue keeps time by.
    started: Instant,
    /// Status messages with several facts, shown as toast cards.
    toasts: twolab_tui::toast::ToastQueue,
    /// When the attach connection entered its current state (connected or
    /// not); the dashboard's start until the first fetch answers.
    link_since: Instant,
    /// Which summoned overlay is open over MAIN (issue #5): `a`→Accounts,
    /// `g`→Stats, `l`→Logs, `Esc`→None. MAIN renders every frame regardless.
    overlay: Overlay,
    /// Activity-log scroll offset (newest entries skipped; 0 = live tail).
    activity_scroll: usize,
    /// Viewport anchor for UI-6 item 4: the newest completed REQUEST's stable
    /// identity as of the last rendered frame, plus the render-row index it sat
    /// at THEN (`last_top_row`). Notes carry no key yet occupy their own render
    /// row, so the newest keyed request can sit at row ≥1 — the anchor's index
    /// must therefore be a DELTA (`new_index - last_top_row`), not an absolute,
    /// or a leading note would bump the offset every idle tick (runaway). When
    /// new entries prepend while scrolled into history, the anchor's new render
    /// row minus its seeded row = the prepend count; bumping the offset by that
    /// keeps the page under the cursor put. Robust at ring capacity (append
    /// evicts oldest → folded-row COUNT is flat, so a length delta would be
    /// dead). `None` = no keyed frame observed yet.
    last_top_key: Option<activity::ActivityKey>,
    last_top_row: usize,
    /// The click-expanded activity entry (Feature B), keyed by stable identity
    /// so it survives new rows prepending. `None` = nothing expanded.
    expanded_activity: Option<activity::ActivityKey>,
    /// The click-opened folded `count` run (UI-5), keyed by any member's
    /// stable identity. Separate from `expanded_activity` (see `Chrome`).
    expanded_run: Option<activity::ActivityKey>,
    /// Older completed entries hydrated from the persisted activity log
    /// (`activity.jsonl`) once the operator scrolls near the end of the live
    /// window (UI-5 infinite scroll, Z 2026-07-15). Newest-first; merged
    /// strictly-older-than-live at render time. `None` = not loaded yet.
    history_completed: Option<Vec<activity::Completed>>,
    /// How many of those history entries are MATERIALIZED into the frame's
    /// view. Grown only on state transitions — scroll events and history
    /// arrival ([`Self::grow_history_take`]) — so the folding work needed to
    /// pick the amount never runs on the per-frame render path (review R3-2).
    history_take: usize,
    /// True while the background history load is in flight.
    history_loading: bool,
    /// Sender the blocking history loader delivers on (mirrors `sessions_tx`).
    history_tx: Option<mpsc::Sender<Vec<activity::Completed>>>,
    /// Every click target the LAST rendered frame drew, in paint order (the
    /// hit module): replaced after each `draw`, so a click never maps onto
    /// an older layout. The mouse handler asks it once per event.
    hits: ui::Hits,
    /// Sessions table hit layout from the last frame (mouse row select).
    sessions_chrome: Option<ui::SessionsChrome>,
    /// The open keys / sessions / usage table's scrollbar from the last frame
    /// (`None`: nothing overflows), and the press-drag-release on it.
    table_scroll: Option<ui::TableScroll>,
    table_drag: twolab_tui::scrollbar::ScrollbarDrag,
    /// Config-editor value-cell hits from the last frame.
    config_chrome: Vec<ui::ConfigHit>,
    /// The raw viewer's hit-test layout from the LAST rendered frame (UI-8):
    /// payload-tab rects + top-right action buttons. Same record/read cycle
    /// as `hits`; empty while the modal is closed.
    raw_chrome: ui::RawModalChrome,
    /// A press-drag-release on the raw viewer's body scrollbar.
    raw_drag: twolab_tui::scrollbar::ScrollbarDrag,
    /// Session-local pane-height overrides set by separator drags.
    pane_heights: PaneHeights,
    /// Tokens-per-day chart span (UI-3 U14), cycled with `d` in Stats.
    chart_days: u64,
    /// Perf overlay span (`d` cycles) + series-table cursor + day select.
    perf_days: u64,
    perf_cursor: usize,
    perf_day_off: Option<u64>,
    /// The rendered context menu's hit layout (UI-3 U11), when open.
    menu_chrome: Option<twolab_tui::menu::MenuLayout>,
    /// Anchor cell of the open context menu (col, row).
    menu_anchor: Option<(u16, u16)>,
    /// The REAL account id the open context menu targets, captured at open
    /// time. Display indexes reorder every frame (selection order follows
    /// live quota state), so actions re-resolve this name to the CURRENT
    /// display row at execution — a reorder between open and click can never
    /// retarget another account (review R1 MUST-FIX 6).
    menu_account: Option<String>,
    /// Whether the limits editor was opened FROM the context menu (then it
    /// exits to Normal, not back into the switcher).
    limits_from_menu: bool,
    /// The separator currently being dragged, if any (mouse button held):
    /// the target its press landed on, kept for the drag that follows.
    drag: Option<ui::SeparatorHit>,
    /// Cursor row in the Stats overlay's model table.
    model_cursor: usize,
    /// Trailing window the Stats heatmap aggregates over (issue #23), cycled
    /// with `w` in the Stats overlay.
    stats_window: activity::StatsWindow,
    /// Folded session timeline (issue #34), loaded from the persisted raw-io log
    /// when the Sessions overlay is opened (`s`) and held until it is reopened.
    /// A point-in-time snapshot — re-opening re-reads the file. Empty otherwise.
    sessions: Vec<crate::session::Session>,
    /// True while the background load kicked off by `open_sessions` is running
    /// (streaming read+parse+fold of the multi-MB raw-io log). Cleared when the
    /// final (`done`) partial arrives over `sessions_tx`. Drives the overlay
    /// loading spinner (while empty) and the `loading… N%` title (once filling).
    sessions_loading: bool,
    /// Percent of the raw-io file the in-flight streaming load has consumed,
    /// carried on each partial and shown in the overlay title. 100 at rest.
    sessions_pct: u8,
    /// The dashboard clock when the in-flight load began.
    sessions_since: Duration,
    /// Sender handed to the `spawn_blocking` load task by `open_sessions`; the
    /// event loop owns the receiver and applies each progressive partial. `None`
    /// only in unit tests that never run `event_loop` (they drive overlay state
    /// directly).
    sessions_tx: Option<mpsc::Sender<SessionsLoad>>,
    /// Cursor row in the Sessions overlay's session list.
    session_cursor: usize,
    /// Sessions overlay sort order (`o` cycles; re-applied on load delivery).
    session_sort: SessionSort,
    /// Session toggle (`o` on MAIN / the accounts overlay): within-group order
    /// of the accounts table — name (default) or the scheduler's next-pick
    /// order. Session-local, never persisted to config.
    account_sort: triage::AccountSort,
    /// Config-editor cursor + the confirm gate's question (Mode::ConfigConfirm;
    /// the value being typed is [`Self::field`]).
    config_cursor: usize,
    config_input: String,
    /// The marked option of the config confirm gate ([`Chrome::confirm_choice`]).
    confirm_choice: usize,
    /// Pending confirmed settings request awaiting y/n (built at edit time so
    /// confirm applies EXACTLY what was validated).
    config_pending: Option<crate::proxy::server::SettingsRequest>,
    /// Restart-only values saved this session (see `Chrome::config_saved`).
    config_saved: std::collections::HashMap<ui::ConfigAction, String>,
    /// The raw display value of the request parked in `config_pending`.
    config_pending_value: Option<String>,
    /// The text field of the open prompt — the API key (`Mode::AddKey`,
    /// masked), the per-account limits (`Mode::EditLimits`) or a config value
    /// (`Mode::ConfigEdit`): a one-line twolab-tui composer with a caret
    /// (←/→, Home/End), mid-value edits, Ctrl+U / Ctrl+K / Ctrl+W cuts that
    /// Ctrl+Y puts back, and pastes. Held outside `Mode` so the enum stays
    /// `Copy`; replaced on open, submit and cancel. The masked key comes out
    /// only through `take_secret` — `text()`, `take()` and `Debug` hide it,
    /// and the frame draws `∙` glyphs.
    field: twolab_tui::composer::ComposerState,
    /// Cells the open field was drawn in last frame: its window follows the
    /// caret from there (see [`Self::track_field`]).
    field_width: u16,
    /// New browser login queued by the `n` picker, performed by the event loop
    /// (the OAuth flow is async AND needs the raw terminal back — the loop
    /// suspends the TUI, runs the flow, then re-inits). Held on `App` (not
    /// `Remote`) because both local and attach mode use it; the only
    /// difference is where the minted credential is injected. `None` on a
    /// headless client (the picker shows the `llmux login` fallback instead).
    pending_login: Option<LoginKind>,
    /// Session-local override of the quota-gauge fill direction, flipped with
    /// `u` (MAIN and the Accounts overlay). `None` until the first press — the
    /// config default carried on the view applies.
    quota_display_override: Option<crate::config::QuotaDisplay>,
    /// Session toggle (`t`): quota bars show the reset as an absolute UTC
    /// stamp instead of the countdown.
    reset_absolute: bool,
    /// Session toggle (`z` on MAIN): fold the accounts whose 7d window is
    /// over its ceiling into a text block under the accounts table. Off
    /// until the first press; session-local like `u`/`t`/`o`.
    fold_exhausted: bool,
    /// Session toggle (`v` on the accounts tab): the compact account list
    /// instead of the table (see [`Chrome::compact_accounts`]).
    compact_accounts: bool,
    /// Usage-tab granularity (`g` cycles hour/day/month, usage-stats).
    usage_gran: activity::UsageGran,
    /// Usage-tab scroll offset in BUCKETS (0 = newest at the top); reset when
    /// the granularity changes so a deep hourly scroll can't strand the
    /// monthly table past its last row.
    usage_scroll: usize,
    /// Logs-overlay position (`None` = follow the tail); see
    /// [`Chrome::logs_scroll`].
    logs_scroll: Option<usize>,
    /// Where the Logs pager drew last frame: what a scroll key moves from.
    logs_pos: twolab_tui::scrollbar::ScrollPos,
    /// The rect the Logs pager drew in last frame (`None`: not open) — where
    /// [`Self::logs_select_mouse`] paints it again off-screen.
    logs_area: Option<ratatui::layout::Rect>,
    /// The Logs pager's drag selection; see [`Chrome::logs_selection`]. While
    /// one is shown the pager holds still instead of following.
    logs_sel: Option<twolab_tui::pager::Selection>,
    /// Copy mode (`v`), see [`Chrome::copy_mode`]: the event loop releases
    /// mouse capture while it is on and holds the frame still, so the text
    /// under the terminal's own selection does not move.
    copy_mode: bool,
    /// The click-opened input-text modal (UI-6 item 3); `None` when closed. The
    /// scroll offset is clamped after each draw against the wrapped line count
    /// the render pass reports (`MainChrome::input_modal_max_scroll`).
    input_modal: Option<InputModal>,
    /// The click-opened raw request/response viewer (UI-7); `None` when closed.
    raw_modal: Option<RawModal>,
    /// The click-opened account detail modal (.prd/19 rule 6); `None` when
    /// closed. Same post-draw reconcile as `input_modal`: the render pass
    /// clamps the scroll, or closes the modal when the pinned account is gone.
    account_modal: Option<AccountModal>,
    /// A queued raw-record fetch, drained by the event loop into a background
    /// task (same pattern as the other `pending_*` remote ops).
    pending_raw: Option<RawFetchReq>,
    /// Sender the background raw fetch delivers its [`RawLoad`] on; installed
    /// by the event loop (mirrors `sessions_tx`).
    raw_tx: Option<mpsc::Sender<RawLoad>>,
    /// Monotonic raw-modal open counter (UI-8); the next modal's generation.
    raw_generation: u64,
    /// Queued exports (UI-8 copy/save), dispatched SINGLE-FLIGHT and in order.
    /// A `VecDeque`, not a single slot: two export actions in one input burst
    /// (ready events drain before the event loop spawns them) must both run —
    /// a single `Option` silently dropped the earlier one. But the loop spawns
    /// only ONE at a time (`clip_inflight`), so clipboard writes execute in
    /// press order (last-writer is the last press, not a race) and a slow/
    /// wedged exporter applies backpressure instead of piling unbounded work
    /// onto the blocking pool. Capped at [`CLIP_QUEUE_MAX`].
    pending_clip: std::collections::VecDeque<ClipReq>,
    /// True while one export runs on the blocking pool; cleared when its
    /// [`ClipResult`] arrives. Gates the next dispatch (single-flight).
    clip_inflight: bool,
    /// Sender the background export delivers its [`ClipResult`] on.
    clip_tx: Option<mpsc::Sender<ClipResult>>,
    /// A usage-control operation queued by a key, drained by the event loop
    /// into a background task (.prd/16) — same pattern as `pending_raw`.
    pending_control: Option<ControlOp>,
    /// True while a control operation is running. SINGLE-FLIGHT: it is what
    /// makes a second Enter/`f`/`y` a no-op instead of a second refresh — or,
    /// far worse, a second redemption.
    control_inflight: bool,
    /// Sender the background control task delivers its outcome on.
    control_tx: Option<mpsc::Sender<ControlResult>>,
    /// A redemption this client is still holding (see [`PendingRedemption`]).
    /// Closing the confirm dialog does NOT clear it; only a terminal outcome
    /// does.
    pending_redemption: Option<PendingRedemption>,
    /// Keys panel: the durable-usage query and its answer (keys-history K).
    keys: KeysPanel,
    /// A keys-usage query queued by a key press, dispatched by the event loop
    /// (blocking SQL locally, HTTP when attached) — same pattern as
    /// `pending_raw`, so neither the DB nor the network is ever touched from a
    /// key handler.
    pending_keys: Option<KeysQuery>,
    /// Sender the background keys-usage query delivers on.
    keys_tx: Option<mpsc::Sender<KeysLoad>>,
}

impl App {
    fn new(backend: Backend) -> Self {
        Self {
            backend,
            mode: Mode::Normal,
            frame: 0,
            should_quit: false,
            status: None,
            started: Instant::now(),
            toasts: twolab_tui::toast::ToastQueue::new(),
            link_since: Instant::now(),
            overlay: Overlay::None,
            activity_scroll: 0,
            last_top_key: None,
            last_top_row: 0,
            expanded_activity: None,
            expanded_run: None,
            history_completed: None,
            history_take: 0,
            history_loading: false,
            history_tx: None,
            hits: ui::Hits::new(),
            sessions_chrome: None,
            table_scroll: None,
            table_drag: Default::default(),
            config_chrome: Vec::new(),
            raw_chrome: ui::RawModalChrome::default(),
            raw_drag: Default::default(),
            pane_heights: PaneHeights::default(),
            chart_days: 14,
            perf_days: 14,
            perf_cursor: 0,
            perf_day_off: None,
            menu_chrome: None,
            menu_anchor: None,
            menu_account: None,
            limits_from_menu: false,
            drag: None,
            model_cursor: 0,
            stats_window: activity::StatsWindow::default(),
            sessions: Vec::new(),
            sessions_loading: false,
            sessions_pct: 100,
            sessions_since: Duration::ZERO,
            sessions_tx: None,
            session_cursor: 0,
            session_sort: SessionSort::default(),
            account_sort: triage::AccountSort::default(),
            config_cursor: 0,
            config_input: String::new(),
            confirm_choice: 0,
            config_pending: None,
            config_pending_value: None,
            config_saved: Default::default(),
            field: Default::default(),
            field_width: 0,
            pending_login: None,
            quota_display_override: None,
            usage_gran: activity::UsageGran::default(),
            usage_scroll: 0,
            logs_scroll: None,
            logs_pos: Default::default(),
            logs_area: None,
            logs_sel: None,
            copy_mode: false,
            input_modal: None,
            raw_modal: None,
            account_modal: None,
            pending_raw: None,
            raw_tx: None,
            raw_generation: 0,
            pending_clip: std::collections::VecDeque::new(),
            clip_inflight: false,
            clip_tx: None,
            reset_absolute: false,
            fold_exhausted: false,
            compact_accounts: false,
            pending_control: None,
            control_inflight: false,
            control_tx: None,
            pending_redemption: None,
            keys: KeysPanel::default(),
            pending_keys: None,
            keys_tx: None,
        }
    }

    /// True when this dashboard is attached to a remote daemon (not the
    /// in-process server). Reused to decide where a minted login credential is
    /// injected (in-process vs. `POST /llmux/inject-account`).
    fn is_remote(&self) -> bool {
        matches!(self.backend, Backend::Remote(_))
    }

    /// Build the view-model for one frame. `None` only in remote mode before
    /// the first document arrives.
    fn view(&self, now: SystemTime) -> Option<DashboardView> {
        let mut view = match &self.backend {
            // The event banner rides the dashboard document from the daemon's
            // live event holder, so both backends get it through `from_doc` —
            // no local-only special case.
            Backend::Local(state) => Some(DashboardView::from_doc(&crate::dashboard::build_doc(
                state, now,
            ))),
            Backend::Remote(remote) => remote.doc.as_ref().map(DashboardView::from_doc),
        }?;
        self.extend_with_history(&mut view);
        Some(view)
    }

    /// Append lazily-hydrated history behind the live window (UI-5 infinite
    /// scroll): only entries strictly OLDER than the oldest live row (the
    /// persisted tail overlaps the live ring — the timestamp cut dedupes),
    /// and only as many as the current scroll depth can reach plus one page,
    /// so the per-frame clone stays bounded by how deep the operator actually
    /// scrolled rather than the whole persisted file.
    fn extend_with_history(&self, view: &mut DashboardView) {
        if let Some(history) = &self.history_completed {
            extend_completed_with_history(&mut view.completed, history, self.history_take);
        }
    }

    /// Keep a scrolled-into-history viewport anchored when new completed entries
    /// arrive (UI-6 item 4). Rows are newest-first and the scroll window counts
    /// render rows from the newest, so a freshly prepended entry would slide the
    /// page the operator is reading down. While `activity_scroll > 0`, locate
    /// the row that carries last frame's anchor key in THIS frame's folded rows:
    /// its new index MINUS the index it was seeded at (`last_top_row`) is the
    /// number of rows prepended above it, so bumping the offset by that delta
    /// leaves the same rows under the cursor. The delta (not the absolute index)
    /// is essential: a Note occupies its own render row but has no key, so the
    /// newest keyed request can sit at row ≥1 — an absolute bump would then add
    /// that offset on every idle redraw tick (runaway). Robust at ring capacity,
    /// where each append evicts the oldest and the folded-row COUNT never
    /// changes. Key not found (evicted / edge) → leave the offset alone. At
    /// `scroll == 0` we keep live-tail (no bump) AND skip the fold — re-seeding
    /// the anchor needs only the cheap leading scan. Called once per rendered
    /// frame, before `chrome()` snapshots the offset.
    fn preserve_scroll_on_new_activity(&mut self, view: &DashboardView) {
        if self.activity_scroll > 0 {
            if let Some(anchor) = self.last_top_key.clone() {
                let rows = triage::collapse_completed(&view.completed);
                if let Some(new_index) = Self::render_row_of_key(&view.completed, &rows, &anchor) {
                    let prepended = new_index.saturating_sub(self.last_top_row);
                    if prepended > 0 {
                        let ceiling = rows.len().saturating_sub(1);
                        self.activity_scroll =
                            self.activity_scroll.saturating_add(prepended).min(ceiling);
                    }
                }
            }
        }
        // Re-seed to the newest keyed (request) entry and the render row it now
        // occupies. That row index is exactly the count of leading Note entries
        // ahead of the first request (Notes are unfoldable 1:1 rows and any run
        // fold sits BELOW the first request), so no second fold is needed.
        match view
            .completed
            .iter()
            .enumerate()
            .find_map(|(i, c)| c.activity_key().map(|k| (k, i)))
        {
            Some((key, row)) => {
                self.last_top_key = Some(key);
                self.last_top_row = row;
            }
            None => {
                self.last_top_key = None;
                self.last_top_row = 0;
            }
        }
    }

    /// The index of the render row containing `key`, or `None` if no row does.
    /// For a folded run, ANY member matching counts (the run is one render row).
    fn render_row_of_key(
        completed: &[activity::Completed],
        rows: &[triage::ActivityRow],
        key: &activity::ActivityKey,
    ) -> Option<usize> {
        rows.iter().position(|row| match row {
            triage::ActivityRow::Single(i) => completed[*i].activity_key().as_ref() == Some(key),
            triage::ActivityRow::Run { start, len } => completed[*start..*start + *len]
                .iter()
                .any(|c| c.activity_key().as_ref() == Some(key)),
        })
    }

    /// Grow the materialized-history window (`history_take`) until the FOLDED
    /// render-row count reaches `activity_scroll + HISTORY_PAGE` — capped at
    /// [`HISTORY_GROW_CHUNKS`] chunks per call so one giant folded `count`
    /// wall cannot make a single event traverse the whole 100k-entry history
    /// (review R3-2; further scrolling fires further calls, so loading stays
    /// progressive). Runs ONLY on state transitions — a scroll event or the
    /// history arrival — never per frame. Folding happens here, on a scratch
    /// copy; the frame path then just appends the first `history_take`
    /// strictly-older entries. Called on arrival even at `scroll == 0` so a
    /// live window that folds to a single row (whose scroll ceiling is 0)
    /// still gets its first history page and the ceiling can rise
    /// (review R3-1 deadlock).
    fn grow_history_take(&mut self, view: &DashboardView) {
        let Some(history) = &self.history_completed else {
            return;
        };
        let target_rows = self.activity_scroll.saturating_add(HISTORY_PAGE);
        let mut scratch = view.completed.clone();
        let mut rows = triage::collapse_completed(&scratch).len();
        let mut chunks = 0;
        while rows < target_rows && chunks < HISTORY_GROW_CHUNKS {
            let oldest = scratch.last().map(|c| c.at);
            let before = scratch.len();
            scratch.extend(
                history
                    .iter()
                    .filter(|c| oldest.is_none_or(|o| c.at < o))
                    .take(HISTORY_CHUNK)
                    .cloned(),
            );
            let appended = scratch.len() - before;
            if appended == 0 {
                return; // history exhausted
            }
            self.history_take += appended;
            rows = triage::collapse_completed(&scratch).len();
            chunks += 1;
        }
    }

    /// Whether infinite-scroll hydration may read the LOCAL `activity.jsonl`
    /// (review M1): the file belongs to THIS host's daemon, so it is the
    /// right history for the in-process backend and for a LOOPBACK attach
    /// (the standard `llmux` → localhost:3456 topology — same machine, same
    /// state file). Attached to a daemon on another host, the local file is a
    /// DIFFERENT daemon's activity — splicing it under the remote live rows
    /// would show wrong data, so hydration stays off until the remote paging
    /// endpoint exists (issue #107).
    fn history_is_local(&self) -> bool {
        match &self.backend {
            Backend::Local(_) => true,
            Backend::Remote(remote) => base_url_is_loopback(&remote.base_url),
        }
    }

    /// Kick off the one-shot background hydration of the persisted activity
    /// log for infinite scroll (UI-5). Read+parse+replay is blocking IO/CPU
    /// over a multi-MB file → blocking pool, result over `history_tx`
    /// (mirrors `open_sessions`). Idempotent: no-op while loading or loaded,
    /// and refused entirely for a cross-host attach (see
    /// [`Self::history_is_local`]).
    fn request_history(&mut self) {
        if self.history_loading || self.history_completed.is_some() || !self.history_is_local() {
            return;
        }
        self.history_loading = true;
        if let Some(tx) = self.history_tx.clone() {
            tokio::task::spawn_blocking(move || {
                let _ = tx.blocking_send(load_history());
            });
        }
    }

    fn chrome(&self) -> Chrome {
        self.chrome_at(Instant::now())
    }

    /// The frame's chrome at `now`, the one clock reading of the frame:
    /// expired messages are dropped here and every age is measured from it,
    /// so nothing the draw sees depends on when the draw runs.
    fn chrome_at(&self, now: Instant) -> Chrome {
        let live = |at: &Instant| now.saturating_duration_since(*at) < STATUS_TTL;
        Chrome {
            frame: self.frame,
            mode: self.mode,
            overlay: self.overlay,
            activity_scroll: self.activity_scroll,
            expanded_activity: self.expanded_activity.clone(),
            expanded_run: self.expanded_run.clone(),
            keys: self.keys.clone(),
            model_cursor: self.model_cursor,
            stats_window: self.stats_window,
            sessions: self.sessions.clone(),
            sessions_loading: self.sessions_loading,
            sessions_pct: self.sessions_pct,
            sessions_since: self.sessions_since,
            session_cursor: self.session_cursor,
            session_sort: self.session_sort,
            account_sort: self.account_sort,
            config_cursor: self.config_cursor,
            config_input: self.config_input.clone(),
            confirm_choice: self.confirm_choice,
            reset_held: self.pending_redemption.clone(),
            config_saved: self.config_saved.clone(),
            // Only while a field is open; a masked clone hides its text
            // the same way the original does.
            field: if self.field_open() {
                self.field.clone()
            } else {
                Default::default()
            },
            quota_display_override: self.quota_display_override,
            reset_absolute: self.reset_absolute,
            fold_exhausted: self.fold_exhausted,
            compact_accounts: self.compact_accounts,
            pane_heights: self.pane_heights,
            menu_anchor: self.menu_anchor,
            menu_account: self.menu_account.clone(),
            chart_days: self.chart_days,
            perf_days: self.perf_days,
            perf_cursor: self.perf_cursor,
            perf_day_off: self.perf_day_off,
            usage_gran: self.usage_gran,
            usage_scroll: self.usage_scroll,
            logs_scroll: self.logs_scroll,
            logs_selection: self.logs_sel,
            copy_mode: self.copy_mode,
            input_modal: self.input_modal.clone(),
            // The Loading spinner rides the shared frame counter (set here so
            // the stored modal itself never needs a per-tick mutation).
            raw_modal: self.raw_modal.clone().map(|mut m| {
                m.spin = self.frame;
                m.flash = m.flash.filter(|(_, at)| live(at));
                m
            }),
            account_modal: self.account_modal.clone(),
            ambiguous_wide: ambiguous_wide(),
            status_line: self
                .status
                .as_ref()
                .filter(|(_, at)| live(at))
                .map(|(msg, _)| msg.clone()),
            toasts: self.toasts.clone(),
            clock: self.clock_at(now),
            attach_wait: now.saturating_duration_since(self.link_since),
            motion: motion(),
            attach: match &self.backend {
                Backend::Local(_) => None,
                Backend::Remote(remote) => Some(Attach {
                    pid: remote.doc.as_ref().map(|d| d.pid).or(remote.pid),
                    connected: remote.connected,
                }),
            },
        }
    }

    /// Active status-line text, if it hasn't expired.
    #[cfg(test)]
    fn status_line(&self) -> Option<&str> {
        self.status
            .as_ref()
            .filter(|(_, since)| since.elapsed() < STATUS_TTL)
            .map(|(msg, _)| msg.text.as_str())
    }

    /// The dashboard's own clock at `now`: time since it started.
    fn clock_at(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
    }

    /// Show a status message. A short one is the footer's one-row flash; one
    /// that carries several facts (a multi-account refresh with failures) is
    /// a toast card instead, one line per fact, and the footer row gives way
    /// to it. A newer card replaces the one up rather than queueing behind.
    fn set_status(&mut self, msg: StatusMsg) {
        let now = Instant::now();
        if msg.lines.is_empty() {
            self.status = Some((msg, now));
            return;
        }
        self.status = None;
        let toast = twolab_tui::toast::Toast::new(msg.outcome.into(), msg.text)
            .lines(msg.lines)
            .key("status");
        let clock = self.clock_at(now);
        self.toasts.push(toast, clock);
    }

    fn apply_fetch(&mut self, msg: FetchMsg) {
        if let Backend::Remote(remote) = &mut self.backend {
            let connected = matches!(msg, FetchMsg::Doc(_));
            if connected != remote.connected {
                self.link_since = Instant::now();
            }
            match msg {
                FetchMsg::Doc(doc) => {
                    remote.doc = Some(*doc);
                    remote.connected = true;
                }
                FetchMsg::Lost => remote.connected = false,
            }
        }
    }

    // --- usage controls (.prd/16) ------------------------------------------

    /// Queue one control operation for the event loop, unless one is already
    /// running. The single-flight refusal is the anti-duplicate gate: a second
    /// Enter on the redemption confirm, or a leaned-on `f`, must never start a
    /// second operation.
    /// Returns whether the operation was ACCEPTED. Callers that mutate state
    /// alongside the queue (the redemption, which starts holding an
    /// idempotency key) must key that mutation off this — a refused queue that
    /// still installed a hold would invent an unresolved redemption that was
    /// never sent.
    fn queue_control(&mut self, op: ControlOp, pending_status: String) -> bool {
        if self.control_inflight || self.pending_control.is_some() {
            self.set_status(StatusMsg::warn(
                "a usage-control operation is already running — wait for it to finish",
            ));
            return false;
        }
        self.pending_control = Some(op);
        // Accepted and under way: the outcome replaces it when it lands.
        self.set_status(StatusMsg::ok(pending_status));
        true
    }

    fn take_pending_control(&mut self) -> Option<ControlOp> {
        self.pending_control.take()
    }

    /// Run one control operation OFF the event loop. Local calls the daemon's
    /// in-process service; attach POSTs the same operation. Both deliver their
    /// outcome on the control channel, so the TUI keeps rendering meanwhile.
    fn spawn_control(&mut self, op: ControlOp) {
        let Some(tx) = self.control_tx.clone() else {
            // Only in unit tests that never run `event_loop`; they assert on
            // the QUEUED operation, not on its delivery.
            return;
        };
        self.control_inflight = true;
        match &self.backend {
            Backend::Local(state) => {
                let state = (**state).clone();
                tokio::spawn(async move {
                    let result = run_control_local(&state, op).await;
                    let _ = tx.send(result).await;
                });
            }
            Backend::Remote(remote) => {
                let client = remote.client.clone();
                let base_url = remote.base_url.clone();
                let api_key = remote.api_key.clone();
                tokio::spawn(async move {
                    let result =
                        run_control_remote(&client, &base_url, api_key.as_deref(), op).await;
                    let _ = tx.send(result).await;
                });
            }
        }
    }

    /// Apply a delivered control outcome: free the single-flight slot, show
    /// the message, and update the held redemption. `hold` replaces the held
    /// key (uncertain outcome, or a pending id the daemon reported); `resolved`
    /// clears it — and ONLY a terminal outcome resolves.
    fn apply_control_result(&mut self, result: ControlResult) {
        self.control_inflight = false;
        if let Some(hold) = result.hold {
            self.pending_redemption = Some(hold);
        }
        if let Some(account) = result.resolved {
            if self
                .pending_redemption
                .as_ref()
                .is_some_and(|p| p.account == account)
            {
                self.pending_redemption = None;
            }
        }
        self.set_status(result.message);
    }

    fn on_key(&mut self, key: KeyEvent, view: Option<&DashboardView>) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        // Ctrl-C quits from any mode.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        // Copy mode swallows every key but `v` / Esc, which take the mouse
        // back: the terminal is selecting, and a stray key (or the arrows a
        // terminal sends for the wheel on the alternate screen) must not
        // move what is being selected.
        if self.copy_mode {
            if matches!(key.code, KeyCode::Char('v') | KeyCode::Esc) {
                self.copy_mode = false;
            }
            return;
        }
        // `v` enters copy mode from MAIN, any overlay and the modals over
        // them — wherever no prompt is open and nothing is being typed.
        if key.code == KeyCode::Char('v')
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && self.mode == Mode::Normal
            && !self.keys.picker.as_ref().is_some_and(|p| p.filtering)
        {
            self.copy_mode = true;
            return;
        }
        // The input modal (UI-6 item 3), when open, swallows every key beneath
        // it: Esc/q/Enter close it, the arrows/PgUp/PgDn scroll it. It sits
        // above overlays and modes so a stray key never leaks to MAIN.
        if self.input_modal.is_some() {
            self.on_key_input_modal(key.code);
            return;
        }
        // The raw viewer (UI-7) swallows keys the same way when open.
        if self.raw_modal.is_some() {
            self.on_key_raw_modal(key.code);
            return;
        }
        // The account detail modal (.prd/19 rule 6) swallows keys the same way:
        // `q` scrolls nothing and quits nothing while it is open.
        if self.account_modal.is_some() {
            self.on_key_account_modal(key.code);
            return;
        }
        if key.code == KeyCode::Char('h')
            && matches!(self.overlay, Overlay::None | Overlay::Accounts)
            && matches!(self.mode, Mode::Normal | Mode::Select { .. })
        {
            if let Some(view) = view {
                let order = view.display_order(self.account_sort, SystemTime::now());
                let selected = match self.mode {
                    Mode::Select { idx } => {
                        order.get(idx).and_then(|&i| view.snapshot.accounts.get(i))
                    }
                    _ => view
                        .snapshot
                        .representative_current()
                        .and_then(|id| view.snapshot.accounts.iter().find(|a| &a.id == id))
                        .or_else(|| view.snapshot.accounts.first()),
                };
                if let Some(account) = selected {
                    self.account_modal = Some(AccountModal::new(account.id.0.clone()));
                }
            }
            return;
        }
        // A pending `Mode` interaction (account switch / key entry / remove
        // confirm / login picker) always takes the key first — these run WITHIN
        // the Accounts overlay (issues #3/#4) and must keep working unchanged.
        match self.mode {
            Mode::Select { idx } => return self.on_key_select(key.code, idx, view),
            Mode::EditLimits { idx } => return self.on_key_edit_limits(key, idx, view),
            Mode::AddKey => return self.on_key_add(key),
            Mode::ConfirmRemove { idx } => return self.on_key_confirm_remove(key.code, idx, view),
            Mode::ConfirmReset { idx } => return self.on_key_confirm_reset(key.code, idx, view),
            Mode::NewLogin { idx } => return self.on_key_new_login(key.code, idx),
            Mode::ContextMenu { idx, item } => {
                return self.on_key_context_menu(key.code, idx, item, view)
            }
            Mode::ConfigEdit { action } => return self.on_key_config_edit(key, action, view),
            Mode::ConfigConfirm { action } => {
                return self.on_key_config_confirm(key.code, action, view)
            }
            Mode::Normal => {}
        }
        // Otherwise (Mode::Normal): the active overlay, if any, gets the key;
        // MAIN-only keys run when no overlay is open.
        match self.overlay {
            Overlay::None => self.on_key_main(key.code, view),
            Overlay::Accounts => self.on_key_accounts(key.code, view),
            Overlay::Stats => self.on_key_stats(key.code, view),
            Overlay::Usage => self.on_key_usage(key.code, view),
            Overlay::Logs => self.on_key_logs(key.code),
            Overlay::Sessions => self.on_key_sessions(key.code),
            Overlay::Misc => self.on_key_misc(key.code),
            Overlay::Perf => self.on_key_perf(key.code, view),
            Overlay::Config => self.on_key_config(key.code, view),
            Overlay::Keys => self.on_key_keys(key.code, view),
        }
    }

    /// Handle a mouse event (Feature B). Mouse input is ADDITIVE — keyboard nav
    /// is untouched. It is ignored entirely unless MAIN owns the screen (no
    /// overlay, `Mode::Normal`); an overlay or a pending interaction keeps the
    /// activity panel out of reach, so a stray click can't toggle a hidden row.
    /// A left-click inside the activity list toggles the clicked entry's expand
    /// state; the wheel scrolls the activity history. Returns whether the event
    /// changed anything (→ redraw).
    fn on_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        view: Option<&DashboardView>,
    ) -> bool {
        // What the last frame drew under the pointer, topmost first — the one
        // lookup every click below matches on. Open modals, the menu and
        // the current surface still decide who gets the event at all.
        let hit = self
            .hits
            .hit(mouse.column, mouse.row)
            .map(|(rect, target)| (rect, target.clone()));
        let target = hit.as_ref().map(|(_, target)| target);
        // The input modal (UI-6 item 3), when open, owns the mouse: the wheel
        // scrolls it, its scrollbar can be clicked and dragged, `esc close`
        // closes it, and every other click is swallowed so it can't reach a
        // row beneath the modal.
        if let Some(modal) = self.input_modal.as_mut() {
            if modal_mouse(&mut modal.state, &mouse, 1) {
                self.input_modal = None;
            }
            return true;
        }
        // The raw viewer (UI-7) owns the mouse the same way when open; UI-8
        // adds clickable payload tabs + the top-right action buttons, and the
        // body's scrollbar takes a press and a drag.
        if self.raw_modal.is_some() {
            let hit = |r: &ratatui::layout::Rect| {
                mouse.column >= r.x
                    && mouse.column < r.x + r.width
                    && mouse.row >= r.y
                    && mouse.row < r.y + r.height
            };
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let button = self
                        .raw_chrome
                        .buttons
                        .iter()
                        .find(|(_, r)| hit(r))
                        .map(|&(b, _)| b);
                    let tab = self
                        .raw_chrome
                        .tabs
                        .iter()
                        .find(|(_, r)| hit(r))
                        .map(|&(i, _)| i);
                    let bar = self.raw_chrome.bar;
                    if let Some(btn) = button {
                        self.raw_modal_action(btn);
                    } else if let (Some(idx), Some(modal)) = (tab, self.raw_modal.as_mut()) {
                        modal.tab = idx;
                        modal.scroll = 0;
                    } else if let (Some(bar), Some(modal)) = (bar, self.raw_modal.as_mut()) {
                        // The body's scrollbar: a press on the track jumps,
                        // one on the thumb grabs it for the drag below.
                        if let Some(top) =
                            self.raw_drag
                                .press(bar.pos, bar.track, mouse.column, mouse.row)
                        {
                            modal.scroll = top;
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let (Some(bar), Some(modal)) = (self.raw_chrome.bar, self.raw_modal.as_mut())
                    {
                        if let Some(top) = self.raw_drag.drag(bar.pos, bar.track, mouse.row) {
                            modal.scroll = top;
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => self.raw_drag.release(),
                kind => {
                    if let Some(modal) = self.raw_modal.as_mut() {
                        match kind {
                            MouseEventKind::ScrollUp => {
                                modal.scroll = modal.scroll.saturating_sub(3)
                            }
                            MouseEventKind::ScrollDown => {
                                modal.scroll = modal.scroll.saturating_add(3)
                            }
                            _ => {}
                        }
                    }
                }
            }
            return true;
        }
        // The account detail modal (.prd/19 rule 6) owns the mouse the same
        // way: the wheel scrolls it three rows a step, the scrollbar drags,
        // every other event is swallowed so a click can't reach the row
        // beneath it.
        if let Some(modal) = self.account_modal.as_mut() {
            if modal_mouse(&mut modal.state, &mouse, 3) {
                self.account_modal = None;
            }
            return true;
        }
        // Tab-bar clicks (UI-3 U6) work from ANY overlay while no text-entry
        // interaction is pending — the tab bar is how the mouse navigates.
        if self.mode == Mode::Normal
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            if let Some(&ui::ClickTarget::Tab(tab)) = target {
                self.open_tab(tab, view);
                return true;
            }
        }
        // The wheel scrolls the Logs pager three rows a step; reaching the
        // bottom follows new lines again.
        if self.overlay == Overlay::Logs && self.mode == Mode::Normal {
            let step: isize = match mouse.kind {
                MouseEventKind::ScrollUp => -3,
                MouseEventKind::ScrollDown => 3,
                _ => 0,
            };
            if step != 0 {
                self.scroll_logs_to(|pos| Some(pos.top.saturating_add_signed(step).min(pos.max)));
                return true;
            }
            if let Some(changed) = self.logs_select_mouse(&mouse, view) {
                return changed;
            }
        }
        // The keys, sessions and usage tables' scrollbar: a press on the
        // track jumps there (or grabs the thumb), a drag moves it, the
        // release lets go. Checked before the row clicks below, so the bar's
        // column never selects a row.
        if matches!(
            self.overlay,
            Overlay::Keys | Overlay::Sessions | Overlay::Usage
        ) && self.mode == Mode::Normal
            && self.keys.picker.is_none()
        {
            if let Some(changed) = self.table_scrollbar_mouse(&mouse, view) {
                return changed;
            }
        }
        // Wheel scrolling in the Usage overlay (usage-stats): its primary
        // interaction IS bucket scrolling, so the wheel must work where the
        // mouse opened the tab (review CR — the tab was mouse-openable but
        // keyboard-only to scroll). Routed through the key handler so the
        // stored-offset clamp applies identically.
        if self.overlay == Overlay::Usage && self.mode == Mode::Normal {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.on_key_usage(KeyCode::Up, view);
                    return true;
                }
                MouseEventKind::ScrollDown => {
                    self.on_key_usage(KeyCode::Down, view);
                    return true;
                }
                _ => {}
            }
        }
        // The wheel scrolls the keys table like ↑/↓ (through the key handler,
        // so its clamp applies).
        if self.overlay == Overlay::Keys && self.mode == Mode::Normal && self.keys.picker.is_none()
        {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.on_key_keys(KeyCode::Up, view);
                    return true;
                }
                MouseEventKind::ScrollDown => {
                    self.on_key_keys(KeyCode::Down, view);
                    return true;
                }
                _ => {}
            }
        }
        // Config editor (config-editor v1): click a value cell to activate
        // that row (same path as Enter); wheel moves the cursor.
        if self.overlay == Overlay::Config && self.mode == Mode::Normal {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.on_key_config(KeyCode::Up, view);
                    return true;
                }
                MouseEventKind::ScrollDown => {
                    self.on_key_config(KeyCode::Down, view);
                    return true;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(hit) = self
                        .config_chrome
                        .iter()
                        .find(|h| {
                            mouse.row == h.area.y
                                && mouse.column >= h.area.x
                                && mouse.column < h.area.right()
                        })
                        .copied()
                    {
                        self.config_cursor = hit.row;
                        self.activate_config_action(hit.action, view);
                        return true;
                    }
                }
                _ => {}
            }
        }
        // Sessions overlay (issue: mouse select): click a row to move the
        // cursor there (the detail pane follows); wheel scrolls the cursor.
        if self.overlay == Overlay::Sessions && self.mode == Mode::Normal {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.move_session_cursor(-1, self.sessions.len());
                    return true;
                }
                MouseEventKind::ScrollDown => {
                    self.move_session_cursor(1, self.sessions.len());
                    return true;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(t) = self.sessions_chrome {
                        let r = t.rows;
                        if mouse.row >= r.y
                            && mouse.row < r.bottom()
                            && mouse.column >= r.x
                            && mouse.column < r.right()
                        {
                            let idx = t.start + (mouse.row - r.y) as usize;
                            if idx < self.sessions.len() {
                                self.session_cursor = idx;
                            }
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
        // An open context menu (UI-3 U11) owns the mouse: click an item to
        // run it, click anywhere else to dismiss.
        if let Mode::ContextMenu { idx, .. } = self.mode {
            if matches!(
                mouse.kind,
                MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
            ) {
                let menu = self.menu_chrome.as_ref();
                match menu.and_then(|m| m.hit(mouse.column, mouse.row)) {
                    Some(item) => self.run_menu_item(idx, item, view),
                    // A dim (cannot-run) item or the border: nothing runs,
                    // the menu stays.
                    None if menu.is_some_and(|m| m.contains(mouse.column, mouse.row)) => {}
                    None => self.close_menu(),
                }
                return true;
            }
            return false;
        }
        // Right-click on an accounts row opens its context menu (UI-3 U11).
        if self.overlay == Overlay::None
            && self.mode == Mode::Normal
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right))
        {
            if let Some(&ui::ClickTarget::AccountRow(idx)) = target {
                // Pin the REAL account id now; display indexes reorder.
                self.menu_account = view.and_then(|v| {
                    let order = v.display_order(self.account_sort, SystemTime::now());
                    order
                        .get(idx)
                        .and_then(|&i| v.snapshot.accounts.get(i))
                        .map(|a| a.id.0.clone())
                });
                self.mode = Mode::ContextMenu { idx, item: 0 };
                self.menu_anchor = Some((mouse.column, mouse.row));
                return true;
            }
            return false;
        }
        // Left-click on an accounts row opens the account detail modal
        // (.prd/19 rule 6). Works on MAIN and on the accounts overlay, which
        // render the same table; the modal blocks above already returned when
        // one is open, so no modal can be stacked. On the accounts overlay the
        // overlay's rows lie over MAIN's, so the map answers with them.
        if self.mode == Mode::Normal
            && matches!(self.overlay, Overlay::None | Overlay::Accounts)
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            if let Some(&ui::ClickTarget::AccountRow(idx)) = target {
                // Pin the REAL account id now; display indexes reorder.
                if let Some(id) = view.and_then(|v| {
                    let order = v.display_order(self.account_sort, SystemTime::now());
                    order
                        .get(idx)
                        .and_then(|&i| v.snapshot.accounts.get(i))
                        .map(|a| a.id.0.clone())
                }) {
                    self.account_modal = Some(AccountModal::new(id));
                }
                return true;
            }
        }
        // Otherwise only MAIN (no overlay, no pending mode interaction) gets
        // the mouse.
        if self.overlay != Overlay::None || self.mode != Mode::Normal {
            return false;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                match target {
                    // A separator row (UI-3 U7/U8): the press starts a drag.
                    Some(&ui::ClickTarget::Separator(sep)) => {
                        self.drag = Some(sep);
                        return true;
                    }
                    // Group-settings bar (UI-3 U9/U10): click a segment to
                    // rotate that setting.
                    Some(&ui::ClickTarget::Setting(action)) => {
                        match action {
                            ui::SettingAction::SchedMode => self.toggle_scheduler_mode(view),
                            ui::SettingAction::CodexModel => self.cycle_codex_model(view),
                            ui::SettingAction::CodexEffort => self.cycle_codex_effort(view),
                            ui::SettingAction::CodexFast => self.toggle_codex_fast(view),
                            ui::SettingAction::GrokEffort => self.cycle_grok_effort(view),
                        }
                        return true;
                    }
                    // A models-strip entry opens the Stats tab with that
                    // model under the cursor, its drill-down beside the table.
                    Some(&ui::ClickTarget::StripEntry(model)) => {
                        self.open_model(model, view);
                        return true;
                    }
                    _ => {}
                }
                // An activity row or one of its detail lines: what the click
                // means depends on where in the recorded rect it fell.
                let click = match hit {
                    Some((rect, ui::ClickTarget::Activity { key, kind })) => {
                        Some(ui::activity_click(rect, key, kind, mouse.column))
                    }
                    _ => None,
                };
                match click {
                    // Clicking the `🔍 input` detail line opens the full-text
                    // modal instead of collapsing the entry (UI-6 item 3).
                    Some(ui::ActivityClick::OpenInput(key)) => {
                        self.open_input_modal(key);
                        true
                    }
                    // Clicking the `🔍 request` detail line opens the raw
                    // request/response viewer (UI-7).
                    Some(ui::ActivityClick::OpenRaw(key, id)) => {
                        self.open_raw_modal(key, id, view);
                        true
                    }
                    Some(ui::ActivityClick::Entry(key)) => {
                        self.toggle_expand(key);
                        true
                    }
                    Some(ui::ActivityClick::RunToggle(key)) => {
                        self.toggle_run(key);
                        true
                    }
                    // Run-header body: expand-only — an open group is closed by
                    // its marker, never by a stray body click (Z 2026-07-15).
                    Some(ui::ActivityClick::RunExpand(key)) => {
                        if self.expanded_run.as_ref() == Some(&key) {
                            false
                        } else {
                            self.expanded_run = Some(key);
                            true
                        }
                    }
                    None => false,
                }
            }
            // Dragging a held separator resizes the pane above it: the pane's
            // new height is the pointer row minus the pane's top row, clamped
            // so a pane can never collapse below its border+header.
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(sep) = self.drag else { return false };
                let height = mouse
                    .row
                    .saturating_sub(sep.pane_top)
                    .clamp(ui::PANE_MIN_HEIGHT, ui::PANE_MAX_HEIGHT);
                let slot = match sep.pane {
                    ui::PaneId::Accounts => &mut self.pane_heights.accounts,
                    ui::PaneId::Middle => &mut self.pane_heights.middle,
                    ui::PaneId::Strip => &mut self.pane_heights.strip,
                };
                if *slot == Some(height) {
                    return false;
                }
                *slot = Some(height);
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag.take();
                false
            }
            // Wheel up = into history, down = toward the live tail — same
            // direction as the ↑/↓ keys (a nice-to-have bonus).
            MouseEventKind::ScrollUp => {
                self.scroll_activity(1, view);
                true
            }
            MouseEventKind::ScrollDown => {
                self.scroll_activity(-1, view);
                true
            }
            _ => false,
        }
    }

    /// A mouse event on the open table's scrollbar (keys, sessions, usage).
    /// `Some(changed)` when the scrollbar took it — a press on the track, a
    /// drag while the thumb is held, the release that ends it; `None` lets
    /// the event through to the table's own clicks and wheel.
    fn table_scrollbar_mouse(
        &mut self,
        mouse: &crossterm::event::MouseEvent,
        view: Option<&DashboardView>,
    ) -> Option<bool> {
        let top = match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let bar = self.table_scroll?;
                let top = self
                    .table_drag
                    .press(bar.pos, bar.track, mouse.column, mouse.row)?;
                (bar, top)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let bar = self.table_scroll?;
                (bar, self.table_drag.drag(bar.pos, bar.track, mouse.row)?)
            }
            MouseEventKind::Up(MouseButton::Left) if self.table_drag.is_dragging() => {
                self.table_drag.release();
                return Some(false);
            }
            _ => return None,
        };
        let (bar, top) = top;
        Some(self.scroll_table_to(bar.pos, top, view))
    }

    /// Scroll the open table so its first row in view is `top` (a row of
    /// `pos`, which the last frame drew). Keys: the panel's own offset.
    /// Sessions: the view follows the cursor, so the cursor moves — the way
    /// the wheel does — to the row that puts `top` first. Usage: the
    /// scrollbar counts rows and the table scrolls by bucket, so the first
    /// bucket that starts at or after `top` (dragging to the end shows the
    /// last rows). Returns whether anything moved.
    fn scroll_table_to(
        &mut self,
        pos: twolab_tui::scrollbar::ScrollPos,
        top: usize,
        view: Option<&DashboardView>,
    ) -> bool {
        if top == pos.top {
            return false;
        }
        match self.overlay {
            Overlay::Keys => {
                self.keys.scroll = top;
                true
            }
            Overlay::Sessions => {
                let len = self.sessions.len();
                let page = pos.viewport.max(1);
                let cursor = if top == 0 {
                    self.session_cursor.min(page - 1)
                } else {
                    top + page - 1
                };
                let cursor = cursor.min(len.saturating_sub(1));
                let moved = cursor != self.session_cursor;
                self.session_cursor = cursor;
                moved
            }
            Overlay::Usage => {
                let Some(view) = view else { return false };
                let bucket = usage_bucket_at_row(view, self.usage_gran, top);
                let moved = bucket != self.usage_scroll;
                self.usage_scroll = bucket;
                moved
            }
            _ => false,
        }
    }

    /// Toggle the click-expanded activity entry by its stable key: clicking the
    /// expanded row again collapses it, clicking a different row moves the
    /// expansion there.
    fn toggle_expand(&mut self, key: activity::ActivityKey) {
        if self.expanded_activity.as_ref() == Some(&key) {
            self.expanded_activity = None;
        } else {
            self.expanded_activity = Some(key);
        }
    }

    /// Open the full-input modal (UI-6 item 3) on the clicked entry's stable
    /// key, scrolled to the top. Re-opening on the same key resets the scroll.
    fn open_input_modal(&mut self, key: activity::ActivityKey) {
        self.input_modal = Some(InputModal::new(key));
    }

    /// Key handling while the input modal is open (UI-6 item 3). Esc/q/Enter
    /// close it; the arrows, `j`/`k`, PgUp/PgDn and Home/End scroll it within
    /// what the last frame drew (a page is the modal's own height). Every
    /// other key is swallowed so nothing leaks to MAIN beneath the modal.
    fn on_key_input_modal(&mut self, code: KeyCode) {
        if let Some(modal) = self.input_modal.as_mut() {
            if modal_key(&mut modal.state, code) {
                self.input_modal = None;
            }
        }
    }

    /// Key handling while the account detail modal is open (.prd/19 rule 6) —
    /// the same key set as [`Self::on_key_input_modal`]: Esc/q/Enter close,
    /// the arrows/PgUp/PgDn/Home/End scroll, everything else is swallowed so
    /// no key reaches the table beneath.
    fn on_key_account_modal(&mut self, code: KeyCode) {
        if let Some(modal) = self.account_modal.as_mut() {
            if modal_key(&mut modal.state, code) {
                self.account_modal = None;
            }
        }
    }

    /// Open the raw request/response viewer (UI-7) on the clicked entry: build
    /// the title + general metadata from the entry NOW (it may age out of the
    /// ring while we fetch), queue the background raw-io lookup, and show the
    /// modal in its Loading state. An entry from a pre-UI-7 daemon (`id == 0`)
    /// fails immediately — there is no raw correlation key to look up.
    fn open_raw_modal(
        &mut self,
        key: activity::ActivityKey,
        id: u64,
        view: Option<&DashboardView>,
    ) {
        // Select by the activity id, NOT just the key: the key omits the id,
        // so two requests completing in the same millisecond with the same
        // method/path/status would otherwise both resolve to the first match
        // and the viewer could show/export the wrong request's record. The id
        // is unique within a live view snapshot (per-process counter).
        let entry = view.and_then(|v| {
            v.completed
                .iter()
                .find(|c| {
                    matches!(&c.body, activity::CompletedBody::Request { id: eid, .. } if *eid == id)
                        && c.activity_key().as_ref() == Some(&key)
                })
                .cloned()
        });
        let Some(entry) = entry else {
            return; // row vanished between draw and click — nothing to open
        };
        let activity::CompletedBody::Request { id, .. } = entry.body else {
            return;
        };
        // A fresh open-generation invalidates any still-in-flight delivery for
        // the PRIOR modal (raw fetch or queued export).
        self.raw_generation = self.raw_generation.wrapping_add(1);
        let generation = self.raw_generation;
        let title = format!(
            " 🔍 raw — {} {} · {} · {} ",
            key.method,
            key.path,
            key.status,
            format::clock_hms_utc(entry.at),
        );
        let state = if id == 0 {
            RawModalState::Failed(
                "no raw link: this entry was recorded by a daemon predating the raw viewer"
                    .to_string(),
            )
        } else {
            // The curl builder needs the client base URL — the record stores
            // only bodies/headers. Local mode targets this process's own
            // listen port; attach mode the daemon it is attached to.
            let base_url = match &self.backend {
                Backend::Local(state) => {
                    // The ACTUAL listener port, not `config.proxy.port` — a
                    // `port = 0` config binds an OS-assigned port stored here,
                    // so the curl target must read `bound_port` or it would
                    // say `localhost:0`.
                    let port = state.bound_port.load(std::sync::atomic::Ordering::Relaxed);
                    format!("http://localhost:{port}")
                }
                Backend::Remote(remote) => remote.base_url.clone(),
            };
            self.pending_raw = Some(RawFetchReq {
                generation,
                id,
                at_ms: key.at_ms,
                general: ui::RawGeneral {
                    lines: ui::raw_general_lines(&entry),
                    method: key.method.clone(),
                    path: key.path.clone(),
                    base_url,
                },
            });
            RawModalState::Loading
        };
        self.raw_modal = Some(RawModal {
            key,
            id,
            generation,
            title,
            tab: 0,
            scroll: 0,
            view: ui::RawBodyView::default(),
            spin: 0,
            since: self.clock_at(Instant::now()),
            flash: None,
            state,
        });
        self.raw_drag.release();
    }

    /// Key handling while the raw viewer is open (UI-7/UI-8): Esc/q/Enter
    /// close, ←/→/Tab/h/l walk the payload tabs (the scroll resets — tabs
    /// have independent sizes), arrows/PgUp/PgDn/Home/End scroll (a page is
    /// the view's height, from the last frame), and c/C/a/s/S fire the
    /// copy/curl/copy-all/save/save-all actions (same as the top-right
    /// buttons). Long lines wrap at the width, so nothing pans sideways.
    /// Everything else is swallowed so nothing leaks beneath the modal.
    fn on_key_raw_modal(&mut self, code: KeyCode) {
        let Some(modal) = self.raw_modal.as_mut() else {
            return;
        };
        let tab_count = match &modal.state {
            RawModalState::Ready(content) => content.tabs.len().max(1),
            _ => 1,
        };
        let page = match self.raw_chrome.page {
            0 => usize::from(MODAL_PAGE),
            page => page,
        };
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.raw_modal = None,
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
                modal.tab = (modal.tab + 1) % tab_count;
                modal.scroll = 0;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                modal.tab = (modal.tab + tab_count - 1) % tab_count;
                modal.scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => modal.scroll = modal.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => modal.scroll = modal.scroll.saturating_add(1),
            KeyCode::PageUp => modal.scroll = modal.scroll.saturating_sub(page),
            KeyCode::PageDown => modal.scroll = modal.scroll.saturating_add(page),
            KeyCode::Home => modal.scroll = 0,
            KeyCode::End => modal.scroll = usize::MAX,
            KeyCode::Char('c') => self.raw_modal_action(ui::RawButton::Copy),
            KeyCode::Char('C') => self.raw_modal_action(ui::RawButton::CopyCurl),
            KeyCode::Char('a') => self.raw_modal_action(ui::RawButton::CopyAll),
            KeyCode::Char('s') => self.raw_modal_action(ui::RawButton::Save),
            KeyCode::Char('S') => self.raw_modal_action(ui::RawButton::SaveAll),
            _ => {}
        }
    }

    /// Queue one raw-viewer action button (UI-8) against the ACTIVE tab's
    /// prebuilt payloads: the export itself (clipboard subprocess / file
    /// write) runs on the blocking pool so a wedged clipboard tool or a slow
    /// Downloads mount can never freeze the TUI event loop (the payload can be
    /// tens of MiB). A not-yet-loaded modal flashes immediately; the real
    /// outcome flashes when the background task reports back, gated on the
    /// modal generation so a stale result never lands on a reopened modal.
    fn raw_modal_action(&mut self, btn: ui::RawButton) {
        let Some(modal) = self.raw_modal.as_mut() else {
            return;
        };
        let generation = modal.generation;
        let id = modal.id;
        let RawModalState::Ready(content) = &modal.state else {
            modal.flash = Some((StatusMsg::warn("raw record not loaded yet"), Instant::now()));
            return;
        };
        let Some(tab) = content
            .tabs
            .get(modal.tab.min(content.tabs.len().saturating_sub(1)))
        else {
            return;
        };
        let ext = |text: &str| {
            if text.trim_start().starts_with(['{', '[']) {
                "json"
            } else {
                "txt"
            }
        };
        // Move the (possibly large) payload string into the queued request —
        // built once here, consumed by the blocking task. No work on the UI
        // thread beyond this clone.
        let (payload, is_save, ext, label) = match btn {
            ui::RawButton::Copy => (tab.body_text.clone(), false, "", String::new()),
            ui::RawButton::CopyCurl => (tab.curl.clone(), false, "", String::new()),
            ui::RawButton::CopyAll => (content.all_text.clone(), false, "", String::new()),
            ui::RawButton::Save => (
                tab.body_text.clone(),
                true,
                ext(&tab.body_text),
                tab.label.to_lowercase().replace(' ', "-"),
            ),
            ui::RawButton::SaveAll => (content.record_json.clone(), true, "json", String::new()),
        };
        if self.pending_clip.len() >= CLIP_QUEUE_MAX {
            // Bounded: a wedged/slow exporter with a spamming user must not
            // grow the queue without limit. Reject the newest with feedback.
            modal.flash = Some((
                StatusMsg::warn("export busy — try again in a moment"),
                Instant::now(),
            ));
            return;
        }
        modal.flash = Some((StatusMsg::ok("working…"), Instant::now()));
        self.pending_clip.push_back(ClipReq {
            origin: ClipOrigin::Raw,
            generation,
            button: btn,
            id,
            payload,
            label,
            is_save,
            ext,
        });
    }

    /// Drain the queued export (event-loop side of [`Self::raw_modal_action`]).
    /// Single-flight: hand the event loop the next export ONLY when none is
    /// running, marking the slot busy. Cleared by [`Self::clip_finished`] when
    /// the result arrives, so exports run one at a time, in FIFO press order.
    fn next_clip_if_idle(&mut self) -> Option<ClipReq> {
        if self.clip_inflight {
            return None;
        }
        let req = self.pending_clip.pop_front()?;
        self.clip_inflight = true;
        Some(req)
    }

    /// Mark the in-flight export done (its result arrived), freeing the next.
    fn clip_finished(&mut self) {
        self.clip_inflight = false;
    }

    /// Resolve a delivered export outcome onto the open modal's flash line,
    /// gated on the open-generation so a stale result never lands on a
    /// reopened modal.
    fn apply_clip_result(&mut self, result: ClipResult) {
        if let ClipOrigin::Logs { .. } = result.origin {
            self.set_status(result.message);
            return;
        }
        if let Some(modal) = self.raw_modal.as_mut() {
            if modal.generation == result.generation {
                modal.flash = Some((result.message, Instant::now()));
            }
        }
    }

    /// Run one queued export on the blocking pool and deliver the flash
    /// message on the clip channel. Never touches the UI thread; the payload
    /// was already built at queue time.
    fn spawn_clip(&mut self, req: ClipReq) {
        let Some(tx) = self.clip_tx.clone() else {
            return;
        };
        let ClipReq {
            origin,
            generation,
            button,
            id,
            payload,
            label,
            is_save,
            ext,
        } = req;
        tokio::task::spawn_blocking(move || {
            let message = if is_save {
                let stem = if label.is_empty() {
                    format!("llmux-raw-{id}")
                } else {
                    format!("llmux-raw-{id}-{label}")
                };
                match clip::save(&stem, ext, &payload) {
                    Ok(path) => StatusMsg::ok(match button {
                        ui::RawButton::SaveAll => format!("saved record → {path}"),
                        _ => format!("saved → {path}"),
                    }),
                    Err(err) => StatusMsg::fail(err),
                }
            } else if let ClipOrigin::Logs { lines } = origin {
                match clip::copy(&payload) {
                    Ok(dest) if lines == 1 => StatusMsg::ok(format!("copied 1 line → {dest}")),
                    Ok(dest) => StatusMsg::ok(format!("copied {lines} lines → {dest}")),
                    Err(err) => StatusMsg::fail(format!("copy failed: {err}")),
                }
            } else {
                let n = payload.len();
                match clip::copy(&payload) {
                    Ok(dest) => StatusMsg::ok(match button {
                        ui::RawButton::CopyCurl => format!("copied curl ({n} bytes) → {dest}"),
                        ui::RawButton::CopyAll => format!("copied all {n} bytes → {dest}"),
                        _ => format!("copied {n} bytes → {dest}"),
                    }),
                    Err(err) => StatusMsg::fail(err),
                }
            };
            let _ = tx.blocking_send(ClipResult {
                origin,
                generation,
                message,
            });
        });
    }

    /// Drain the queued raw fetch (event-loop side of [`Self::open_raw_modal`]).
    fn take_pending_raw(&mut self) -> Option<RawFetchReq> {
        self.pending_raw.take()
    }

    /// Resolve a delivered raw load into the open modal. Ignored when the modal
    /// was closed or re-targeted while the fetch ran (stale delivery).
    fn apply_raw_load(&mut self, load: RawLoad) {
        if let Some(modal) = self.raw_modal.as_mut() {
            // Gate on the open-generation, not the key: the key omits the
            // activity id, so a close→reopen of the same row (or two same-ms
            // requests) could otherwise let a stale fetch's late result — even
            // a stale 404 — clobber the reopened modal.
            if modal.generation == load.generation && matches!(modal.state, RawModalState::Loading)
            {
                modal.state = match load.result {
                    Ok(content) => RawModalState::Ready(content),
                    Err(msg) => RawModalState::Failed(msg),
                };
            }
        }
    }

    /// Spawn the background raw-record fetch for `req` and deliver the result
    /// Run one queued keys-usage query and deliver the answer on the keys
    /// channel (keys-history K-5). NEITHER path runs on the event loop: local
    /// goes to the blocking pool (SQLite), attach spawns an HTTP GET. The
    /// document is named + priced where the data lives, so both backends
    /// render byte-identical rows.
    fn spawn_keys_query(&mut self, req: KeysQuery) {
        let Some(tx) = self.keys_tx.clone() else {
            return; // unit tests drive the panel state directly
        };
        let KeysQuery {
            generation,
            window,
            models,
        } = req;
        let now_ms = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        match &self.backend {
            Backend::Local(state) => {
                let query = crate::key_usage::UsageQuery::new(window, now_ms).with_models(models);
                let Some(store) = state.hub.usage_store() else {
                    // No durable store on this daemon — say so; do NOT fall
                    // back to the in-memory lifetime aggregates.
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let _ = tx
                            .send(KeysLoad {
                                generation,
                                result: Err(
                                    "durable keys usage is unavailable (no config path or the \
                                     store failed to open)"
                                        .to_string(),
                                ),
                            })
                            .await;
                    });
                    return;
                };
                let keys = crate::dashboard::key_row_docs(state);
                let overrides = state.config.pricing.clone();
                tokio::task::spawn_blocking(move || {
                    let result = store
                        .query(&query)
                        .map(|report| {
                            crate::key_usage::usage_doc(
                                report,
                                &keys,
                                &overrides,
                                store.health(),
                                now_ms,
                            )
                        })
                        .map_err(|err| err.to_string());
                    let _ = tx.blocking_send(KeysLoad { generation, result });
                });
            }
            Backend::Remote(remote) => {
                let client = remote.client.clone();
                let api_key = remote.api_key.clone();
                let url = format!(
                    "{}/llmux/keys/usage?window={}&models={}",
                    remote.base_url,
                    window.as_str(),
                    models
                        .iter()
                        .map(|m| encode_query_value(m))
                        .collect::<Vec<_>>()
                        .join(",")
                );
                tokio::spawn(async move {
                    let mut request = client.get(&url);
                    if let Some(key) = &api_key {
                        request = request.header("x-api-key", key);
                    }
                    let result = match request.send().await {
                        Ok(response) if response.status().is_success() => response
                            .json::<crate::key_usage::KeysUsageDoc>()
                            .await
                            .map_err(|err| format!("keys usage decode failed: {err}")),
                        Ok(response) => {
                            let status = response.status();
                            let body = response.text().await.unwrap_or_default();
                            let detail = serde_json::from_str::<serde_json::Value>(&body)
                                .ok()
                                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                                .unwrap_or_else(|| status.to_string());
                            Err(format!("keys usage failed: {detail}"))
                        }
                        Err(err) => Err(format!("keys usage failed: {err}")),
                    };
                    let _ = tx.send(KeysLoad { generation, result }).await;
                });
            }
        }
    }

    /// on the raw channel (never blocks the event loop — the local path is a
    /// backwards file scan on the blocking pool, the attach path an HTTP GET to
    /// `GET /llmux/raw-io`). Content lines are built in the task too: a Ready
    /// body can be megabytes.
    fn spawn_raw_fetch(&mut self, req: RawFetchReq) {
        let Some(tx) = self.raw_tx.clone() else {
            return;
        };
        let RawFetchReq {
            generation,
            id,
            general,
            at_ms,
        } = req;
        enum Source {
            Local(Option<std::path::PathBuf>),
            Remote {
                client: reqwest::Client,
                url: String,
                api_key: Option<String>,
            },
        }
        let source = match &self.backend {
            Backend::Local(state) => Source::Local(
                state
                    .config
                    .raw_io
                    .enabled
                    .then(|| state.raw_io_path.clone())
                    .flatten(),
            ),
            Backend::Remote(remote) => Source::Remote {
                client: remote.client.clone(),
                url: format!("{}/llmux/raw-io?id={id}&at_ms={at_ms}", remote.base_url),
                api_key: remote.api_key.clone(),
            },
        };
        tokio::spawn(async move {
            let not_found = || {
                "no raw-io record for this request (capture disabled, pruned, or the daemon \
                 restarted since)"
                    .to_string()
            };
            let record = match source {
                Source::Local(path) => tokio::task::spawn_blocking(move || {
                    crate::proxy::raw_io::find_record(path.as_deref(), id, at_ms)
                })
                .await
                .map_err(|err| format!("raw lookup task failed: {err}"))
                .and_then(|found| found.ok_or_else(not_found)),
                Source::Remote {
                    client,
                    url,
                    api_key,
                } => {
                    let mut request = client.get(&url);
                    if let Some(k) = &api_key {
                        request = request.header("x-api-key", k);
                    }
                    match request.send().await {
                        Ok(response) if response.status().is_success() => response
                            .json::<crate::proxy::raw_io::RawIoRecord>()
                            .await
                            .map_err(|err| format!("raw-io response parse failed: {err}")),
                        Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => {
                            Err(not_found())
                        }
                        Ok(response) => Err(format!("raw-io fetch failed: {}", response.status())),
                        Err(err) => Err(format!("raw-io fetch failed: {err}")),
                    }
                }
            };
            let result = tokio::task::spawn_blocking(move || {
                record.map(|record| {
                    std::sync::Arc::new(ui::raw_content_from_record(general, &record))
                })
            })
            .await
            .unwrap_or_else(|err| Err(format!("raw render task failed: {err}")));
            let _ = tx.send(RawLoad { generation, result }).await;
        });
    }

    /// Toggle the click-opened folded `count` run by any member's stable key
    /// (UI-5): the marker opens a closed group and closes an open one.
    fn toggle_run(&mut self, key: activity::ActivityKey) {
        if self.expanded_run.as_ref() == Some(&key) {
            self.expanded_run = None;
        } else {
            self.expanded_run = Some(key);
        }
    }

    /// Key handling for the Stats overlay (`g`). Arrows/`j`/`k` move the cursor
    /// through model rows; `g`/`Esc` closes back to MAIN; `q` quits.
    fn on_key_stats(&mut self, code: KeyCode, view: Option<&DashboardView>) {
        let len = view.map_or(0, |v| v.model_usage.len());
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('g') | KeyCode::Esc => self.overlay = Overlay::None,
            // Cycle the heatmap window 24h ↔ 72h (issue #23).
            KeyCode::Char('w') => self.stats_window = self.stats_window.next(),
            // Cycle the tokens-per-day chart span (UI-3 U14).
            KeyCode::Char('d') => {
                let spans = ui::DAILY_CHART_SPANS;
                let next = spans
                    .iter()
                    .position(|d| *d == self.chart_days)
                    .map(|i| (i + 1) % spans.len())
                    .unwrap_or(0);
                self.chart_days = spans[next];
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_model_cursor(-1, len),
            KeyCode::Down | KeyCode::Char('j') => self.move_model_cursor(1, len),
            KeyCode::PageUp => self.move_model_cursor(-10, len),
            KeyCode::PageDown => self.move_model_cursor(10, len),
            KeyCode::Home => self.model_cursor = 0,
            KeyCode::End => self.model_cursor = len.saturating_sub(1),
            _ => {}
        }
    }

    /// Key handling for the Logs overlay (`l`). `l`/`Esc` closes back to MAIN;
    /// `q` quits. The tail is full-screen, so there is no size cycle anymore.
    fn on_key_logs(&mut self, code: KeyCode) {
        let code = match code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('l') | KeyCode::Esc => {
                self.overlay = Overlay::None;
                self.logs_sel = None;
                return;
            }
            KeyCode::Char('k') => KeyCode::Up,
            KeyCode::Char('j') => KeyCode::Down,
            other => other,
        };
        if let Some(key) = key_input(code) {
            self.scroll_logs_to(|pos| pos.on_key(key));
        }
    }

    /// Open the Logs overlay at the newest line, following new ones, with
    /// nothing selected.
    fn open_logs(&mut self) {
        self.logs_scroll = None;
        self.logs_sel = None;
        self.overlay = Overlay::Logs;
    }

    /// A left-button press, drag or release over the Logs pager: a drag
    /// selects text with twolab-tui's `Pager` selection (from herdr) and the
    /// release copies it ([`Self::queue_logs_copy`]); a plain click clears
    /// it. The pager is rebuilt from this drain's lines and painted
    /// off-screen at the rect it drew in last, so its mouse methods map the
    /// pointer onto the cells the person saw. Once the drag shows a
    /// selection the view holds still (new lines must not scroll it out from
    /// under the pointer); a drag past the top or bottom edge scrolls.
    /// `None`: not a left-button event, or no pager on screen.
    fn logs_select_mouse(
        &mut self,
        mouse: &crossterm::event::MouseEvent,
        view: Option<&DashboardView>,
    ) -> Option<bool> {
        let (area, view) = (self.logs_area?, view?);
        let (col, row) = (mouse.column, mouse.row);
        if !matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left)
                | MouseEventKind::Up(MouseButton::Left)
        ) {
            return None;
        }
        let before = (self.logs_sel, self.logs_scroll);
        let mut pager = ui::logs_pager(view, self.logs_scroll, self.logs_sel);
        let mut scratch = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&pager, area, &mut scratch);
        let changed = match mouse.kind {
            MouseEventKind::Down(_) => pager.mouse_down(col, row),
            MouseEventKind::Drag(_) => pager.mouse_drag(col, row),
            _ => {
                if pager.mouse_up(col, row) {
                    if let Some(text) = pager.selected_text().filter(|t| !t.trim().is_empty()) {
                        self.queue_logs_copy(text);
                    }
                }
                true
            }
        };
        self.logs_sel = pager.selection().copied();
        if self.logs_sel.is_some_and(|s| s.is_visible()) {
            self.logs_scroll = Some(pager.scroll_offset());
        }
        Some(changed || (self.logs_sel, self.logs_scroll) != before)
    }

    /// Queue the Logs selection for the clipboard on the raw viewer's export
    /// queue: [`clip::copy`] (the platform tool, else OSC 52 wrapped for
    /// tmux or screen) on the blocking pool; the outcome lands on the status
    /// line, naming how many lines went where.
    fn queue_logs_copy(&mut self, text: String) {
        if self.pending_clip.len() >= CLIP_QUEUE_MAX {
            self.set_status(StatusMsg::warn("copy busy — try again in a moment"));
            return;
        }
        self.pending_clip.push_back(ClipReq {
            origin: ClipOrigin::Logs {
                lines: text.lines().count(),
            },
            generation: 0,
            button: ui::RawButton::Copy,
            id: 0,
            payload: text,
            label: String::new(),
            is_save: false,
            ext: "",
        });
    }

    /// Move the Logs pager to the row `to` picks from where it was drawn
    /// last. Reaching the bottom follows the tail again; `None` from `to`
    /// leaves the view alone.
    fn scroll_logs_to(&mut self, to: impl FnOnce(ScrollPos) -> Option<usize>) {
        let pos = self.logs_pos;
        let pos = pos.with_top(self.logs_scroll.unwrap_or(pos.max));
        if let Some(top) = to(pos) {
            self.logs_scroll = (top < pos.max).then_some(top);
        }
    }

    /// Key handling for the context menu (UI-3 U11), by the library's
    /// `MenuState`: ↑/k ↓/j move over the items that can run, Home/End jump,
    /// Enter runs the highlighted entry, an entry's own key (`p`, `L`, `r`)
    /// runs it, Esc — or any other key — dismisses. The items (and which of
    /// them can run) are rebuilt from the pinned account exactly as the
    /// frame drew them (`ui::account_menu_items`).
    fn on_key_context_menu(
        &mut self,
        code: KeyCode,
        idx: usize,
        item: usize,
        view: Option<&DashboardView>,
    ) {
        use twolab_tui::menu::{Menu, MenuAction, MenuState};
        let pinned = self.menu_account.clone();
        let order = view.map_or_else(Vec::new, |v| {
            v.display_order(self.account_sort, SystemTime::now())
        });
        let target = view.map(|v| ui::menu_target(v, pinned.as_deref(), &order, idx));
        let account = target.and_then(|t| t.ok().flatten());
        let menu = Menu::new(ui::account_menu_items(account));
        let mut state = MenuState {
            selected: item,
            ..MenuState::default()
        };
        match key_input(code).map(|key| state.on_key(&menu, key)) {
            Some(MenuAction::Moved | MenuAction::Ignored) => {
                self.mode = Mode::ContextMenu {
                    idx,
                    item: state.selected,
                };
            }
            Some(MenuAction::Run(entry)) => self.run_menu_item(idx, entry, view),
            Some(MenuAction::Close) | None => {
                // Enter on a menu with nothing to run: the pinned account
                // left the pool — say so instead of closing in silence.
                self.close_menu();
                if let (KeyCode::Enter, Some(Err(name))) = (code, target) {
                    self.set_status(StatusMsg::warn(format!(
                        "{name} is gone — menu action cancelled"
                    )));
                }
            }
        }
    }

    /// Dismiss the context menu.
    fn close_menu(&mut self) {
        self.mode = Mode::Normal;
        self.menu_anchor = None;
        self.menu_account = None;
    }

    /// Run one context-menu entry against the account PINNED at menu-open
    /// time. The display index is re-resolved from the pinned account id at
    /// execution (rows reorder as live quota state changes); a vanished
    /// account aborts with a status instead of acting on whoever moved into
    /// the row. Every action reuses the exact key-flow path (switch / pause /
    /// limits editor / remove confirm), so local and attach behave
    /// identically.
    fn run_menu_item(&mut self, fallback_idx: usize, item: usize, view: Option<&DashboardView>) {
        let pinned = self.menu_account.clone();
        self.close_menu();
        let idx = match (&pinned, view) {
            (Some(name), Some(v)) => {
                let order = v.display_order(self.account_sort, SystemTime::now());
                match order
                    .iter()
                    .position(|&i| v.snapshot.accounts.get(i).is_some_and(|a| a.id.0 == *name))
                {
                    Some(pos) => pos,
                    None => {
                        self.set_status(StatusMsg::warn(format!(
                            "{name} is gone — menu action cancelled"
                        )));
                        return;
                    }
                }
            }
            _ => fallback_idx,
        };
        match item {
            0 => self.try_manual_switch(idx, view),
            1 => self.toggle_pause_selected(idx, view),
            2 => {
                self.limits_from_menu = true;
                self.open_limits_editor(idx, view);
            }
            // Destructive delete keeps its confirm gate (y/N) — never silent.
            3 => self.mode = Mode::ConfirmRemove { idx },
            _ => {}
        }
    }

    /// Key handling for the Perf overlay (`p`, perf telemetry v1): `d`
    /// cycles the day span, arrows/`j`/`k` move the series cursor, `p`/`Esc`
    /// closes, `q` quits.
    fn on_key_perf(&mut self, code: KeyCode, view: Option<&DashboardView>) {
        let rows = view
            .map(|v| ui::perf_series_count(v, self.perf_days, self.perf_day_off))
            .unwrap_or(0);
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('p') | KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Char('d') => {
                let spans = ui::DAILY_CHART_SPANS;
                let next = spans
                    .iter()
                    .position(|d| *d == self.perf_days)
                    .map(|i| (i + 1) % spans.len())
                    .unwrap_or(0);
                self.perf_days = spans[next];
                self.perf_cursor = 0;
                self.perf_day_off = None;
            }
            // Day drill-down (contract C5: per-day model×fast detail):
            // ←/h walks back a day, →/l walks forward; walking past today
            // returns to the whole-span aggregate.
            KeyCode::Left | KeyCode::Char('h') => {
                let max_off = self.perf_days.saturating_sub(1);
                self.perf_day_off = Some(match self.perf_day_off {
                    None => 0,
                    Some(off) => (off + 1).min(max_off),
                });
                self.perf_cursor = 0;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.perf_day_off = match self.perf_day_off {
                    None | Some(0) => None,
                    Some(off) => Some(off - 1),
                };
                self.perf_cursor = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.perf_cursor = self.perf_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.perf_cursor = (self.perf_cursor + 1).min(rows.saturating_sub(1));
            }
            _ => {}
        }
    }

    /// Key handling for the Misc overlay (`?`, UI-3 U6). `?`/`Esc` closes.
    fn on_key_misc(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') | KeyCode::Esc => self.overlay = Overlay::None,
            _ => {}
        }
    }

    /// Open the Keys overlay (keys-history K-0) and ask the durable store for
    /// the CURRENT (window, models) answer. Reopening re-queries — the panel
    /// is a point-in-time answer, never a stale one wearing a fresh label.
    fn open_keys(&mut self) {
        self.overlay = Overlay::Keys;
        self.keys.scroll = 0;
        self.keys.picker = None;
        self.request_keys_usage();
    }

    /// Queue a keys-usage query for the event loop. Bumps the generation so a
    /// slower earlier answer can never overwrite a newer one.
    ///
    /// If the question CHANGED (window or model filter), the previous answer is
    /// dropped right here: the title renders the new question immediately, so
    /// keeping the old rows would label totals with a question they never
    /// answered. A refresh of the SAME question keeps its rows (the label still
    /// describes exactly what is drawn).
    fn request_keys_usage(&mut self) {
        let models: Vec<String> = self.keys.models.iter().cloned().collect();
        let answers_this_question = self
            .keys
            .doc
            .as_ref()
            .is_some_and(|doc| doc.window == self.keys.window.as_str() && doc.models == models);
        if !answers_this_question {
            self.keys.doc = None;
        }
        self.keys.generation += 1;
        if !self.keys.loading {
            self.keys.loading_since = self.clock_at(Instant::now());
        }
        self.keys.loading = true;
        self.keys.error = None;
        self.pending_keys = Some(KeysQuery {
            generation: self.keys.generation,
            window: self.keys.window,
            models,
        });
    }

    fn take_pending_keys(&mut self) -> Option<KeysQuery> {
        self.pending_keys.take()
    }

    /// Apply one delivered answer. Stale generations are dropped.
    fn apply_keys_load(&mut self, load: KeysLoad) {
        if load.generation != self.keys.generation {
            return;
        }
        self.keys.loading = false;
        match load.result {
            Ok(doc) => {
                self.keys.available = doc.available_models.clone();
                self.keys.doc = Some(std::sync::Arc::new(doc));
                self.keys.error = None;
                // An open picker offers what this answer observed: opened
                // during the first load it used to say "no models observed"
                // over a panel full of models.
                if let Some(working) = self.keys.picker.as_ref().map(KeysPicker::selected) {
                    let options = self.keys_picker_options(&working);
                    if let Some(picker) = self.keys.picker.as_mut() {
                        picker.rebuild(options, &working);
                    }
                }
            }
            // The previous answer is DROPPED on failure: showing older rows
            // under the new filter label is exactly the lie this panel must
            // not tell.
            Err(err) => {
                self.keys.doc = None;
                self.keys.error = Some(err);
            }
        }
    }

    /// Open the `f` model picker over the models the current answer observed,
    /// unioned with the current selection.
    fn open_keys_picker(&mut self) {
        let options = self.keys_picker_options(&self.keys.models);
        self.keys.picker = Some(KeysPicker::new(options, &self.keys.models));
    }

    /// What the picker offers: the models the last answer observed, unioned
    /// with the applied filter and the `working` selection.
    fn keys_picker_options(&self, working: &std::collections::BTreeSet<String>) -> Vec<String> {
        let mut options: Vec<String> = self.keys.available.clone();
        options.extend(self.keys.models.iter().cloned());
        options.extend(working.iter().cloned());
        options.sort();
        options.dedup();
        options
    }

    /// Key handling for the Keys overlay (keys-history K-0): `w` cycles the
    /// window, `f` opens the multi-select model picker, the arrows/Page/Home/
    /// End scroll EVERY flattened row (key rows and their model rows), `r`
    /// re-runs the query, `K`/`Esc` closes, `q` quits. Key MUTATIONS stay in
    /// the CLI (`llmux key …`), matching the admin-filter decision (option A).
    fn on_key_keys(&mut self, code: KeyCode, view: Option<&DashboardView>) {
        if self.keys.picker.is_some() {
            return self.on_key_keys_picker(code);
        }
        let rows = view
            .map(|v| ui::keys_row_count(v, &self.chrome()))
            .unwrap_or(0);
        let last = rows.saturating_sub(1);
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('K') | KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Char('w') => {
                self.keys.window = self.keys.window.next();
                self.keys.scroll = 0;
                self.request_keys_usage();
            }
            KeyCode::Char('f') => self.open_keys_picker(),
            KeyCode::Char('r') => self.request_keys_usage(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.keys.scroll = self.keys.scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.keys.scroll = (self.keys.scroll + 1).min(last);
            }
            KeyCode::PageUp => self.keys.scroll = self.keys.scroll.saturating_sub(KEYS_PAGE),
            KeyCode::PageDown => self.keys.scroll = (self.keys.scroll + KEYS_PAGE).min(last),
            KeyCode::Home => self.keys.scroll = 0,
            KeyCode::End => self.keys.scroll = last,
            _ => {}
        }
    }

    /// Key handling inside the `f` picker ([`KeysPicker`]): arrows and
    /// `j`/`k` move, Space toggles, `a` checks every offered model, `c`
    /// clears, `/` starts the filter, Enter applies (and re-queries),
    /// `Esc`/`f` cancel without touching the active filter, `q` quits. While
    /// the filter is open printable keys type into it, Backspace on an empty
    /// filter or Esc closes it (the picker stays), and Space/Enter/arrows
    /// keep working.
    fn on_key_keys_picker(&mut self, code: KeyCode) {
        use twolab_tui::multi_select::Outcome;
        let Some(picker) = self.keys.picker.as_mut() else {
            return;
        };
        let code = if picker.filtering {
            match code {
                KeyCode::Esc => {
                    picker.list.set_search_query("");
                    picker.filtering = false;
                    return;
                }
                KeyCode::Backspace if picker.list.search_query().is_empty() => {
                    picker.filtering = false;
                    return;
                }
                code => code,
            }
        } else {
            match code {
                KeyCode::Char('q') => {
                    self.should_quit = true;
                    return;
                }
                KeyCode::Esc | KeyCode::Char('f') => {
                    self.keys.picker = None;
                    return;
                }
                KeyCode::Char('a') => return picker.set_all(true),
                KeyCode::Char('c') => return picker.set_all(false),
                KeyCode::Char('/') => {
                    picker.filtering = true;
                    return;
                }
                KeyCode::Char('k') => KeyCode::Up,
                KeyCode::Char('j') => KeyCode::Down,
                // Letters do not type until `/` opens the filter.
                KeyCode::Char(c) if c != ' ' => return,
                KeyCode::Backspace => return,
                code => code,
            }
        };
        let Some(key) = key_input(code) else {
            return;
        };
        match picker.list.key(key) {
            Outcome::Applied(models) => {
                self.keys.models = models.into_iter().collect();
                self.keys.picker = None;
                self.keys.scroll = 0;
                self.request_keys_usage();
            }
            Outcome::Cancelled => self.keys.picker = None,
            Outcome::Changed | Outcome::Ignored => {}
        }
    }

    /// Key handling for the Config overlay (`c`, config-editor v1): arrows
    /// (and `j`/`k`, PgUp/PgDn, Home/End) move the cursor over the editable
    /// rows — read-only rows are skipped — and stop at the ends; Enter
    /// activates the row (toggle / cycle / edit prompt), `c`/`Esc` closes.
    fn on_key_config(&mut self, code: KeyCode, view: Option<&DashboardView>) {
        let code = match code {
            KeyCode::Char('k') => KeyCode::Up,
            KeyCode::Char('j') => KeyCode::Down,
            code => code,
        };
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') | KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End => {
                if let (Some(view), Some(key)) = (view, key_input(code)) {
                    let chrome = self.chrome();
                    self.config_cursor = ui::config_cursor_after(view, &chrome, key);
                }
            }
            KeyCode::Enter => {
                let action = view.and_then(|v| {
                    let chrome = self.chrome();
                    ui::config_row_action(v, &chrome, self.config_cursor)
                });
                if let Some(action) = action {
                    self.activate_config_action(action, view);
                }
            }
            _ => {}
        }
    }

    /// Activate one config row: toggles/cycles act (through the confirm gate
    /// when the action is blast-radius); value rows open the edit prompt
    /// prefilled with the current value.
    fn activate_config_action(&mut self, action: ui::ConfigAction, view: Option<&DashboardView>) {
        use ui::ConfigAction as A;
        // Existing live paths keep their own flows (no settings round-trip).
        match action {
            A::CodexModel => return self.cycle_codex_model(view),
            A::CodexEffort => return self.cycle_codex_effort(view),
            A::CodexFast => return self.toggle_codex_fast(view),
            A::GrokEffort => return self.cycle_grok_effort(view),
            A::ResetDisplay => return self.toggle_reset_display(),
            _ => {}
        }
        if action.input_hint().is_some() {
            // Prefill with the current value where that is cheap to read;
            // the caret starts at its end.
            let current = view
                .map(|v| {
                    let f = &v.config_facts;
                    let p = &v.select_params;
                    match action {
                        A::Ceiling5h => format!("{:.0}", p.five_hour_max * 100.0),
                        A::Ceiling7d => format!("{:.0}", p.seven_day_max * 100.0),
                        A::CeilingFbl => format!("{:.0}", p.fable_weekly_max * 100.0),
                        A::UsageMaxAge => p.usage_max_age.as_secs().to_string(),
                        A::GradientSpeed => format!("{:.2}", f.gradient_speed),
                        A::RoutingDefaultGroup => f.routing_default_group.clone(),
                        A::RoutingOnEmptyGroup => f.routing_on_empty_group.clone(),
                        A::RawIoRetention => f.raw_io_retention_days.to_string(),
                        A::RawIoMaxBody => f.raw_io_max_body_bytes.to_string(),
                        A::Upstream => v.upstream.clone().unwrap_or_default(),
                        A::CodexUpstream => f.codex_upstream.clone(),
                        A::ProxyPort => v.port.to_string(),
                        A::ProxyMaxBody => f.proxy_max_request_bytes.to_string(),
                        _ => String::new(),
                    }
                })
                .unwrap_or_default();
            self.field = ComposerState::with_text(&current).single_line();
            self.mode = Mode::ConfigEdit { action };
            return;
        }
        // Scheduler mode applies through its own live path
        // (toggle_scheduler_mode) — enter the confirm gate directly.
        if action == ui::ConfigAction::SchedMode {
            self.open_config_confirm(
                action,
                &crate::proxy::server::SettingsRequest::default(),
                view,
            );
            self.config_pending = None;
            self.config_pending_value = None;
            return;
        }
        // Toggle actions: build the request from the CURRENT view value.
        let req = self.toggle_settings_request(action, view);
        let Some(req) = req else {
            return self.set_status(StatusMsg::fail("config: value unavailable"));
        };
        if action.needs_confirm() {
            self.open_config_confirm(action, &req, view);
            self.config_pending = Some(req);
        } else {
            self.send_settings(req, None, view);
        }
    }

    /// Open the confirm gate for `req`: the question names the setting and
    /// its current → new value, and the mark starts on Apply (Enter always
    /// applied on this gate).
    fn open_config_confirm(
        &mut self,
        action: ui::ConfigAction,
        req: &crate::proxy::server::SettingsRequest,
        view: Option<&DashboardView>,
    ) {
        let old = self.config_current_value(action, view);
        self.config_input = confirm_prompt(action, req, &old, view);
        self.confirm_choice = 0;
        self.mode = Mode::ConfigConfirm { action };
    }

    /// The value a config row holds now, as the gate's "old" side: the value
    /// saved this session (restart rows) when there is one, else what the
    /// editor shows.
    fn config_current_value(
        &self,
        action: ui::ConfigAction,
        view: Option<&DashboardView>,
    ) -> String {
        let saved = self.config_saved.get(&action).cloned();
        if action == ui::ConfigAction::RawIoRetention {
            return saved
                .and_then(|d| d.parse::<u64>().ok())
                .or_else(|| view.map(|v| v.config_facts.raw_io_retention_days))
                .map_or_else(|| "?".into(), retention_label);
        }
        saved
            .or_else(|| view.and_then(|v| ui::config_row_value(v, &self.chrome(), action)))
            .unwrap_or_else(|| "?".into())
    }

    /// Build the SettingsRequest a TOGGLE row produces (inverse of the
    /// current view value). `None` when the view is not available.
    fn toggle_settings_request(
        &self,
        action: ui::ConfigAction,
        view: Option<&DashboardView>,
    ) -> Option<crate::proxy::server::SettingsRequest> {
        use crate::proxy::server::SettingsRequest;
        use ui::ConfigAction as A;
        let view = view?;
        let mut req = SettingsRequest::default();
        match action {
            A::EmailMask => req.email_anonymous = Some(!view.email_anonymous),
            A::QuotaFill => {
                let cur = self.quota_display_override.unwrap_or(view.quota_display);
                req.quota_display = Some(match cur {
                    crate::config::QuotaDisplay::Used => "remaining".into(),
                    crate::config::QuotaDisplay::Remaining => "used".into(),
                });
            }
            A::TuiEffects => req.tui_effects = Some(!view.tui_effects),
            A::FableWeekly => req.show_fable_weekly = Some(!view.show_fable_weekly),
            A::RoutingEnabled => req.routing_enabled = Some(!view.config_facts.routing_enabled),
            A::RawIoEnabled => req.raw_io_enabled = Some(!view.config_facts.raw_io_enabled),
            _ => return None,
        }
        Some(req)
    }

    /// Keys while typing a config value (Mode::ConfigEdit): Enter parses and
    /// applies (or opens the confirm gate), Esc cancels, every other key
    /// edits the field ([`Self::edit_field`]).
    fn on_key_config_edit(
        &mut self,
        key: KeyEvent,
        action: ui::ConfigAction,
        view: Option<&DashboardView>,
    ) {
        match key.code {
            KeyCode::Esc => {
                self.close_field();
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let raw = self.field.take();
                self.close_field();
                self.mode = Mode::Normal;
                match parse_config_input(action, raw.trim(), view) {
                    Err(err) => self.set_status(StatusMsg::fail(format!("config: {err}"))),
                    Ok(req) => {
                        // Blast-radius confirms: endpoint changes always; a
                        // ceiling driven to 0 (locks scheduling out); a
                        // retention decrease (deletes history).
                        let needs = action.needs_confirm()
                            || matches!(
                                action,
                                ui::ConfigAction::Ceiling5h
                                    | ui::ConfigAction::Ceiling7d
                                    | ui::ConfigAction::CeilingFbl
                            ) && [req.five_hour_max, req.seven_day_max, req.fable_weekly_max]
                                .iter()
                                .flatten()
                                .any(|v| *v == 0.0)
                            || action == ui::ConfigAction::RawIoRetention && {
                                // Compare against the latest SAVED value when
                                // one is pending — the boot value would
                                // misjudge repeat edits in one session.
                                let baseline = self
                                    .config_saved
                                    .get(&ui::ConfigAction::RawIoRetention)
                                    .and_then(|v| v.parse::<u64>().ok())
                                    .or_else(|| view.map(|v| v.config_facts.raw_io_retention_days));
                                baseline.is_some_and(|base| {
                                    req.raw_io_retention_days.is_some_and(|days| days < base)
                                })
                            };
                        if needs {
                            self.open_config_confirm(action, &req, view);
                            self.config_pending = Some(req);
                            self.config_pending_value = Some(raw.trim().to_string());
                        } else {
                            self.send_settings(req, Some((action, raw.trim().to_string())), view);
                        }
                    }
                }
            }
            _ => {
                self.edit_field(key, |_| true);
            }
        }
    }

    /// Keys on the confirm gate (Mode::ConfigConfirm): `1` / `y` apply the
    /// pending request, `2` / `n` / Esc cancel with no change, ↑↓ move the
    /// mark and Enter answers it (Apply until moved — Enter always applied
    /// here). Other keys do nothing.
    fn on_key_config_confirm(
        &mut self,
        code: KeyCode,
        action: ui::ConfigAction,
        view: Option<&DashboardView>,
    ) {
        match gate_answer(code, self.confirm_choice) {
            GateAnswer::Confirm => {
                self.config_input.clear();
                self.mode = Mode::Normal;
                if action == ui::ConfigAction::SchedMode {
                    self.config_pending = None;
                    self.toggle_scheduler_mode(view);
                    return;
                }
                if let Some(req) = self.config_pending.take() {
                    let origin = self.config_pending_value.take().map(|v| (action, v));
                    self.send_settings(req, origin, view);
                }
            }
            GateAnswer::Cancel => {
                self.config_input.clear();
                self.config_pending = None;
                self.config_pending_value = None;
                self.mode = Mode::Normal;
                self.set_status(StatusMsg::warn("config: cancelled"));
            }
            GateAnswer::Moved(choice) => self.confirm_choice = choice,
            GateAnswer::Ignored => {}
        }
    }

    /// Apply a settings request: locally through the daemon's own
    /// `apply_settings` (identical validation + persist + live flip), or
    /// queued for the event loop to POST in attach mode. The status line
    /// reports live-applied vs restart-required honestly.
    fn send_settings(
        &mut self,
        req: crate::proxy::server::SettingsRequest,
        origin: Option<(ui::ConfigAction, String)>,
        _view: Option<&DashboardView>,
    ) {
        match &mut self.backend {
            Backend::Local(state) => match crate::proxy::server::apply_settings(state, &req) {
                Ok((applied, restart)) => {
                    // A persisted quota-fill change supersedes any session
                    // `u` override — otherwise the screen keeps the stale
                    // override while claiming "applied" (review MUST-FIX 5).
                    if req.quota_display.is_some() {
                        self.quota_display_override = None;
                    }
                    // Restart-only saves get a visible pending note on their
                    // row (the effective value stays the boot value).
                    if let Some((action, value)) = origin {
                        if restart.is_empty() {
                            self.config_saved.remove(&action);
                        } else {
                            self.config_saved.insert(action, value);
                        }
                    }
                    self.set_status(settings_status(&applied, &restart));
                }
                Err(err) => self.set_status(StatusMsg::fail(format!("config: {err}"))),
            },
            Backend::Remote(remote) => {
                remote.pending_settings = Some((req, origin));
                self.set_status(StatusMsg::ok("config: applying…"));
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn take_pending_settings(
        &mut self,
    ) -> Option<(
        crate::proxy::server::SettingsRequest,
        Option<(ui::ConfigAction, String)>,
    )> {
        match &mut self.backend {
            Backend::Remote(remote) => remote.pending_settings.take(),
            Backend::Local(_) => None,
        }
    }

    /// POST a settings change to the attached daemon and report the honest
    /// applied/restart split from its response.
    async fn perform_remote_settings(
        &mut self,
        req: crate::proxy::server::SettingsRequest,
        origin: Option<(ui::ConfigAction, String)>,
    ) {
        let Backend::Remote(remote) = &mut self.backend else {
            return;
        };
        let url = format!("{}/llmux/settings", remote.base_url);
        let mut request = remote.client.post(&url).json(&req);
        if let Some(key) = &remote.api_key {
            request = request.header("x-api-key", key);
        }
        let quota_changed = req.quota_display.is_some();
        let message = match request.send().await {
            Ok(response) if response.status().is_success() => {
                // TYPED ack, parsed BEFORE any local state changes: a
                // malformed / empty / ok=false 2xx is UNVERIFIED — neither
                // the quota override nor the pending-restart map may move
                // on an unconfirmed apply (review MUST-FIX).
                match response.json::<crate::proxy::server::SettingsAck>().await {
                    Ok(ack) if ack.ok => {
                        if quota_changed {
                            self.quota_display_override = None;
                        }
                        if let Some((action, value)) = origin {
                            if ack.restart_required.is_empty() {
                                self.config_saved.remove(&action);
                            } else {
                                self.config_saved.insert(action, value);
                            }
                        }
                        settings_status(
                            &ack.applied.iter().map(String::as_str).collect::<Vec<_>>(),
                            &ack.restart_required
                                .iter()
                                .map(String::as_str)
                                .collect::<Vec<_>>(),
                        )
                    }
                    Ok(_) => StatusMsg::fail("config change failed: daemon reported ok=false"),
                    Err(_) => StatusMsg::warn(
                        "config: accepted by daemon, but the ack was unreadable — verify the \
                         value in the config tab (nothing assumed)",
                    ),
                }
            }
            Ok(response) => StatusMsg::fail(format!("config change failed: {}", response.status())),
            Err(err) => StatusMsg::fail(format!("config change failed: {err}")),
        };
        self.set_status(message);
    }

    /// Open the surface a tab click selected (UI-3 U6). Stats and Sessions go
    /// through their openers (the guard / the background load); the rest are
    /// plain overlay switches. Clicking the active tab returns to MAIN.
    fn open_tab(&mut self, tab: Overlay, view: Option<&DashboardView>) {
        if tab == self.overlay {
            self.overlay = Overlay::None;
            return;
        }
        match tab {
            Overlay::None => self.overlay = Overlay::None,
            Overlay::Stats => self.open_stats(view),
            Overlay::Sessions => self.open_sessions(),
            // Same entry behavior as the `a` key: one refresh, not one per
            // frame (.prd/16).
            Overlay::Accounts => self.open_accounts(view),
            Overlay::Usage => {
                self.usage_scroll = 0;
                self.overlay = Overlay::Usage;
            }
            // Same entry behavior as `K`: open AND ask the durable store.
            Overlay::Keys => self.open_keys(),
            Overlay::Logs => self.open_logs(),
            other => self.overlay = other,
        }
    }

    /// Key handling for the Usage overlay (usage-stats). `g` cycles the
    /// granularity (hour → day → month), arrows/`j`/`k` scroll by bucket,
    /// `U`/Esc closes back to MAIN, `q` quits. The STORED offset is clamped
    /// against the selected granularity's bucket count on every press —
    /// clamping only a draw-time copy would let presses at the bottom
    /// accumulate invisible overscroll debt (review R1 MUST-FIX 2).
    fn on_key_usage(&mut self, code: KeyCode, view: Option<&DashboardView>) {
        let max_scroll = view
            .map(|v| usage_bucket_count(v, self.usage_gran).saturating_sub(1))
            .unwrap_or(0);
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('U') | KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Char('g') => {
                self.usage_gran = self.usage_gran.next();
                self.usage_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.usage_scroll = self.usage_scroll.saturating_sub(1).min(max_scroll);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.usage_scroll = self.usage_scroll.saturating_add(1).min(max_scroll);
            }
            KeyCode::PageUp => {
                self.usage_scroll = self.usage_scroll.saturating_sub(10).min(max_scroll);
            }
            KeyCode::PageDown => {
                self.usage_scroll = self.usage_scroll.saturating_add(10).min(max_scroll);
            }
            KeyCode::Home => self.usage_scroll = 0,
            KeyCode::End => self.usage_scroll = max_scroll,
            _ => {}
        }
    }

    /// Move the model cursor by `delta` rows, clamped to `[0, len-1]`.
    fn move_model_cursor(&mut self, delta: i64, len: usize) {
        if len == 0 {
            self.model_cursor = 0;
            return;
        }
        let next = (self.model_cursor as i64).saturating_add(delta);
        self.model_cursor = next.clamp(0, (len - 1) as i64) as usize;
    }

    /// Key handling for the Sessions overlay (`s`, issue #34). Arrows/`j`/`k`
    /// move the cursor through session rows; `s`/`Esc` closes back to MAIN; `q`
    /// quits. The folded sessions are a snapshot taken at open time.
    fn on_key_sessions(&mut self, code: KeyCode) {
        let len = self.sessions.len();
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('s') | KeyCode::Esc => self.overlay = Overlay::None,
            // Sort cycle (recent → tokens → requests); cursor resets so the
            // selection follows the ORDER, not a stale index.
            KeyCode::Char('o') => {
                self.session_sort = self.session_sort.next();
                self.session_sort.apply(&mut self.sessions);
                self.session_cursor = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_session_cursor(-1, len),
            KeyCode::Down | KeyCode::Char('j') => self.move_session_cursor(1, len),
            KeyCode::PageUp => self.move_session_cursor(-10, len),
            KeyCode::PageDown => self.move_session_cursor(10, len),
            KeyCode::Home => self.session_cursor = 0,
            KeyCode::End => self.session_cursor = len.saturating_sub(1),
            _ => {}
        }
    }

    /// Move the session cursor by `delta` rows, clamped to `[0, len-1]`.
    fn move_session_cursor(&mut self, delta: i64, len: usize) {
        if len == 0 {
            self.session_cursor = 0;
            return;
        }
        let next = (self.session_cursor as i64).saturating_add(delta);
        self.session_cursor = next.clamp(0, (len - 1) as i64) as usize;
    }

    /// Open the Sessions overlay (`s`, issue #34): kick off a background read of
    /// the persisted raw-io log from `$XDG_STATE_HOME/llmux/raw-io.jsonl`, fold it
    /// into a confidence-labeled session timeline off the runtime, and open the
    /// overlay immediately with a loading spinner. The read+parse+fold is blocking
    /// IO/CPU over a multi-MB log, so running it inline inside the async event
    /// loop froze the whole TUI ~10s — it now runs on the blocking pool and the
    /// timeline arrives over `sessions_tx` as a stream of progressive partials
    /// (`stream_sessions`), mirroring the remote-fetch pattern. Each partial
    /// replaces `sessions`, so the table fills in as the file is read rather than
    /// appearing all-at-once at the end. A missing/unreadable file delivers a
    /// single empty, done partial (the overlay then shows the empty hint). The
    /// snapshot is point-in-time — re-opening re-reads the file from scratch.
    fn open_sessions(&mut self) {
        self.overlay = Overlay::Sessions;
        self.session_cursor = 0;
        if self.sessions_loading {
            return; // a load is already in flight — no second reader (reopen guard)
        }
        self.sessions_loading = true;
        self.sessions_pct = 0;
        self.sessions_since = self.clock_at(Instant::now());
        if let Some(tx) = self.sessions_tx.clone() {
            // read + parse + fold is blocking IO/CPU → off the runtime onto the
            // blocking pool so the event loop keeps rendering and taking input.
            tokio::task::spawn_blocking(move || {
                stream_sessions(&tx);
            });
        }
        // No tx (only in unit tests that never run `event_loop`) → stays in the
        // loading state; those tests drive overlay/sessions state directly, not
        // this path.
    }

    /// Key handling for MAIN (no overlay open). `a`/`g`/`l` summon the overlays;
    /// `R` reloads, `f/m/e` drive codex, arrows scroll the activity log, `q`
    /// quits. The account-mutation affordances (add/remove/login/switch) live in
    /// the Accounts overlay, reached with `a`.
    fn on_key_main(&mut self, code: KeyCode, view: Option<&DashboardView>) {
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('R') => self.reload(),
            // Summon overlays (issue #5). Accounts entry also requests ONE
            // usage refresh when the reset counts are absent/stale (.prd/16).
            KeyCode::Char('a') => self.open_accounts(view),
            KeyCode::Char('g') => self.open_stats(view),
            KeyCode::Char('l') => self.open_logs(),
            // Observed performance (perf telemetry v1): daily tok/s + health.
            KeyCode::Char('p') => self.overlay = Overlay::Perf,
            // Session timeline (issue #34): read + fold the persisted raw-io log.
            KeyCode::Char('s') => self.open_sessions(),
            // Calendar usage table (usage-stats): hourly/daily/monthly × model
            // tokens + API-equivalent cost.
            KeyCode::Char('U') => self.open_tab(Overlay::Usage, view),
            // Misc (keys/build facts) + Config surfaces (UI-3 U6).
            KeyCode::Char('?') => self.overlay = Overlay::Misc,
            KeyCode::Char('c') => self.overlay = Overlay::Config,
            // Client keys / tenants (multi-tenant #22).
            KeyCode::Char('K') => self.open_keys(),
            // Activity-log scrolling (req6): up = into history, down = toward
            // the live tail. Clamped to the number of completed entries.
            KeyCode::Up | KeyCode::Char('k') => self.scroll_activity(1, view),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_activity(-1, view),
            KeyCode::PageUp => self.scroll_activity(10, view),
            KeyCode::PageDown => self.scroll_activity(-10, view),
            KeyCode::Home => self.scroll_activity(i64::MAX, view),
            KeyCode::End => self.activity_scroll = 0,
            // Codex group settings (req8.1): f = fast on/off, m = cycle model,
            // e = cycle reasoning effort. No-op (with a hint) when there is no
            // codex account.
            KeyCode::Char('f') => self.toggle_codex_fast(view),
            KeyCode::Char('m') => self.cycle_codex_model(view),
            KeyCode::Char('e') => self.cycle_codex_effort(view),
            // Quota-gauge fill direction (used% grows / left% drains) —
            // session-local override of config `quota_display`.
            KeyCode::Char('u') => self.toggle_quota_display(view),
            // Reset display: countdown ↔ absolute UTC stamp in the quota bars.
            KeyCode::Char('t') => self.toggle_reset_display(),
            // Accounts row order WITHIN each backend group: name ↔ next-pick.
            KeyCode::Char('o') => self.toggle_account_sort(),
            // Fold the 7d-spent accounts into a text block under the table.
            KeyCode::Char('z') => self.toggle_fold_exhausted(),
            // Scheduler mode: default (quota-max) ↔ round-robin (min switch).
            KeyCode::Char('S') => self.toggle_scheduler_mode(view),
            _ => {}
        }
    }

    /// Flip the scheduler between default and round-robin (persisted server
    /// side; see README "Schedulers").
    fn toggle_scheduler_mode(&mut self, view: Option<&DashboardView>) {
        use crate::config::SchedulerMode;
        let current = view.map_or(SchedulerMode::Default, |v| v.select_params.mode);
        let next = match current {
            SchedulerMode::Default => SchedulerMode::RoundRobin,
            SchedulerMode::RoundRobin => SchedulerMode::Default,
        };
        match &mut self.backend {
            Backend::Local(state) => match state.set_scheduler_mode(next) {
                Ok(mode) => {
                    self.set_status(StatusMsg::ok(format!("scheduler mode: {}", mode.label())))
                }
                Err(err) => self.set_status(StatusMsg::fail(format!(
                    "scheduler mode change failed: {err}"
                ))),
            },
            Backend::Remote(remote) => {
                remote.pending_mode = Some(next);
                self.set_status(StatusMsg::ok(format!("scheduler mode → {}…", next.label())));
            }
        }
    }

    fn take_pending_mode(&mut self) -> Option<crate::config::SchedulerMode> {
        match &mut self.backend {
            Backend::Remote(remote) => remote.pending_mode.take(),
            Backend::Local(_) => None,
        }
    }

    /// Perform the queued remote mode change (`POST /llmux/scheduler-mode`).
    async fn perform_remote_mode(&mut self, mode: crate::config::SchedulerMode) {
        let Backend::Remote(remote) = &mut self.backend else {
            return;
        };
        let url = format!("{}/llmux/scheduler-mode", remote.base_url);
        let mut request = remote
            .client
            .post(&url)
            .json(&serde_json::json!({ "mode": mode.label() }));
        if let Some(key) = &remote.api_key {
            request = request.header("x-api-key", key);
        }
        let message = match request.send().await {
            Ok(response) if response.status().is_success() => {
                StatusMsg::ok(format!("scheduler mode: {}", mode.label()))
            }
            Ok(response) => StatusMsg::fail(format!(
                "scheduler mode change failed: {}",
                response.status()
            )),
            Err(err) => StatusMsg::fail(format!("scheduler mode change failed: {err}")),
        };
        self.set_status(message);
    }

    /// Flip the quota bars between reset countdown and absolute UTC stamp.
    fn toggle_reset_display(&mut self) {
        self.reset_absolute = !self.reset_absolute;
        self.set_status(StatusMsg::ok(if self.reset_absolute {
            "reset shown as absolute time (UTC)"
        } else {
            "reset shown as countdown"
        }));
    }

    /// Flip MAIN's fold of the accounts whose 7d window is over its ceiling
    /// (session-local, like `t`/`u`/`o`; off until the first press).
    fn toggle_fold_exhausted(&mut self) {
        self.fold_exhausted = !self.fold_exhausted;
        self.set_status(StatusMsg::ok(if self.fold_exhausted {
            "fold on"
        } else {
            "fold off"
        }));
    }

    /// Flip the accounts table's WITHIN-GROUP order between account name and
    /// the scheduler's next-pick order. The backend-group blocks never move;
    /// session-local, like `t`/`u`.
    fn toggle_account_sort(&mut self) {
        self.account_sort = self.account_sort.toggle();
        self.set_status(StatusMsg::ok(format!(
            "accounts sorted by {}",
            self.account_sort.label()
        )));
    }

    /// Flip the quota-gauge fill direction between used% and remaining%
    /// (session-local override of config `quota_display`; the config default
    /// applies until the first press). Color bands stay keyed on USED
    /// utilization either way — this only flips what the fill length means.
    fn toggle_quota_display(&mut self, view: Option<&DashboardView>) {
        use crate::config::QuotaDisplay;
        let current = self
            .quota_display_override
            .unwrap_or_else(|| view.map_or(QuotaDisplay::default(), |v| v.quota_display));
        let next = match current {
            QuotaDisplay::Used => QuotaDisplay::Remaining,
            QuotaDisplay::Remaining => QuotaDisplay::Used,
        };
        self.quota_display_override = Some(next);
        self.set_status(StatusMsg::ok(match next {
            QuotaDisplay::Used => "quota gauges fill: used%",
            QuotaDisplay::Remaining => "quota gauges fill: remaining%",
        }));
    }

    /// Key handling for the Accounts overlay (`a`). Houses the issue #3/#4
    /// affordances — switch (`s`), add an API key (`a`), remove (`r`), start a
    /// new browser login (`n`) — each entering its own [`Mode`] which is handled
    /// over this overlay. `Esc` closes back to MAIN; `q` quits.
    fn on_key_accounts(&mut self, code: KeyCode, view: Option<&DashboardView>) {
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.overlay = Overlay::None,
            // Same fill-direction toggle as MAIN — the accounts overlay is
            // where the gauges live full-width.
            KeyCode::Char('u') => self.toggle_quota_display(view),
            KeyCode::Char('t') => self.toggle_reset_display(),
            // Same row-order toggle as MAIN — this overlay is the full-width
            // accounts table, so the order matters most here.
            KeyCode::Char('o') => self.toggle_account_sort(),
            // The compact account list (twolab-tui agent_list) or the table:
            // `V` — `v` is copy mode everywhere.
            KeyCode::Char('V') => {
                self.compact_accounts = !self.compact_accounts;
                self.set_status(StatusMsg::ok(if self.compact_accounts {
                    "accounts: compact list (V: table)"
                } else {
                    "accounts: table (V: compact list)"
                }));
            }
            // Switch the active account (the `s` switcher, now scoped to this
            // overlay). The cursor starts at row 0 — which is the first row of
            // the first group block under EITHER sort (name order does not put
            // the current account there), not necessarily the current account.
            KeyCode::Char('s') => {
                let accounts = view.map_or(0, |v| v.snapshot.accounts.len());
                if accounts == 0 {
                    self.set_status(StatusMsg::warn("no accounts to switch between"));
                } else {
                    self.mode = Mode::Select { idx: 0 };
                }
            }
            // Add an API-key account (issue #3): works in BOTH local and attach
            // mode (local: in-process config update + pool reload; remote:
            // POST /llmux/add-account).
            KeyCode::Char('a') => {
                self.field = ComposerState::new().masked(Mask::default());
                self.mode = Mode::AddKey;
                self.set_status(StatusMsg::ok(
                    "add account: paste an Anthropic API key, Enter to add, Esc to cancel",
                ));
            }
            // Remove the selected account (issue #3): a destructive delete, so
            // it opens a confirm step (y/N) — never a silent delete.
            KeyCode::Char('r') => {
                let len = view.map_or(0, |v| v.snapshot.accounts.len());
                if len == 0 {
                    self.set_status(StatusMsg::warn("no accounts to remove"));
                } else {
                    self.mode = Mode::ConfirmRemove { idx: 0 };
                }
            }
            // Start a NEW browser login (issue #4): opens a provider picker
            // (Claude / Codex). The OAuth flow runs in THIS client; the minted
            // credential is injected into the daemon, so it works in both local
            // and attach mode with no restart.
            KeyCode::Char('n') => self.open_new_login(),
            // Explicit usage refresh for every supported account (.prd/16).
            // Unlike the entry refresh this has NO staleness floor: the
            // operator asking is the reason.
            KeyCode::Char('f') => {
                self.queue_control(
                    ControlOp::Refresh { account: None },
                    "refreshing usage for all supported accounts…".into(),
                );
            }
            // Redeem ONE reset (.prd/16): opens the confirm gate on the first
            // codex row. Capitalized on purpose — `r` removes an account, and
            // a redemption must not share a keystroke with anything.
            KeyCode::Char('R') => self.open_reset_confirm(view),
            _ => {}
        }
    }

    /// Open the redemption confirm on the first row whose provider actually
    /// has resets (codex or Claude). No such account → a hint, never an empty
    /// gate.
    fn open_reset_confirm(&mut self, view: Option<&DashboardView>) {
        let Some(view) = view else { return };
        let now = SystemTime::now();
        let order = view.display_order(self.account_sort, now);
        // A held (unresolved) redemption pins the gate to ITS account, so
        // reopening resumes that id instead of prompting for another account.
        let held = self.pending_redemption.as_ref().and_then(|p| {
            order
                .iter()
                .position(|&i| view.snapshot.accounts[i].id.0 == p.account)
        });
        let Some(pos) = held.or_else(|| {
            order
                .iter()
                .position(|&i| view::has_usage_controls(view.snapshot.accounts[i].credential_kind))
        }) else {
            self.set_status(StatusMsg::warn(
                "reset: no codex or claude account (rate-limit resets are a ChatGPT/Codex \
                 or Claude subscription entitlement)",
            ));
            return;
        };
        self.mode = Mode::ConfirmReset { idx: pos };
        self.set_reset_confirm_status(pos, view);
    }

    /// Status line for the open confirm gate: names the account under the
    /// cursor and its inventory, or the pending redemption this client is
    /// still holding for it (which `y` retries with the SAME id).
    fn set_reset_confirm_status(&mut self, idx: usize, view: &DashboardView) {
        let now = SystemTime::now();
        let order = view.display_order(self.account_sort, now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        let account = target.id.0.clone();
        if let Some(pending) = self
            .pending_redemption
            .as_ref()
            .filter(|p| p.account == account)
        {
            let request_id = pending.request_id.clone();
            self.set_status(StatusMsg::warn(format!(
                "{account} has an UNRESOLVED redemption (request id {request_id}) — y retries \
                 THAT id, Esc closes this dialog (it does not abandon the redemption)"
            )));
            return;
        }
        let control = view.usage_control(&account);
        self.set_status(StatusMsg::warn(reset_confirm_status(
            &account,
            control.and_then(|c| c.available_resets),
            control.and_then(|c| c.applicable_resets),
        )));
    }

    /// Key handling for `Mode::ConfirmReset`: arrows pick the account, `1` /
    /// `y` redeems ONE reset, anything else — Enter included, the gate keeps
    /// its mark on Cancel — closes the dialog.
    ///
    /// Closing is NOT abandoning: a redemption this client is still holding
    /// stays held (and the daemon keeps its own pending receipt), so reopening
    /// the gate offers the same id again rather than minting a new one.
    fn on_key_confirm_reset(&mut self, code: KeyCode, idx: usize, view: Option<&DashboardView>) {
        let Some(view) = view else {
            self.mode = Mode::Normal;
            return;
        };
        let len = view.snapshot.accounts.len();
        if len == 0 {
            self.mode = Mode::Normal;
            return;
        }
        let idx = idx.min(len - 1);
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                let idx = idx.saturating_sub(1);
                self.mode = Mode::ConfirmReset { idx };
                self.set_reset_confirm_status(idx, view);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let idx = (idx + 1).min(len - 1);
                self.mode = Mode::ConfirmReset { idx };
                self.set_reset_confirm_status(idx, view);
            }
            _ if gate_answer(code, ui::ACCOUNT_GATE_MARK) == GateAnswer::Confirm => {
                self.submit_reset(idx, view);
                self.mode = Mode::Normal;
            }
            // Any other key (Esc/n/2/Enter/q/…) closes the dialog only.
            _ => {
                self.mode = Mode::Normal;
                self.set_status(StatusMsg::warn(match self.pending_redemption.as_ref() {
                    Some(pending) => format!(
                        "closed — {} still has an unresolved redemption (request id {})",
                        pending.account, pending.request_id
                    ),
                    None => "reset cancelled — nothing was spent".to_string(),
                }));
            }
        }
    }

    /// Queue the redemption for the row under the cursor. The account must be
    /// a codex account; the idempotency key is minted HERE, once, and a held
    /// (unresolved) key for this account is REUSED rather than replaced.
    fn submit_reset(&mut self, idx: usize, view: &DashboardView) {
        let now = SystemTime::now();
        let order = view.display_order(self.account_sort, now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        if !view::has_usage_controls(target.credential_kind) {
            self.set_status(StatusMsg::warn(format!(
                "reset: {} is a {} account — rate-limit resets are a Codex or Claude \
                 subscription entitlement",
                target.id, target.credential_kind
            )));
            return;
        }
        let account = target.id.0.clone();
        // Reuse an unresolved key for this account: the first attempt may
        // already have spent the credit, so only THAT id may be retried. The
        // daemon's own pending receipt is the second line of defense.
        let held = self
            .pending_redemption
            .as_ref()
            .filter(|p| p.account == account)
            .cloned()
            .or_else(|| {
                view.usage_control(&account)
                    .and_then(|c| c.pending_request_id.as_ref())
                    .map(|request_id| PendingRedemption {
                        account: account.clone(),
                        request_id: request_id.clone(),
                        credit_id: view
                            .usage_control(&account)
                            .and_then(|c| c.pending_credit_id.clone()),
                    })
            });
        let (request_id, credit_id, pending_text) = match held {
            Some(pending) => (
                pending.request_id.clone(),
                pending.credit_id.clone(),
                format!(
                    "retrying the pending redemption for {account} (request id {})…",
                    pending.request_id
                ),
            ),
            None => {
                let request_id = ulid::Ulid::new().to_string();
                let text = format!("redeeming one reset for {account} (request id {request_id})…");
                (request_id, None, text)
            }
        };
        // The hold is installed ONLY if the queue accepted the redemption, but
        // still before the send: a refused queue (another control operation in
        // flight) must leave any existing hold — and the absence of one —
        // exactly as it was, while an accepted one is held from this moment so
        // a lost outcome still has its retry key on the client.
        let accepted = self.queue_control(
            ControlOp::Reset {
                account: account.clone(),
                request_id: request_id.clone(),
                credit_id: credit_id.clone(),
            },
            pending_text,
        );
        if accepted {
            self.pending_redemption = Some(PendingRedemption {
                account,
                request_id,
                credit_id,
            });
        }
    }

    /// Open the Accounts overlay and request ONE usage refresh if the reset
    /// inventory is absent or older than [`ENTRY_REFRESH_FLOOR`] (.prd/16):
    /// entering the surface populates the counts, but re-entering it (or
    /// re-rendering it) does not hammer upstream. `f` has no such floor.
    fn open_accounts(&mut self, view: Option<&DashboardView>) {
        self.overlay = Overlay::Accounts;
        if let Some(view) = view {
            if accounts_entry_refresh_due(view, SystemTime::now()) {
                self.queue_control(
                    ControlOp::Refresh { account: None },
                    "refreshing reset counts…".into(),
                );
            }
        }
    }

    /// Open the Stats overlay (`g`). No-op (with a hint) until at least one
    /// model row exists, matching the old detailed-view guard (req13).
    fn open_stats(&mut self, view: Option<&DashboardView>) {
        if view.is_some_and(|v| !v.model_usage.is_empty()) {
            self.overlay = Overlay::Stats;
            self.model_cursor = 0;
        } else {
            self.set_status(StatusMsg::warn("models: no model usage yet"));
        }
    }

    /// The Stats tab with model `index` (strip order, which is the table's)
    /// under the cursor — a click on a models-strip entry.
    fn open_model(&mut self, index: usize, view: Option<&DashboardView>) {
        self.open_stats(view);
        if self.overlay == Overlay::Stats {
            let len = view.map_or(0, |v| v.model_usage.len());
            self.model_cursor = index.min(len.saturating_sub(1));
        }
    }

    /// The live codex settings, or `None` when no codex account exists.
    fn current_codex(&self, view: Option<&DashboardView>) -> Option<CodexSettingsDoc> {
        view.and_then(|v| v.codex.available.then(|| v.codex.clone()))
    }

    fn toggle_codex_fast(&mut self, view: Option<&DashboardView>) {
        match self.current_codex(view) {
            Some(mut c) => {
                c.fast = !c.fast;
                self.set_codex(c);
            }
            None => self.set_status(StatusMsg::warn(
                "codex: no codex account (run `llmux login --codex`)",
            )),
        }
    }

    fn cycle_codex_model(&mut self, view: Option<&DashboardView>) {
        if let Some(mut c) = self.current_codex(view) {
            let next = CODEX_MODELS
                .iter()
                .position(|m| *m == c.model)
                .map(|i| (i + 1) % CODEX_MODELS.len())
                .unwrap_or(0);
            c.model = CODEX_MODELS[next].to_string();
            self.set_codex(c);
        } else {
            self.set_status(StatusMsg::warn("codex: no codex account"));
        }
    }

    fn cycle_codex_effort(&mut self, view: Option<&DashboardView>) {
        if let Some(mut c) = self.current_codex(view) {
            let cur = c.effort.as_deref().unwrap_or("");
            let next = CODEX_EFFORTS
                .iter()
                .position(|e| *e == cur)
                .map(|i| (i + 1) % CODEX_EFFORTS.len())
                .unwrap_or(0);
            let e = CODEX_EFFORTS[next];
            c.effort = (!e.is_empty()).then(|| e.to_string());
            self.set_codex(c);
        } else {
            self.set_status(StatusMsg::warn("codex: no codex account"));
        }
    }

    /// Cycle the grok effort override (UI-3 U12): bypass → none → low →
    /// medium → high → xhigh → bypass. Local mode applies + persists in-process;
    /// attach mode queues `POST /llmux/grok`.
    fn cycle_grok_effort(&mut self, view: Option<&DashboardView>) {
        let Some(view) = view else { return };
        if !view.grok.available {
            self.set_status(StatusMsg::warn("grok: no grok account"));
            return;
        }
        let cur = view.grok.effort.as_deref().unwrap_or("");
        let next = GROK_EFFORTS
            .iter()
            .position(|e| *e == cur)
            .map(|i| (i + 1) % GROK_EFFORTS.len())
            .unwrap_or(0);
        let effort = (!GROK_EFFORTS[next].is_empty()).then(|| GROK_EFFORTS[next].to_string());
        let label = effort.clone().unwrap_or_else(|| "bypass".to_string());
        match &mut self.backend {
            Backend::Local(state) => {
                // Persist FIRST (config-editor contract) — see set_codex.
                if state.config_path.is_none() {
                    return self.set_status(StatusMsg::fail(
                        "grok change failed: persistence unavailable",
                    ));
                }
                if let Some(path) = &state.config_path {
                    if let Err(err) = crate::config::update_path(path, |c| {
                        c.grok.reasoning_effort = effort.clone();
                    }) {
                        self.set_status(StatusMsg::fail(format!("grok change failed: {err}")));
                        return;
                    }
                }
                let mut shape = state.grok.shape();
                shape.effort = effort.clone();
                state.grok.set_shape(shape);
                self.set_status(StatusMsg::ok(format!("grok effort: {label}")));
            }
            Backend::Remote(remote) => {
                remote.pending_grok = Some(effort);
                self.set_status(StatusMsg::ok(format!("grok effort → {label}…")));
            }
        }
    }

    fn take_pending_grok(&mut self) -> Option<Option<String>> {
        match &mut self.backend {
            Backend::Remote(remote) => remote.pending_grok.take(),
            Backend::Local(_) => None,
        }
    }

    /// Perform the queued remote grok effort change (`POST /llmux/grok`).
    /// `None` clears to bypass (the endpoint's "unset" form).
    async fn perform_remote_grok(&mut self, effort: Option<String>) {
        let Backend::Remote(remote) = &mut self.backend else {
            return;
        };
        let url = format!("{}/llmux/grok", remote.base_url);
        let label = effort.as_deref().unwrap_or("bypass").to_string();
        let mut request = remote.client.post(&url).json(&serde_json::json!({
            "reasoning_effort": effort.as_deref().unwrap_or("unset"),
        }));
        if let Some(key) = &remote.api_key {
            request = request.header("x-api-key", key);
        }
        let message = match request.send().await {
            Ok(response) if response.status().is_success() => {
                StatusMsg::ok(format!("grok effort: {label}"))
            }
            Ok(response) => {
                StatusMsg::fail(format!("grok effort change failed: {}", response.status()))
            }
            Err(err) => StatusMsg::fail(format!("grok effort change failed: {err}")),
        };
        self.set_status(message);
    }

    /// Apply a codex settings change: locally in-process, or queued for the
    /// event loop to POST in attach mode.
    fn set_codex(&mut self, new: CodexSettingsDoc) {
        match &mut self.backend {
            Backend::Local(state) => {
                // Persist FIRST (config-editor contract): a failed write
                // leaves the live shape untouched and reports the error —
                // live-then-persist would let disk and runtime diverge.
                if state.config_path.is_none() {
                    return self.set_status(StatusMsg::fail(
                        "codex change failed: persistence unavailable",
                    ));
                }
                if let Some(path) = &state.config_path {
                    if let Err(err) = crate::config::update_path(path, |c| {
                        c.codex.default_model = new.model.clone();
                        c.codex.fast = new.fast;
                        c.codex.reasoning_effort = new.effort.clone();
                    }) {
                        self.set_status(StatusMsg::fail(format!("codex change failed: {err}")));
                        return;
                    }
                }
                // Carry the live `client_model` override forward: it is a
                // config-only opt-in the TUI settings panel doesn't manage, so
                // a model/fast/effort change here must not silently clear it.
                let client_model = state.codex.shape().client_model;
                state.codex.set_shape(crate::provider::codex::CodexShape {
                    model: new.model.clone(),
                    client_model,
                    fast: new.fast,
                    effort: new.effort.clone(),
                });
                self.set_status(StatusMsg::ok(codex_status_line(&new)));
            }
            Backend::Remote(remote) => {
                remote.pending_codex = Some(new.clone());
                self.set_status(StatusMsg::ok(format!(
                    "applying {}…",
                    codex_status_line(&new)
                )));
            }
        }
    }

    fn take_pending_codex(&mut self) -> Option<CodexSettingsDoc> {
        match &mut self.backend {
            Backend::Remote(remote) => remote.pending_codex.take(),
            Backend::Local(_) => None,
        }
    }

    fn take_pending_pause(&mut self) -> Option<(String, bool)> {
        match &mut self.backend {
            Backend::Remote(remote) => remote.pending_pause.take(),
            Backend::Local(_) => None,
        }
    }

    /// Perform the queued remote pause/resume (`POST /llmux/pause-account`).
    async fn perform_remote_pause(&mut self, account: String, paused: bool) {
        let Backend::Remote(remote) = &mut self.backend else {
            return;
        };
        let url = format!("{}/llmux/pause-account", remote.base_url);
        let mut request = remote
            .client
            .post(&url)
            .json(&serde_json::json!({ "account": account, "paused": paused }));
        if let Some(key) = &remote.api_key {
            request = request.header("x-api-key", key);
        }
        let verb = if paused { "paused" } else { "resumed" };
        let message = match request.send().await {
            Ok(response) if response.status().is_success() => {
                StatusMsg::ok(format!("{verb} {account}"))
            }
            Ok(response) => StatusMsg::fail(format!("pause change failed: {}", response.status())),
            Err(err) => StatusMsg::fail(format!("pause change failed: {err}")),
        };
        self.set_status(message);
    }

    fn take_pending_limits(&mut self) -> Option<(String, crate::config::AccountLimits)> {
        match &mut self.backend {
            Backend::Remote(remote) => remote.pending_limits.take(),
            Backend::Local(_) => None,
        }
    }

    /// Perform the queued remote limits change (`POST /llmux/account-limits`).
    async fn perform_remote_limits(
        &mut self,
        account: String,
        limits: crate::config::AccountLimits,
    ) {
        let Backend::Remote(remote) = &mut self.backend else {
            return;
        };
        let url = format!("{}/llmux/account-limits", remote.base_url);
        let mut request = remote.client.post(&url).json(&serde_json::json!({
            "account": account,
            "five_hour_max": limits.five_hour_max,
            "seven_day_max": limits.seven_day_max,
            "fable_weekly_max": limits.fable_weekly_max,
        }));
        if let Some(key) = &remote.api_key {
            request = request.header("x-api-key", key);
        }
        let message = match request.send().await {
            Ok(response) if response.status().is_success() => {
                StatusMsg::ok(format!("limits updated for {account}"))
            }
            Ok(response) => StatusMsg::fail(format!("limits change failed: {}", response.status())),
            Err(err) => StatusMsg::fail(format!("limits change failed: {err}")),
        };
        self.set_status(message);
    }

    /// Open the limits editor for the switcher's highlighted row (`L` in
    /// `Mode::Select`). Input format: `5h,7d,fbl` as percents (`90,98,98`),
    /// missing/empty positions keep no override, empty input = all-global.
    fn open_limits_editor(&mut self, idx: usize, view: Option<&DashboardView>) {
        let Some(view) = view else { return };
        let now = SystemTime::now();
        let order = view.display_order(self.account_sort, now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        let fmt = |v: Option<f64>| v.map_or("global".to_string(), |v| format!("{:.0}%", v * 100.0));
        self.field = ComposerState::new().single_line();
        self.mode = Mode::EditLimits { idx };
        self.set_status(StatusMsg::ok(format!(
            "limits for {} — enter `5h,7d,fbl` percents (now {}, {}, {}); empty = global; Enter apply, Esc cancel",
            target.id,
            fmt(target.limits.five_hour_max),
            fmt(target.limits.seven_day_max),
            fmt(target.limits.fable_weekly_max),
        )));
    }

    /// Key handling for `Mode::EditLimits`: a text field that takes digits,
    /// `,`, `.` and spaces (typed or pasted) and edits like every field
    /// ([`Self::edit_field`]); Enter parses + applies, Esc cancels back to
    /// the switcher.
    fn on_key_edit_limits(&mut self, key: KeyEvent, idx: usize, view: Option<&DashboardView>) {
        match key.code {
            KeyCode::Esc => {
                self.close_field();
                self.mode = if std::mem::take(&mut self.limits_from_menu) {
                    Mode::Normal
                } else {
                    Mode::Select { idx }
                };
            }
            KeyCode::Enter => match parse_limits_input(self.field.text()) {
                Ok(limits) => {
                    self.close_field();
                    self.apply_limits_selected(idx, view, limits);
                    self.mode = if std::mem::take(&mut self.limits_from_menu) {
                        Mode::Normal
                    } else {
                        Mode::Select { idx }
                    };
                }
                // Keep editing — the text stays so it can be fixed.
                Err(err) => self.set_status(StatusMsg::fail(err)),
            },
            _ => {
                self.edit_field(key, limits_char);
            }
        }
    }

    /// Apply parsed limits to the highlighted row (local: in-process persist;
    /// attach: queue the POST).
    fn apply_limits_selected(
        &mut self,
        idx: usize,
        view: Option<&DashboardView>,
        limits: crate::config::AccountLimits,
    ) {
        let Some(view) = view else { return };
        let now = SystemTime::now();
        let order = view.display_order(self.account_sort, now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        let name = target.id.0.clone();
        match &mut self.backend {
            Backend::Local(state) => match state.set_account_limits(&name, limits) {
                Ok(true) => self.set_status(StatusMsg::ok(format!("limits updated for {name}"))),
                Ok(false) => self.set_status(StatusMsg::fail(format!("account {name} not found"))),
                Err(err) => {
                    self.set_status(StatusMsg::fail(format!("limits change failed: {err}")))
                }
            },
            Backend::Remote(remote) => {
                remote.pending_limits = Some((name.clone(), limits));
                self.set_status(StatusMsg::ok(format!("updating limits for {name}…")));
            }
        }
    }

    /// Toggle the operator pause on the switcher's highlighted row (`p` in
    /// `Mode::Select`). Local mode applies + persists in-process; attach mode
    /// queues the POST for the event loop.
    fn toggle_pause_selected(&mut self, idx: usize, view: Option<&DashboardView>) {
        let Some(view) = view else { return };
        let now = SystemTime::now();
        // The cursor indexes DISPLAY rows (selection order), not config order.
        let order = view.display_order(self.account_sort, now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        let name = target.id.0.clone();
        let next = !target.paused;
        match &mut self.backend {
            Backend::Local(state) => {
                let verb = if next { "paused" } else { "resumed" };
                match state.set_account_paused(&name, next) {
                    Ok(true) => self.set_status(StatusMsg::ok(format!("{verb} {name}"))),
                    Ok(false) => {
                        self.set_status(StatusMsg::fail(format!("account {name} not found")))
                    }
                    Err(err) => {
                        self.set_status(StatusMsg::fail(format!("pause change failed: {err}")))
                    }
                }
            }
            Backend::Remote(remote) => {
                remote.pending_pause = Some((name.clone(), next));
                self.set_status(StatusMsg::ok(format!(
                    "{} {name}…",
                    if next { "pausing" } else { "resuming" }
                )));
            }
        }
    }

    /// Perform the queued remote codex change (`POST /llmux/codex`).
    async fn perform_remote_codex(&mut self, new: CodexSettingsDoc) {
        let Backend::Remote(remote) = &mut self.backend else {
            return;
        };
        let url = format!("{}/llmux/codex", remote.base_url);
        let mut request = remote.client.post(&url).json(&serde_json::json!({
            "fast": new.fast,
            "default_model": new.model,
            "reasoning_effort": new.effort.clone().unwrap_or_default(),
        }));
        if let Some(key) = &remote.api_key {
            request = request.header("x-api-key", key);
        }
        let message = match request.send().await {
            Ok(response) if response.status().is_success() => {
                StatusMsg::ok(codex_status_line(&new))
            }
            Ok(response) => StatusMsg::fail(format!("codex change failed: {}", response.status())),
            Err(err) => StatusMsg::fail(format!("codex change failed: {err}")),
        };
        self.set_status(message);
    }

    /// Move the activity scroll offset by `delta` rows (positive = older),
    /// clamped to `[0, rendered_rows - 1]`. The unit is the FOLDED render
    /// row (glance-triage atom 3) — the same model `ui::draw_activity`
    /// windows by — so the offset can never strand past the last row.
    /// Scrolling near the end of what's loaded arms the background history
    /// hydration (UI-5 infinite scroll), after which the clamp ceiling grows
    /// as [`Self::extend_with_history`] pages more rows in.
    fn scroll_activity(&mut self, delta: i64, view: Option<&DashboardView>) {
        let len: usize = view.map_or(0, |v| triage::collapse_completed(&v.completed).len());
        let max = len.saturating_sub(1) as i64;
        let next = (self.activity_scroll as i64).saturating_add(delta);
        self.activity_scroll = next.clamp(0, max) as usize;
        if delta > 0 && (self.activity_scroll as i64) >= max.saturating_sub(HISTORY_ARM_MARGIN) {
            self.request_history();
            if let Some(view) = view {
                self.grow_history_take(view);
            }
        }
    }

    fn on_key_select(&mut self, code: KeyCode, idx: usize, view: Option<&DashboardView>) {
        let len = view.map_or(0, |v| v.snapshot.accounts.len());
        if len == 0 {
            self.mode = Mode::Normal;
            return;
        }
        let idx = idx.min(len - 1); // roster may have shrunk under us (R reload)
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Select {
                    idx: idx.saturating_sub(1),
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.mode = Mode::Select {
                    idx: (idx + 1).min(len - 1),
                };
            }
            KeyCode::Enter => {
                self.try_manual_switch(idx, view);
                self.mode = Mode::Normal;
            }
            // `n` from the switcher: start a brand-new login (issue #4's
            // headline path — "start a new login from the account switcher").
            KeyCode::Char('n') => self.open_new_login(),
            // `p` from the switcher: pause/resume the highlighted account
            // (operator pause — excluded from selection until resumed).
            KeyCode::Char('p') => {
                self.toggle_pause_selected(idx, view);
                self.mode = Mode::Select { idx };
            }
            // `L` from the switcher: edit the highlighted account's ceiling
            // overrides (config `account_limits`).
            KeyCode::Char('L') => self.open_limits_editor(idx, view),
            // `f` from the switcher: refresh THIS account's usage (.prd/16) —
            // the targeted form of the overlay's all-accounts refresh.
            KeyCode::Char('f') => {
                self.refresh_selected(idx, view);
                self.mode = Mode::Select { idx };
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('s') => self.mode = Mode::Normal,
            _ => self.mode = Mode::Select { idx },
        }
    }

    /// Queue an explicit usage refresh for the switcher's highlighted row.
    fn refresh_selected(&mut self, idx: usize, view: Option<&DashboardView>) {
        let Some(view) = view else { return };
        let now = SystemTime::now();
        let order = view.display_order(self.account_sort, now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        let account = target.id.0.clone();
        let pending = format!("refreshing usage for {account}…");
        self.queue_control(
            ControlOp::Refresh {
                account: Some(account),
            },
            pending,
        );
    }

    /// Open the new-login provider picker, OR — on a headless client that
    /// cannot open a browser — refuse with the `llmux login` fallback instead
    /// of starting a flow that would hang on the callback. "Headless" is
    /// decided by [`Self::can_open_browser`].
    fn open_new_login(&mut self) {
        if can_open_browser() {
            self.mode = Mode::NewLogin { idx: 0 };
            self.set_status(StatusMsg::ok(
                "new login: 1-5 or ↑↓ + Enter picks the provider and opens the browser, Esc \
                 cancels",
            ));
        } else {
            self.mode = Mode::Normal;
            self.set_status(StatusMsg::warn(headless_login_hint(self.is_remote())));
        }
    }

    /// Key handling for `Mode::NewLogin` — the provider picker
    /// ([`ui::login_list`]). The arrows (and `j`/`k`, PgUp/PgDn, Home/End)
    /// move the cursor and stop at the ends; a digit `1`–`5` or Enter queues
    /// that login for the event loop (which suspends the TUI, runs the
    /// browser flow, then re-inits); any other key cancels.
    fn on_key_new_login(&mut self, code: KeyCode, idx: usize) {
        use twolab_tui::scroll::ListAction;
        use twolab_tui::select_list::Outcome;
        let len = LoginKind::ALL.len();
        let idx = idx.min(len - 1);
        let code = match code {
            KeyCode::Char('k') => KeyCode::Up,
            KeyCode::Char('j') => KeyCode::Down,
            code => code,
        };
        let mut list = ui::login_list(idx);
        let outcome = key_input(code).map(|key| {
            if ListAction::from_key(key).is_some() {
                // Movement never cancels, and never wraps past an end.
                let wrapped = match code {
                    KeyCode::Up => idx == 0,
                    KeyCode::Down => idx + 1 == len,
                    _ => false,
                };
                if !wrapped {
                    list.key(key);
                }
                Outcome::Changed
            } else {
                list.key(key)
            }
        });
        match outcome {
            Some(Outcome::Changed) => {
                self.mode = Mode::NewLogin {
                    idx: list.selected().unwrap_or(idx).min(len - 1),
                };
            }
            Some(Outcome::Accepted(i)) => {
                let kind = LoginKind::ALL[i.min(len - 1)];
                self.pending_login = Some(kind);
                self.mode = Mode::Normal;
                self.set_status(StatusMsg::ok(format!(
                    "opening browser for {}…",
                    kind.label()
                )));
            }
            // Any other key cancels.
            Some(Outcome::Cancelled | Outcome::Ignored) | None => {
                self.mode = Mode::Normal;
                self.set_status(StatusMsg::warn("new login cancelled"));
            }
        }
    }

    fn take_pending_login(&mut self) -> Option<LoginKind> {
        self.pending_login.take()
    }

    /// A text field is open: the API key, the limits or a config value.
    fn field_open(&self) -> bool {
        matches!(
            self.mode,
            Mode::AddKey | Mode::EditLimits { .. } | Mode::ConfigEdit { .. }
        )
    }

    /// An editing key for the open field (twolab-tui composer, one line):
    /// ←/→ and Home/End (Ctrl+A/E) move the caret, Ctrl+←/→ by word;
    /// typing and Backspace/Delete edit at the caret; Ctrl+U / Ctrl+K /
    /// Ctrl+W cut to the start, to the end, the word before, and Ctrl+Y puts
    /// the cut back. A typed character `accept` refuses is dropped.
    fn edit_field(&mut self, key: KeyEvent, accept: impl Fn(char) -> bool) -> ComposerEvent {
        let Some(input) = key_event_input(&key) else {
            return ComposerEvent::Ignored;
        };
        if input.typed().is_some_and(|c| !accept(c)) {
            return ComposerEvent::Ignored;
        }
        match self.field.key(input, self.field_width) {
            // Enter is answered by the prompt's own handler before this; a
            // field never submits by itself.
            ComposerEvent::Submitted(text) => {
                self.field.set_text(&text);
                ComposerEvent::Ignored
            }
            ev => ev,
        }
    }

    /// Empty the field (and its kill buffer: a cut piece of a key must not
    /// outlive the prompt).
    fn close_field(&mut self) {
        self.field = ComposerState::default();
    }

    /// Text pasted into the terminal (bracketed paste). An open field takes
    /// it whole at the caret — line breaks become spaces (dropped from a
    /// key), the limits field keeps only the characters it accepts. Anywhere
    /// else the paste is typed key by key, as it was before bracketed paste
    /// was turned on. Returns whether anything changed.
    fn on_paste(&mut self, text: &str, view: Option<&DashboardView>) -> bool {
        match self.mode {
            Mode::AddKey | Mode::ConfigEdit { .. } => self.field.insert_str(text),
            Mode::EditLimits { .. } => {
                let kept: String = text.chars().filter(|&c| limits_char(c)).collect();
                self.field.insert_str(&kept)
            }
            _ => {
                for c in text.chars() {
                    let code = match c {
                        '\r' | '\n' => KeyCode::Enter,
                        '\t' => KeyCode::Tab,
                        c if c.is_control() => continue,
                        c => KeyCode::Char(c),
                    };
                    self.on_key(KeyEvent::new(code, KeyModifiers::NONE), view);
                }
                !text.is_empty()
            }
        }
    }

    /// Move the open field's window as the last frame drew it (`width`
    /// cells), so the next frame scrolls it only as far as the caret needs —
    /// the composer keeps that window in its state.
    fn track_field(&mut self, width: Option<u16>) {
        let Some(width) = width.filter(|_| self.field_open()) else {
            return;
        };
        self.field_width = width;
        let _ = twolab_tui::composer::Composer::new().line(&mut self.field, width);
    }

    /// Key handling for `Mode::AddKey` — typing the new account's API key in
    /// a masked field: Enter submits, Esc cancels, every other key edits
    /// ([`Self::edit_field`]; control characters are never inserted). The
    /// field is never drawn raw — the footer shows one `∙` per character.
    fn on_key_add(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.close_field();
                self.mode = Mode::Normal;
                self.set_status(StatusMsg::warn("add account cancelled"));
            }
            KeyCode::Enter => self.submit_add(),
            _ => {
                self.edit_field(key, |c| !c.is_control());
            }
        }
    }

    /// Submit the typed API key: add the account in-process (local) or queue a
    /// `POST /llmux/add-account` (remote). `take_secret` — the one way out of
    /// the masked field — empties it and its kill buffer, so the secret does
    /// not linger in memory longer than necessary.
    fn submit_add(&mut self) {
        let api_key = self.field.take_secret().trim().to_string();
        self.close_field();
        self.mode = Mode::Normal;
        if api_key.is_empty() {
            self.set_status(StatusMsg::warn("add account cancelled: empty key"));
            return;
        }
        match &mut self.backend {
            Backend::Local(state) => match state.add_apikey_account(None, &api_key) {
                // Status echoes the assigned NAME only — never the key.
                Ok((name, outcome)) => self.set_status(StatusMsg::ok(add_account_status(
                    &name,
                    matches!(outcome, crate::config::Upsert::Added),
                ))),
                Err(err) => self.set_status(StatusMsg::fail(format!("add account failed: {err}"))),
            },
            Backend::Remote(remote) => {
                remote.pending_add = Some(api_key);
                self.set_status(StatusMsg::ok("adding account…"));
            }
        }
    }

    /// Key handling for `Mode::ConfirmRemove` — a destructive delete gate.
    /// `1` / `y` confirm the removal; any other key (Enter included — the
    /// gate keeps its mark on Cancel) cancels. Arrow/j/k move the target row
    /// so the operator can pick which account to delete.
    fn on_key_confirm_remove(&mut self, code: KeyCode, idx: usize, view: Option<&DashboardView>) {
        let len = view.map_or(0, |v| v.snapshot.accounts.len());
        if len == 0 {
            self.mode = Mode::Normal;
            return;
        }
        let idx = idx.min(len - 1);
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::ConfirmRemove {
                    idx: idx.saturating_sub(1),
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.mode = Mode::ConfirmRemove {
                    idx: (idx + 1).min(len - 1),
                };
            }
            _ if gate_answer(code, ui::ACCOUNT_GATE_MARK) == GateAnswer::Confirm => {
                self.submit_remove(idx, view);
                self.mode = Mode::Normal;
            }
            // Any other key (Esc/n/2/Enter/q/…) cancels — delete is never silent.
            _ => {
                self.mode = Mode::Normal;
                self.set_status(StatusMsg::warn("remove cancelled"));
            }
        }
    }

    /// Resolve the display row to an account name, then remove it in-process
    /// (local) or queue a `POST /llmux/remove-account` (remote).
    fn submit_remove(&mut self, idx: usize, view: Option<&DashboardView>) {
        let Some(view) = view else { return };
        let now = SystemTime::now();
        // The cursor indexes DISPLAY rows (selection order), not config order.
        let order = view.display_order(self.account_sort, now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        let name = target.id.0.clone();
        match &mut self.backend {
            Backend::Local(state) => match state.remove_account(&name) {
                Ok(true) => self.set_status(StatusMsg::ok(format!("removed account {name}"))),
                Ok(false) => self.set_status(StatusMsg::fail(format!("account {name} not found"))),
                Err(err) => self.set_status(StatusMsg::fail(format!("remove failed: {err}"))),
            },
            Backend::Remote(remote) => {
                remote.pending_remove = Some(name.clone());
                self.set_status(StatusMsg::ok(format!("removing {name}…")));
            }
        }
    }

    fn take_pending_add(&mut self) -> Option<String> {
        match &mut self.backend {
            Backend::Remote(remote) => remote.pending_add.take(),
            Backend::Local(_) => None,
        }
    }

    fn take_pending_remove(&mut self) -> Option<String> {
        match &mut self.backend {
            Backend::Remote(remote) => remote.pending_remove.take(),
            Backend::Local(_) => None,
        }
    }

    /// Perform the queued remote add (`POST /llmux/add-account`). The api key
    /// travels in the JSON body over the (loopback or api-key-gated) control
    /// channel; the response echoes only a masked form, so nothing here logs
    /// or displays the raw key.
    async fn perform_remote_add(&mut self, api_key: String) {
        let Backend::Remote(remote) = &mut self.backend else {
            return;
        };
        let url = format!("{}/llmux/add-account", remote.base_url);
        let mut request = remote
            .client
            .post(&url)
            .json(&serde_json::json!({ "api_key": api_key }));
        if let Some(key) = &remote.api_key {
            request = request.header("x-api-key", key);
        }
        let message = match request.send().await {
            Ok(response) if response.status().is_success() => {
                let body = response
                    .json::<serde_json::Value>()
                    .await
                    .unwrap_or(serde_json::Value::Null);
                let name = body["name"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| "account".into());
                StatusMsg::ok(add_account_status(&name, response_added(&body)))
            }
            Ok(response) => StatusMsg::fail(format!("add account failed: {}", response.status())),
            Err(err) => StatusMsg::fail(format!("add account failed: {err}")),
        };
        self.set_status(message);
    }

    /// Perform the queued remote removal (`POST /llmux/remove-account`).
    async fn perform_remote_remove(&mut self, name: String) {
        let Backend::Remote(remote) = &mut self.backend else {
            return;
        };
        let url = format!("{}/llmux/remove-account", remote.base_url);
        let mut request = remote
            .client
            .post(&url)
            .json(&serde_json::json!({ "name": name, "confirm": true }));
        if let Some(key) = &remote.api_key {
            request = request.header("x-api-key", key);
        }
        let message = match request.send().await {
            Ok(response) if response.status().is_success() => {
                StatusMsg::ok(format!("removed account {name}"))
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let detail = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                    .unwrap_or_else(|| status.to_string());
                StatusMsg::fail(format!("remove {name} failed: {detail}"))
            }
            Err(err) => StatusMsg::fail(format!("remove {name} failed: {err}")),
        };
        self.set_status(message);
    }

    /// Run a new browser login in THIS client and inject the resulting account
    /// into the daemon (issue #4). The OAuth flow runs HERE — a localhost
    /// callback for Anthropic/Codex/OpenRouter/Gemini (51121 for Gemini), a
    /// printed device code for Grok (which has no callback) — and the minted
    /// credential is then injected: in-process when local, via
    /// `POST /llmux/inject-account` when attached.
    /// ONE code path: the only fork is where the credential lands.
    ///
    /// MUST run with the raw terminal SUSPENDED (the flow prints prompts and
    /// may read a pasted code from stdin) — the event loop handles
    /// suspend/resume around this call. No token is logged or rendered raw; the
    /// status line shows only the resulting account name.
    async fn perform_login(&mut self, kind: LoginKind) {
        let client = reqwest::Client::new();
        // Build the account by running the client-side login. The profile fetch
        // (Anthropic only) hits the public upstream with the user's own token.
        let account = match kind {
            LoginKind::Anthropic => {
                let upstream = match &self.backend {
                    Backend::Local(state) => state.config.upstream.clone(),
                    // The attached client has no copy of the daemon's config;
                    // the profile endpoint is the public Anthropic API.
                    Backend::Remote(_) => crate::config::DEFAULT_UPSTREAM.to_string(),
                };
                match crate::cli::login::oauth_login_to_account(&client, &upstream).await {
                    Ok(account) => account,
                    Err(err) => {
                        self.set_status(StatusMsg::fail(format!("login failed: {err}")));
                        return;
                    }
                }
            }
            LoginKind::Codex => {
                let token_url = match &self.backend {
                    Backend::Local(state) => state.config.codex.token_url.clone(),
                    Backend::Remote(_) => crate::config::DEFAULT_CODEX_TOKEN_URL.to_string(),
                };
                match crate::auth::codex::login_codex_interactive(&client, &token_url).await {
                    Ok(account) => account,
                    Err(err) => {
                        self.set_status(StatusMsg::fail(format!("codex login failed: {err}")));
                        return;
                    }
                }
            }
            LoginKind::Grok => {
                // Device-code flow (docs/grok/spec.md T1): there is NO
                // localhost callback, so the operator has to carry the code to
                // the browser themselves — print it exactly like
                // `llmux login --grok` does (the terminal is suspended here).
                //
                // Deliberately NOT the shared `client`: the poll POST returns
                // the refresh token, so it must run on the no-redirect client
                // (a redirect to an off-boundary host would resend it).
                let grok_client = crate::auth::grok::oauth_http_client();
                let discovery = match crate::auth::grok::discover(&grok_client).await {
                    Ok(discovery) => discovery,
                    Err(err) => {
                        self.set_status(StatusMsg::fail(format!("grok login failed: {err}")));
                        return;
                    }
                };
                let device =
                    match crate::auth::grok::request_device_code(&grok_client, &discovery).await {
                        Ok(device) => device,
                        Err(err) => {
                            self.set_status(StatusMsg::fail(format!("grok login failed: {err}")));
                            return;
                        }
                    };
                println!("Open:  {}", device.open_url());
                println!("Code:  {}", device.user_code);
                crate::auth::oauth::open_browser(device.open_url());
                println!("Waiting for authorization (Ctrl-C to abort)...");
                let bundle = match crate::auth::grok::poll_token(
                    &grok_client,
                    &discovery.token_endpoint,
                    &device,
                )
                .await
                {
                    Ok(bundle) => bundle,
                    Err(err) => {
                        self.set_status(StatusMsg::fail(format!("grok login failed: {err}")));
                        return;
                    }
                };
                match crate::auth::grok::account_from_bundle(&bundle, &discovery.token_endpoint) {
                    Ok(account) => account,
                    Err(err) => {
                        self.set_status(StatusMsg::fail(format!("grok login failed: {err}")));
                        return;
                    }
                }
            }
            LoginKind::OpenRouter => {
                // PKCE with a localhost callback (docs/openrouter/spec.md §R5).
                // The exchange yields a long-lived API key, so nothing here
                // expires. No stdin paste fallback in the TUI arm — a headless
                // box should use the CLI, which the error message names.
                let api_key = match crate::auth::openrouter::login_interactive(&client).await {
                    Ok(api_key) => api_key,
                    Err(err) => {
                        self.set_status(StatusMsg::fail(format!(
                            "openrouter login failed: {err} — try `llmux login --openrouter --paste`"
                        )));
                        return;
                    }
                };
                // Key-shaped "labels" are discarded, not persisted — OpenRouter
                // returns the masked KEY as the label for a PKCE-minted key.
                let label = crate::auth::openrouter::fetch_key_label(
                    &client,
                    crate::auth::openrouter::KEY_INFO_URL,
                    &api_key,
                )
                .await
                .filter(|l| !crate::cli::login::label_is_key_shaped(l))
                .unwrap_or_default();
                // The NAME is left empty on purpose: `inject_account` (and the
                // inject endpoint, which allows an empty name for openrouter)
                // derives `or:<label>` / `or:key-N` inside its read-merge-write
                // closure. Deriving it here from a separate snapshot would
                // reintroduce a TOCTOU window and a second naming scheme.
                AccountConfig {
                    name: String::new(),
                    credential: crate::config::AccountCredential::OpenRouter {
                        api_key,
                        label: label.trim().to_string(),
                    },
                }
            }
            LoginKind::Gemini => {
                // Google Antigravity OAuth, localhost:51121 callback
                // (docs/gemini/spec.md §R2). Same pre-browser ToS note as
                // `llmux login --gemini` (the terminal is suspended here).
                let gemini = match &self.backend {
                    Backend::Local(state) => state.config.gemini.clone(),
                    // The flow runs HERE, in the attached client, which has
                    // no copy of the daemon's config: use this machine's
                    // config file (and environment) — the OAuth client is
                    // the operator's, never a built-in default.
                    Backend::Remote(_) => crate::config::load()
                        .map(|config| config.gemini)
                        .unwrap_or_default(),
                };
                let oauth = match gemini.oauth_client() {
                    Ok(oauth) => oauth,
                    Err(err) => {
                        self.set_status(StatusMsg::fail(format!("gemini login failed: {err}")));
                        return;
                    }
                };
                println!("{}", crate::auth::gemini::GEMINI_TOS_NOTE);
                match crate::auth::gemini::login_gemini_interactive(
                    &client,
                    &oauth,
                    &gemini.token_url,
                    &gemini.upstream,
                    &gemini.user_agent,
                )
                .await
                {
                    Ok(account) => account,
                    Err(err) => {
                        self.set_status(StatusMsg::fail(format!("gemini login failed: {err}")));
                        return;
                    }
                }
            }
        };

        // Inject: in-process locally, or relay to the daemon when attached.
        match &mut self.backend {
            Backend::Local(state) => match state.inject_account(account) {
                Ok((name, outcome)) => self.set_status(StatusMsg::ok(login_status(
                    &name,
                    matches!(outcome, crate::config::Upsert::Added),
                ))),
                Err(err) => {
                    self.set_status(StatusMsg::fail(format!("login persist failed: {err}")))
                }
            },
            Backend::Remote(_) => self.perform_remote_inject(account).await,
        }
    }

    /// Relay a freshly-minted OAuth/Codex account to the daemon
    /// (`POST /llmux/inject-account`). The credential travels in the JSON body
    /// over the (loopback or api-key-gated) control channel; the response
    /// echoes only a masked access token, so nothing here logs or displays the
    /// raw token.
    async fn perform_remote_inject(&mut self, account: AccountConfig) {
        let Backend::Remote(remote) = &mut self.backend else {
            return;
        };
        let url = format!("{}/llmux/inject-account", remote.base_url);
        // `AccountConfig` serializes to the `{name, type, …credential}` shape
        // the inject endpoint deserializes (the flattened, type-tagged enum).
        let mut request = remote.client.post(&url).json(&account);
        if let Some(key) = &remote.api_key {
            request = request.header("x-api-key", key);
        }
        let message = match request.send().await {
            Ok(response) if response.status().is_success() => {
                let body = response
                    .json::<serde_json::Value>()
                    .await
                    .unwrap_or(serde_json::Value::Null);
                let name = body["name"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| account.name.clone());
                StatusMsg::ok(login_status(&name, response_added(&body)))
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let detail = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                    .unwrap_or_else(|| status.to_string());
                StatusMsg::fail(format!("login inject failed: {detail}"))
            }
            Err(err) => StatusMsg::fail(format!("login inject failed: {err}")),
        };
        self.set_status(message);
    }

    /// `R` — re-read the config file and swap the roster into the live pool
    /// (`AccountPool::reload_accounts` keeps window/cooldown state for
    /// surviving accounts). Local mode only: an attached client must not
    /// reload the DAEMON's roster from the CLIENT's config file.
    fn reload(&mut self) {
        let now = SystemTime::now();
        match &self.backend {
            Backend::Local(state) => match crate::config::load() {
                Ok(config) => {
                    let n = config.accounts.len();
                    state.pool.reload_accounts(&config.accounts);
                    let msg = format!("config reloaded: {n} account(s)");
                    state.hub.push_note(msg.clone(), false, now);
                    self.set_status(StatusMsg::ok(msg));
                }
                Err(err) => self.set_status(StatusMsg::fail(format!("reload failed: {err}"))),
            },
            Backend::Remote(_) => {
                self.set_status(StatusMsg::warn(
                    "reload: local mode only — restart applies on the server host",
                ));
            }
        }
    }

    /// Enter in select mode — switch the scheduler to the chosen account.
    ///
    /// Queues the switch as a control operation in BOTH backends (.prd/16 S3):
    /// the daemon service refreshes the target's usage FIRST — including an
    /// already-active target, which is the whole point of re-selecting one —
    /// and then commits through the pool.
    ///
    /// There is deliberately NO client-side eligibility precheck any more. It
    /// judged CACHED windows, so an account whose quota was reset upstream
    /// read as exhausted and the TUI refused the very switch that would have
    /// refreshed it. The pool stays the authority on pause / auth / cooldown
    /// and refuses with its own reason.
    fn try_manual_switch(&mut self, idx: usize, view: Option<&DashboardView>) {
        let Some(view) = view else { return };
        let now = SystemTime::now();
        // The cursor indexes DISPLAY rows (selection order), not config order.
        let order = view.display_order(self.account_sort, now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        let account = target.id.0.clone();
        let pending = if view.snapshot.is_current(&target.id) {
            format!("refreshing {account} and re-selecting it…")
        } else {
            format!("switching to {account}…")
        };
        self.queue_control(ControlOp::Switch { account }, pending);
    }
}

// ---------------------------------------------------------------------------
// Usage-control execution (.prd/16) — one semantics, two transports
// ---------------------------------------------------------------------------

/// How stale a reset-inventory observation may be before ENTERING the accounts
/// overlay asks for a fresh one. Entry refresh is a convenience (populate the
/// counts); the explicit `f` refresh has no floor at all.
const ENTRY_REFRESH_FLOOR: Duration = Duration::from_secs(60);

/// Does entering the accounts overlay warrant one refresh? Only when an
/// account that HAS reset entitlements (codex) carries no observation, or one
/// older than [`ENTRY_REFRESH_FLOOR`]. No such account → no refresh at all, so
/// a claude-only roster never pays for this.
fn accounts_entry_refresh_due(view: &DashboardView, now: SystemTime) -> bool {
    view.snapshot
        .accounts
        .iter()
        .filter(|a| view::has_usage_controls(a.credential_kind))
        .any(|a| match view.usage_control(&a.id.0) {
            None => true,
            Some(control) => match control.last_refresh_ms {
                None => true,
                Some(ms) => {
                    let observed = UNIX_EPOCH + Duration::from_millis(ms);
                    now.duration_since(observed)
                        .map(|age| age > ENTRY_REFRESH_FLOOR)
                        // A clock that went backwards is not evidence of
                        // freshness; treat it as due.
                        .unwrap_or(false)
                }
            },
        })
}

/// Status for a completed refresh: how many accounts were refreshed, the
/// reset inventory when it is known, and EVERY failure. A partial failure is
/// never summarized into a green: any failed account makes it a `fail`. With
/// several accounts the failures are the message's lines — one per account,
/// shown as a toast card — instead of one row joined with ` · ` that the
/// terminal's edge cut off.
fn refresh_message(response: &crate::proxy::usage_controls::RefreshResponse) -> StatusMsg {
    let failures: Vec<String> = response
        .results
        .iter()
        .filter(|r| !r.ok)
        .map(|r| {
            format!(
                "{}: {}",
                r.account,
                r.error.as_deref().unwrap_or("unspecified error")
            )
        })
        .collect();
    let ok = response.results.len() - failures.len();
    if failures.is_empty() {
        // One account: name its inventory; many: the count is the signal.
        if let [only] = &response.results[..] {
            let control = only.usage_control.as_ref();
            return StatusMsg::ok(format!(
                "refreshed {} · resets {}",
                only.account,
                match control.and_then(|c| c.available_resets) {
                    None => "unknown".to_string(),
                    Some(owned) => match control.and_then(|c| c.applicable_resets) {
                        Some(applicable) => format!("{owned} owned · {applicable} applicable now"),
                        None => format!("{owned} owned · applicable unknown"),
                    },
                }
            ));
        }
        return StatusMsg::ok(format!("refreshed {ok} account(s)"));
    }
    let head = format!("refresh: {ok} ok, {} FAILED", failures.len());
    if response.results.len() == 1 {
        return StatusMsg::fail(format!("{head} — {}", failures.join(" · ")));
    }
    StatusMsg::fail(head).with_lines(failures)
}

/// Status for a terminal redemption outcome. The codes stay distinct, and a
/// failed follow-up read is appended as a warning on a SUCCESS — never turned
/// into a retryable failure. Outcome: a redemption is `ok` (a `warn` once a
/// note or a stale read rides on it); a replay or nothing to reset spent
/// nothing and is a `warn`; no credit or a cooldown is a `fail` — the reset
/// the operator asked for did not happen.
fn consume_message(
    account: &str,
    response: &crate::proxy::usage_controls::ConsumeResponse,
) -> StatusMsg {
    use crate::auth::codex_usage::ResetOutcome;
    use twolab_tui::banner::Outcome;
    let mut outcome = match response.outcome {
        ResetOutcome::Reset => Outcome::Ok,
        ResetOutcome::AlreadyRedeemed | ResetOutcome::NothingToReset => Outcome::Warn,
        ResetOutcome::NoCredit | ResetOutcome::Cooldown => Outcome::Fail,
    };
    let head = match response.outcome {
        ResetOutcome::Reset => format!(
            "reset redeemed for {account} ({} window(s))",
            response.windows_reset
        ),
        ResetOutcome::AlreadyRedeemed => {
            format!("{account}: already redeemed — this replay spent no second reset")
        }
        ResetOutcome::NothingToReset => {
            format!("{account}: nothing to reset — no reset was spent")
        }
        ResetOutcome::NoCredit => {
            format!("{account}: no reset credit available upstream — nothing was spent")
        }
        ResetOutcome::Cooldown => {
            format!("{account}: upstream is in a post-reset cooldown — nothing was spent")
        }
    };
    let mut text = format!("{head} · request id {}", response.request_id);
    let noted = response.applicability_warning.is_some() || response.refresh_warning.is_some();
    if noted && outcome == Outcome::Ok {
        outcome = Outcome::Warn;
    }
    if let Some(warning) = &response.applicability_warning {
        text.push_str(&format!(" · note: {warning}"));
    }
    if let Some(warning) = &response.refresh_warning {
        text.push_str(&format!(
            " · WARNING (stale read, redemption stands): {warning}"
        ));
    }
    StatusMsg::new(outcome, text)
}

/// Turn a TERMINAL consume response into a client outcome.
///
/// R1 receipt-close failure: upstream can reach a terminal outcome while the
/// daemon fails to release its durable pending receipt, and then it keeps
/// reporting the redemption as pending. That combination is a SUCCESS whose
/// receipt is still open — the client keeps the safe retry identity (the same
/// id, never a new one) instead of declaring the redemption resolved, and it
/// never re-offers the spend as a fresh redemption.
fn consume_result(
    account: &str,
    response: crate::proxy::usage_controls::ConsumeResponse,
) -> ControlResult {
    let message = consume_message(account, &response);
    let still_pending = response
        .usage_control
        .as_ref()
        .and_then(|control| control.pending_request_id.clone());
    match still_pending {
        // A success whose receipt is still open is at most a warning.
        Some(request_id) => ControlResult {
            message: StatusMsg::new(
                match message.outcome {
                    twolab_tui::banner::Outcome::Ok => twolab_tui::banner::Outcome::Warn,
                    other => other,
                },
                format!(
                    "{} · the daemon still reports this redemption as pending — it was NOT \
                     re-spent; only request id {request_id} may be repeated",
                    message.text
                ),
            ),
            hold: Some(PendingRedemption {
                account: account.to_string(),
                request_id,
                credit_id: response.credit_id,
            }),
            resolved: None,
        },
        None => ControlResult {
            message,
            hold: None,
            resolved: Some(account.to_string()),
        },
    }
}

/// Status for a manual switch. The pre-switch refresh warning is shown
/// SEPARATELY from the switch result — a stale read never reads as a failed
/// switch (it is a `warn`, not a `fail`), and a successful switch never
/// hides it.
fn switch_message(response: &crate::proxy::usage_controls::SwitchResponse) -> StatusMsg {
    let head = format!("switched to {} (manual)", response.current);
    match &response.refresh_warning {
        Some(warning) => StatusMsg::warn(format!(
            "{head} · WARNING: usage refresh failed — {warning}"
        )),
        None => StatusMsg::ok(head),
    }
}

/// The redemption confirm prompt: names the ACCOUNT and the ONE reset, with
/// the inventory (unknown stays unknown).
pub(crate) fn reset_confirm_status(
    account: &str,
    owned: Option<u64>,
    applicable: Option<u64>,
) -> String {
    let inventory = reset_inventory(owned, applicable);
    format!(
        "redeem ONE rate-limit reset for {account}? ({inventory}) — y confirms, Esc cancels; \
         this spends a reset upstream and cannot be undone"
    )
}

/// An account's reset inventory as the gate and its status line say it.
pub(crate) fn reset_inventory(owned: Option<u64>, applicable: Option<u64>) -> String {
    match owned {
        None => "inventory unknown".to_string(),
        Some(owned) => match applicable {
            Some(applicable) => format!("{owned} owned · {applicable} applicable now"),
            None => format!("{owned} owned · applicable unknown"),
        },
    }
}

/// A crossterm key as twolab-tui's [`KeyInput`](twolab_tui::KeyInput), the
/// input every catalog widget takes. The handlers see only the key code
/// (Ctrl-C is handled above them), so no modifiers are set. Written here
/// rather than through the crate's `crossterm` feature, which would change
/// the lockfiles of llmux and the three islands crates for ten lines.
fn key_input(code: KeyCode) -> Option<twolab_tui::KeyInput> {
    use twolab_tui::Key;
    let key = match code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        _ => return None,
    };
    Some(twolab_tui::KeyInput::new(key))
}

/// [`key_input`] with the key's Ctrl / Alt / Shift — what a text field needs
/// for Ctrl+U, Ctrl+W, Ctrl+Y and Ctrl+←/→.
fn key_event_input(key: &KeyEvent) -> Option<twolab_tui::KeyInput> {
    let mut input = key_input(key.code)?;
    input.mods = twolab_tui::Mods {
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    };
    Some(input)
}

/// What the limits field takes: `5h,7d,fbl` percents or fractions.
fn limits_char(c: char) -> bool {
    c.is_ascii_digit() || c == ',' || c == '.' || c == ' '
}

/// What a key does on a two-option confirm gate (option 1 confirms, option 2
/// cancels — the card drawn by `ui::draw_gate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateAnswer {
    Confirm,
    Cancel,
    /// ↑↓ / Tab moved the mark to this option.
    Moved(usize),
    Ignored,
}

/// Run `code` through the gate's [`ChoiceForm`](twolab_tui::choice_form::ChoiceForm)
/// with option `marked` marked: digits answer at once, Enter answers the
/// marked option, Esc cancels, ↑↓ / Tab move the mark. `y` and `n` stay as
/// aliases for options 1 and 2 (the keys these gates always had).
fn gate_answer(code: KeyCode, marked: usize) -> GateAnswer {
    use twolab_tui::choice_form::{ChoiceAnswer, ChoiceEvent, ChoiceForm, ChoiceState};
    match code {
        KeyCode::Char('y' | 'Y') => return GateAnswer::Confirm,
        KeyCode::Char('n' | 'N') => return GateAnswer::Cancel,
        _ => {}
    }
    let Some(key) = key_input(code) else {
        return GateAnswer::Ignored;
    };
    let form = ChoiceForm::new("", ["confirm", "cancel"]).no_other();
    let mut state = ChoiceState::at(marked);
    match form.key(&mut state, key) {
        ChoiceEvent::Answered(ChoiceAnswer::Option(0)) => GateAnswer::Confirm,
        ChoiceEvent::Answered(_) | ChoiceEvent::Cancelled => GateAnswer::Cancel,
        ChoiceEvent::Moved => GateAnswer::Moved(state.selected()),
        ChoiceEvent::Edited | ChoiceEvent::Ignored => GateAnswer::Ignored,
    }
}

/// Map a daemon-side control error onto a client outcome. The two errors that
/// carry an id are the load-bearing ones: an UNCERTAIN redemption keeps its
/// own key, and a PENDING one hands back the key that must be retried instead.
fn control_error_result(
    account: &str,
    err: &crate::proxy::usage_controls::UsageControlError,
) -> ControlResult {
    use crate::proxy::usage_controls::UsageControlError as E;
    match err {
        E::Uncertain { request_id, .. } => ControlResult {
            message: StatusMsg::fail(format!(
                "{account}: redemption outcome UNCERTAIN — {err}. Press R again to retry the \
                 SAME id ({request_id}); a new one could spend a second reset"
            )),
            hold: Some(PendingRedemption {
                account: account.to_string(),
                request_id: request_id.clone(),
                credit_id: None,
            }),
            resolved: None,
        },
        E::Pending {
            request_id,
            credit_id,
            ..
        } => ControlResult {
            message: StatusMsg::warn(format!("{account}: {err}")),
            hold: Some(PendingRedemption {
                account: account.to_string(),
                request_id: request_id.clone(),
                credit_id: credit_id.clone(),
            }),
            resolved: None,
        },
        // Everything else refused before spending anything.
        other => ControlResult {
            message: StatusMsg::fail(format!("{account}: {other}")),
            hold: None,
            resolved: None,
        },
    }
}

/// Execute one control operation against the IN-PROCESS daemon (local mode).
async fn run_control_local(state: &crate::proxy::server::AppState, op: ControlOp) -> ControlResult {
    use crate::proxy::usage_controls::ConsumeRequest;
    match op {
        ControlOp::Refresh { account } => match state.refresh_usage(account.as_deref()).await {
            Ok(response) => ControlResult {
                message: refresh_message(&response),
                hold: None,
                resolved: None,
            },
            Err(err) => ControlResult {
                message: StatusMsg::fail(format!("refresh failed: {err}")),
                hold: None,
                resolved: None,
            },
        },
        ControlOp::Reset {
            account,
            request_id,
            credit_id,
        } => {
            let request = ConsumeRequest {
                account: account.clone(),
                redeem_request_id: request_id,
                credit_id,
                confirm: true,
            };
            match state.consume_reset(&request).await {
                Ok(response) => consume_result(&account, response),
                Err(err) => control_error_result(&account, &err),
            }
        }
        ControlOp::Switch { account } => match state.manual_switch(&account).await {
            Ok(response) => ControlResult {
                message: switch_message(&response),
                hold: None,
                resolved: None,
            },
            Err(err) => ControlResult {
                message: StatusMsg::fail(format!("switch to {account} failed: {err}")),
                hold: None,
                resolved: None,
            },
        },
    }
}

/// Execute one control operation against an ATTACHED daemon over HTTP — the
/// same endpoints `llmux accounts refresh/reset` uses.
async fn run_control_remote(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    op: ControlOp,
) -> ControlResult {
    let post = |path: &str, body: serde_json::Value| {
        let mut request = client.post(format!("{base_url}{path}")).json(&body);
        if let Some(key) = api_key {
            request = request.header("x-api-key", key);
        }
        request
    };
    match op {
        ControlOp::Refresh { account } => {
            let body = match &account {
                Some(name) => serde_json::json!({ "account": name }),
                None => serde_json::json!({}),
            };
            match post("/llmux/refresh-usage", body).send().await {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    if !status.is_success() {
                        return ControlResult {
                            message: StatusMsg::fail(format!(
                                "refresh failed: {}",
                                http_error_text(status, &text)
                            )),
                            hold: None,
                            resolved: None,
                        };
                    }
                    match serde_json::from_str(&text) {
                        Ok(parsed) => ControlResult {
                            message: refresh_message(&parsed),
                            hold: None,
                            resolved: None,
                        },
                        Err(err) => ControlResult {
                            message: StatusMsg::fail(format!(
                                "refresh: unreadable response ({err})"
                            )),
                            hold: None,
                            resolved: None,
                        },
                    }
                }
                Err(err) => ControlResult {
                    message: StatusMsg::fail(format!("refresh failed: {err}")),
                    hold: None,
                    resolved: None,
                },
            }
        }
        ControlOp::Reset {
            account,
            request_id,
            credit_id,
        } => {
            let mut body = serde_json::json!({
                "account": account,
                "redeem_request_id": request_id,
                "confirm": true,
            });
            // Omitted, never null, when no credit was named.
            if let Some(id) = &credit_id {
                body["credit_id"] = serde_json::Value::String(id.clone());
            }
            match post("/llmux/reset-credits/consume", body).send().await {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    if status.is_success() {
                        return match serde_json::from_str(&text) {
                            Ok(parsed) => consume_result(&account, parsed),
                            // A 2xx we cannot read is UNCERTAIN: the
                            // redemption may have happened. Keep the key.
                            Err(err) => ControlResult {
                                message: StatusMsg::fail(format!(
                                    "{account}: redemption outcome UNCERTAIN — unreadable \
                                     response ({err}). Press R again to retry the SAME id \
                                     ({request_id})"
                                )),
                                hold: Some(PendingRedemption {
                                    account,
                                    request_id,
                                    credit_id,
                                }),
                                resolved: None,
                            },
                        };
                    }
                    remote_consume_error(&account, &text, request_id, credit_id, status)
                }
                // A transport failure is uncertain by definition.
                Err(err) => ControlResult {
                    message: StatusMsg::fail(format!(
                        "{account}: redemption outcome UNCERTAIN — {err}. Press R again to \
                         retry the SAME id ({request_id})"
                    )),
                    hold: Some(PendingRedemption {
                        account,
                        request_id,
                        credit_id,
                    }),
                    resolved: None,
                },
            }
        }
        ControlOp::Switch { account } => {
            match post("/llmux/switch", serde_json::json!({ "account": account }))
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    if !status.is_success() {
                        return ControlResult {
                            message: StatusMsg::fail(format!(
                                "switch to {account} failed: {}",
                                http_error_text(status, &text)
                            )),
                            hold: None,
                            resolved: None,
                        };
                    }
                    let message = match serde_json::from_str(&text) {
                        Ok(parsed) => switch_message(&parsed),
                        Err(_) => StatusMsg::ok(format!("switched to {account} (manual)")),
                    };
                    ControlResult {
                        message,
                        hold: None,
                        resolved: None,
                    }
                }
                Err(err) => ControlResult {
                    message: StatusMsg::fail(format!("switch to {account} failed: {err}")),
                    hold: None,
                    resolved: None,
                },
            }
        }
    }
}

/// Classify a FAILED consume response by the daemon's stable error code (the
/// same codes the CLI branches on).
fn remote_consume_error(
    account: &str,
    body: &str,
    request_id: String,
    credit_id: Option<String>,
    status: reqwest::StatusCode,
) -> ControlResult {
    let value: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let code = value["error"]["type"].as_str().unwrap_or("");
    let message = value["error"]["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| status.to_string());
    match code {
        "pending_redemption" => {
            let pending = value["pending_request_id"]
                .as_str()
                .unwrap_or(&request_id)
                .to_string();
            let pending_credit = value["pending_credit_id"]
                .as_str()
                .map(str::to_string)
                .or(credit_id);
            ControlResult {
                message: StatusMsg::warn(format!(
                    "{account}: {message} — press R again to retry THAT id"
                )),
                hold: Some(PendingRedemption {
                    account: account.to_string(),
                    request_id: pending,
                    credit_id: pending_credit,
                }),
                resolved: None,
            }
        }
        "uncertain" => {
            let id = value["request_id"]
                .as_str()
                .unwrap_or(&request_id)
                .to_string();
            ControlResult {
                message: StatusMsg::fail(format!(
                    "{account}: redemption outcome UNCERTAIN — {message}. Press R again to \
                     retry the SAME id ({id})"
                )),
                hold: Some(PendingRedemption {
                    account: account.to_string(),
                    request_id: id,
                    credit_id,
                }),
                resolved: None,
            }
        }
        _ => ControlResult {
            message: StatusMsg::fail(format!("{account}: redemption refused — {message}")),
            hold: None,
            resolved: None,
        },
    }
}

/// The daemon's `error.message` for a non-2xx, falling back to the status.
fn http_error_text(status: reqwest::StatusCode, body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| status.to_string())
}

/// Initialize the terminal for the dashboard: raw mode + alternate screen +
/// a full clear + native mouse capture (Feature B), then a fresh
/// [`ratatui::Terminal`] over stdout. Done by hand instead of
/// `ratatui::try_init`, which installs a new panic hook on every call and so
/// stacked one more per login suspend/resume; [`install_panic_restore_hook`]
/// installs ours once per process. Every call site uses this helper (and
/// [`restore_terminal`]) so the enable/disable always pair up — including the
/// login suspend/resume path that re-inits the terminal mid-session.
///
/// If any step after raw mode fails, the terminal is restored before the
/// error is returned: a failed start must not leave the outer terminal in raw
/// mode on the alternate screen, the very state that garbles the next launch.
///
/// Why the clear: `Terminal::new` starts from blank buffers, so the first draw
/// only writes non-blank cells. If the outer terminal is already on the
/// alternate screen (a previous TUI killed before it could leave it), entering
/// it again is a no-op and the stale content shows through every blank cell.
/// A bare `Clear(All)` makes the screen match the blank back buffer. Colours
/// and attributes are reset first because `CSI 2J` paints with the current
/// background. Not `Terminal::clear()`: it first queries the cursor position
/// (`CSI 6n`) and waits on stdin for the reply, which can time out under a
/// slow multiplexer.
fn init_terminal() -> std::io::Result<ratatui::DefaultTerminal> {
    install_panic_restore_hook();
    enable_raw_mode()?;
    TERMINAL_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
    crossterm::execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        ResetColor,
        SetAttribute(Attribute::Reset),
        Clear(ClearType::All),
        EnableMouseCapture
    )
    .and_then(|()| {
        // A paste arrives whole (`Event::Paste`): a text field takes it at
        // the caret as one edit, and a line break in a pasted key does not
        // submit half of it. Best effort — a terminal without bracketed
        // paste types the paste key by key, as before.
        let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
        ratatui::Terminal::new(CrosstermBackend::new(std::io::stdout()))
    })
    .inspect_err(|_| restore_terminal())
}

/// True while the dashboard owns the terminal: set right after raw mode is
/// enabled, cleared by the first [`restore_terminal`]. Makes restore
/// idempotent — the init error path, the panic hook, [`TerminalGuard`] and the
/// login suspend can all call it, but only the first call after an init
/// writes anything. A second `?1049l` would land on the main screen and can
/// move the cursor.
static TERMINAL_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Install, once per process, a panic hook that runs [`restore_terminal`] and
/// then the previous hook, so a panic while the dashboard owns the terminal
/// leaves it fully restored (mouse capture off, cursor shown, raw mode off,
/// main screen) before the panic message prints. When the TUI is not active
/// the restore is a no-op ([`TERMINAL_ACTIVE`]).
fn install_panic_restore_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            prev(info);
        }));
    });
}

/// Tear down the terminal: disable mouse capture and show the cursor FIRST,
/// then `ratatui::restore` (disable raw mode + leave alternate screen). The
/// inverse of [`init_terminal`]. A no-op unless the terminal is active
/// ([`TERMINAL_ACTIVE`]), so a second call writes nothing. The cursor is shown
/// here because the dashboard hides it while drawing and otherwise only the
/// old `Terminal`'s drop shows it again — on the login suspend path that drop
/// runs after the re-init, so the OAuth prompt would run with a hidden cursor.
fn restore_terminal() {
    if !TERMINAL_ACTIVE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let _ = crossterm::execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        crossterm::cursor::Show
    );
    ratatui::restore();
}

/// Restores the terminal when dropped. Held by [`run_local`] and
/// [`run_remote`] for the whole session, so the terminal is restored even when
/// their future is dropped mid-await — `cli` races `run_local` against the
/// server task in a `tokio::select!`, and a server exit drops the TUI future
/// before any code after its `.await` runs.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Run the in-process dashboard over live server state until quit.
///
/// Terminal lifecycle via [`init_terminal`] / [`restore_terminal`]: raw mode +
/// alternate screen + mouse capture, all undone on every exit path — normal
/// return, a dropped future ([`TerminalGuard`]) and the panic hook.
pub async fn run_local(state: crate::proxy::server::AppState) -> std::io::Result<()> {
    let mut terminal = init_terminal()?;
    let guard = TerminalGuard;
    let mut app = App::new(Backend::Local(Box::new(state)));
    let result = event_loop(&mut terminal, &mut app, None).await;
    drop(guard);
    result
}

/// Attach to a running daemon: poll `GET /llmux/dashboard` every second
/// and render the fetched document with the same draw code as local mode. A
/// lost connection shows a reconnect banner and keeps retrying — never
/// crashes the client.
pub async fn run_remote(opts: RemoteOptions) -> std::io::Result<()> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(std::io::Error::other)?;
    let (tx, rx) = mpsc::channel(4);
    let fetcher = tokio::spawn(fetch_loop(
        client.clone(),
        opts.base_url.clone(),
        opts.api_key.clone(),
        tx,
    ));
    let mut terminal = init_terminal()?;
    let guard = TerminalGuard;
    let mut app = App::new(Backend::Remote(Box::new(Remote {
        client,
        base_url: opts.base_url,
        api_key: opts.api_key,
        pid: opts.pid,
        doc: None,
        connected: false,
        pending_codex: None,
        pending_settings: None,
        pending_pause: None,
        pending_limits: None,
        pending_mode: None,
        pending_add: None,
        pending_remove: None,
        pending_grok: None,
    })));
    let result = event_loop(&mut terminal, &mut app, Some(rx)).await;
    drop(guard);
    fetcher.abort();
    result
}

/// Poll the dashboard endpoint forever, reporting documents and losses to
/// the event loop. Exits only when the TUI side hangs up.
async fn fetch_loop(
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    tx: mpsc::Sender<FetchMsg>,
) {
    let url = format!("{base_url}/llmux/dashboard");
    let mut interval = tokio::time::interval(FETCH_TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let mut request = client.get(&url);
        if let Some(key) = &api_key {
            request = request.header("x-api-key", key);
        }
        let msg = match request.send().await {
            Ok(response) if response.status().is_success() => {
                match response.json::<DashboardDoc>().await {
                    Ok(doc) => FetchMsg::Doc(Box::new(doc)),
                    Err(_) => FetchMsg::Lost,
                }
            }
            _ => FetchMsg::Lost,
        };
        if tx.send(msg).await.is_err() {
            return; // dashboard quit
        }
    }
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    mut fetch: Option<mpsc::Receiver<FetchMsg>>,
) -> std::io::Result<()> {
    let mut render = tokio::time::interval(RENDER_TICK);
    render.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Sessions overlay (`s`) loads the persisted raw-io log on the blocking pool
    // and delivers the folded timeline here — mirrors the remote fetch channel so
    // the read+parse+fold never blocks this select (it once froze the TUI ~10s).
    let (sess_tx, mut sess_rx) = mpsc::channel::<SessionsLoad>(4);
    app.sessions_tx = Some(sess_tx);
    // Infinite-scroll history hydration (UI-5): the one-shot blocking load of
    // `activity.jsonl` delivers here, same pattern as the sessions channel.
    let (hist_tx, mut hist_rx) = mpsc::channel::<Vec<activity::Completed>>(1);
    app.history_tx = Some(hist_tx);
    // Raw request/response viewer (UI-7): the background raw-io lookup delivers
    // here — same pattern as the sessions channel (never block this select).
    let (raw_tx, mut raw_rx) = mpsc::channel::<RawLoad>(2);
    app.raw_tx = Some(raw_tx);
    // Raw-viewer exports (UI-8 copy/save): the blocking pool delivers the
    // outcome flash here so a wedged clipboard tool / slow disk never freezes
    // this select.
    let (clip_tx, mut clip_rx) = mpsc::channel::<ClipResult>(4);
    app.clip_tx = Some(clip_tx);
    // Usage controls (.prd/16): refresh / redeem / switch each wait on an
    // upstream round-trip, so they run in a background task and deliver here.
    // Running them inline would freeze input for the length of the call — and
    // a frozen TUI is exactly how an operator ends up pressing Enter twice.
    let (control_tx, mut control_rx) = mpsc::channel::<ControlResult>(4);
    app.control_tx = Some(control_tx);
    // Keys panel (keys-history K): the windowed/filtered usage query answers
    // here — SQLite locally, HTTP when attached — so neither ever blocks this
    // select or the render tick.
    let (keys_tx, mut keys_rx) = mpsc::channel::<KeysLoad>(4);
    app.keys_tx = Some(keys_tx);
    // Input is event-driven, not polled: `EventStream` parks on the terminal fd
    // (mio) and only wakes the task when a real key/mouse/resize/paste arrives.
    // At idle (no input) this contributes zero wakeups, unlike a fixed-interval
    // poll which fired ~30×/s reading nothing. See issue #14 (idle quiescence).
    let mut events = EventStream::new();
    // Whether the terminal is capturing the mouse for us: `init_terminal`
    // turned it on; copy mode (`v`) turns it off and back.
    let mut captured = true;

    loop {
        let held = app.copy_mode;
        let mut input = false;
        let mut redraw = tokio::select! {
            _ = render.tick() => {
                app.frame = app.frame.wrapping_add(1);
                true
            }
            // Wakes only when the terminal actually has an event. Handle the one
            // the stream delivered, then drain any *already-ready* siblings (a
            // multi-byte paste) without blocking, so a burst is one redraw.
            Some(event) = events.next() => {
                input = true;
                drain_input(app, event?)?
            }
            msg = async {
                match fetch.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            }, if fetch.is_some() => {
                match msg {
                    Some(msg) => app.apply_fetch(msg),
                    // Fetch task gone (cannot happen before abort) — show
                    // the reconnect banner instead of spinning.
                    None => {
                        app.apply_fetch(FetchMsg::Lost);
                        fetch = None;
                    }
                }
                true
            }
            // A progressive session-load partial arrived — replace the timeline
            // with the fold-so-far and keep `sessions_loading` until the final
            // (`done`) partial, so the overlay fills in and its title tracks the
            // read progress instead of appearing all-at-once at the end.
            Some(load) = sess_rx.recv() => {
                app.sessions = load.sessions;
                // Deliveries arrive in fold order — re-apply the user's sort
                // so a mid-load `o` press survives the next partial.
                app.session_sort.apply(&mut app.sessions);
                app.sessions_pct = load.pct;
                app.sessions_loading = !load.done;
                true
            }
            // The background history load finished (UI-5 infinite scroll) —
            // materialize the first page immediately so a live window that
            // folds to a single row (scroll ceiling 0) can start scrolling
            // (review R3-1), then scrolling pages the rest in.
            Some(history) = hist_rx.recv() => {
                app.history_completed = Some(history);
                app.history_loading = false;
                if let Some(view) = app.view(SystemTime::now()) {
                    app.grow_history_take(&view);
                }
                true
            }
            // A background raw-record fetch resolved (UI-7) — hand it to the
            // open modal (stale deliveries are ignored inside).
            Some(load) = raw_rx.recv() => {
                app.apply_raw_load(load);
                true
            }
            // A background export (UI-8 copy/save) finished — free the
            // single-flight slot so the next queued export can dispatch, then
            // flash the outcome on the modal that requested it (the flash is
            // gated on generation; freeing the slot is unconditional).
            Some(result) = clip_rx.recv() => {
                app.clip_finished();
                app.apply_clip_result(result);
                true
            }
            // A usage-control operation finished (.prd/16): free the
            // single-flight slot, show the outcome, and update the held
            // redemption key (only a TERMINAL outcome releases it).
            Some(result) = control_rx.recv() => {
                app.apply_control_result(result);
                true
            }
            // A keys-usage query answered (keys-history K): apply it unless a
            // newer question has already been asked.
            Some(load) = keys_rx.recv() => {
                app.apply_keys_load(load);
                true
            }
        };
        if let Some(op) = app.take_pending_control() {
            app.spawn_control(op);
            redraw = true;
        }
        if let Some(req) = app.take_pending_raw() {
            app.spawn_raw_fetch(req);
            redraw = true;
        }
        if let Some(req) = app.take_pending_keys() {
            app.spawn_keys_query(req);
            redraw = true;
        }
        if let Some(req) = app.next_clip_if_idle() {
            app.spawn_clip(req);
            redraw = true;
        }
        if let Some(codex) = app.take_pending_codex() {
            app.perform_remote_codex(codex).await;
            redraw = true;
        }
        if let Some((settings, origin)) = app.take_pending_settings() {
            app.perform_remote_settings(settings, origin).await;
            redraw = true;
        }
        if let Some((account, paused)) = app.take_pending_pause() {
            app.perform_remote_pause(account, paused).await;
            redraw = true;
        }
        if let Some((account, limits)) = app.take_pending_limits() {
            app.perform_remote_limits(account, limits).await;
            redraw = true;
        }
        if let Some(mode) = app.take_pending_mode() {
            app.perform_remote_mode(mode).await;
            redraw = true;
        }
        if let Some(api_key) = app.take_pending_add() {
            app.perform_remote_add(api_key).await;
            redraw = true;
        }
        if let Some(name) = app.take_pending_remove() {
            app.perform_remote_remove(name).await;
            redraw = true;
        }
        if let Some(effort) = app.take_pending_grok() {
            app.perform_remote_grok(effort).await;
            redraw = true;
        }
        // A new browser login needs the RAW terminal back: the OAuth flow
        // prints prompts and may read a pasted code from stdin, which would
        // corrupt the alternate-screen TUI. Suspend (restore the terminal),
        // run the flow, then re-init (which clears the screen) and force a
        // full redraw. The fetch poller (remote mode) keeps running in the
        // background meanwhile.
        if let Some(kind) = app.take_pending_login() {
            restore_terminal();
            app.perform_login(kind).await;
            *terminal = init_terminal()?;
            captured = true;
            redraw = true;
        }
        if app.should_quit {
            return Ok(());
        }
        // Copy mode (`v`) hands the mouse to the terminal so its own
        // selection works, and takes it back on the way out.
        if captured == app.copy_mode {
            if app.copy_mode {
                crossterm::execute!(std::io::stdout(), DisableMouseCapture)?;
            } else {
                crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
            }
            captured = !app.copy_mode;
        }
        if frame_due(redraw, held, app.copy_mode, input) {
            let view = app.view(SystemTime::now());
            // Anchor a scrolled-into-history viewport against newly arrived rows
            // (UI-6 item 4) before `chrome()` snapshots the scroll offset.
            if let Some(view) = view.as_ref() {
                app.preserve_scroll_on_new_activity(view);
            }
            // One clock reading per frame: waiting toasts move up at it and
            // every age on screen is measured from it.
            let now = Instant::now();
            let clock = app.clock_at(now);
            app.toasts.tick(clock);
            let chrome = app.chrome_at(now);
            // Capture the activity panel's hit-test layout from this frame so a
            // left-click in the next input drain maps to the right entry.
            let mut hits = None;
            terminal.draw(|frame| {
                ui::draw(frame, view.as_ref(), &chrome, &mut hits);
                // NO_COLOR (https://no-color.org): take the colour out of
                // the frame itself. crossterm already skips colour codes
                // under NO_COLOR, but it still writes an empty `ESC[m` for
                // each colour change — a full SGR reset that wiped bold and
                // reverse too. A colourless frame emits no colour changes,
                // so weight stays; red text turns bold.
                if no_color() {
                    twolab_tui::theme::decolor(frame.buffer_mut());
                }
            })?;
            let main = hits.unwrap_or_default();
            app.hits = main.hits;
            app.sessions_chrome = main.sessions_table;
            app.table_scroll = main.table_scroll;
            app.config_chrome = main.config_rows;
            app.raw_chrome = main.raw_modal.clone().unwrap_or_default();
            app.menu_chrome = main.menu;
            // The open text field's window, as drawn.
            app.track_field(main.field_width);
            // The Logs pager clamps a held offset itself; one at (or past)
            // the bottom follows the tail again — unless a selection holds it.
            app.logs_area = main.logs_area;
            if let Some(pos) = main.logs {
                app.logs_pos = pos;
                let selecting = app.logs_sel.is_some_and(|s| s.is_visible());
                if !selecting && app.logs_scroll.is_some_and(|top| top >= pos.max) {
                    app.logs_scroll = None;
                }
            }
            // Reconcile the input modal (UI-6 item 3) against what the frame
            // could actually render: `Some(layout)` is where it drew (the
            // clamped scroll, the page size and scrollbar track the next key
            // or click acts on), `None` means the entry aged out of the ring
            // (lookup failed) so the modal closes gracefully.
            if let Some(modal) = app.input_modal.as_mut() {
                match main.input_modal.clone() {
                    Some(layout) => adopt_modal_layout(&mut modal.state, layout),
                    None => app.input_modal = None,
                }
            }
            // Same reconcile for the account detail modal (.prd/19 rule 6):
            // `None` means the pinned account is no longer in the snapshot so
            // the modal closes.
            if let Some(modal) = app.account_modal.as_mut() {
                match main.account_modal.clone() {
                    Some(layout) => adopt_modal_layout(&mut modal.state, layout),
                    None => app.account_modal = None,
                }
            }
            // Clamp the raw viewer's scroll offset against what this frame
            // rendered (UI-7/UI-8). Unlike the input modal, no draw ⇒ no
            // clamp — the modal owns its content and never closes on aging.
            if let (Some(modal), Some(raw)) = (app.raw_modal.as_mut(), main.raw_modal.as_ref()) {
                modal.scroll = modal.scroll.min(raw.max_scroll);
            }
        }
    }
}

/// Keep what a frame drew on the stored modal state: the scroll position the
/// render clamped and the layout the next key or click acts on (a held
/// scrollbar grab stays).
fn adopt_modal_layout(state: &mut ModalState, layout: twolab_tui::modal::ModalLayout) {
    state.scroll = layout.pos.top;
    state.layout = layout;
}

/// Whether this turn of the event loop draws a frame. Copy mode holds the
/// frame still while it stays on — the text under the terminal's own
/// selection must not move — so then only input draws (a resize; the keys it
/// swallows ask for nothing), not the render tick or a background delivery.
/// Entering and leaving it draw: `copy_mode_before` and `copy_mode_now`
/// differ.
fn frame_due(redraw: bool, copy_mode_before: bool, copy_mode_now: bool, input: bool) -> bool {
    redraw && (input || !(copy_mode_before && copy_mode_now))
}

/// Handle `first` (the event the `EventStream` just woke us with), then drain
/// any *already-ready* terminal events without blocking (`poll(ZERO)` is a
/// non-blocking readiness check), so a multi-byte paste is one redraw rather
/// than one per byte. Returns whether anything warrants a redraw.
fn drain_input(app: &mut App, first: Event) -> std::io::Result<bool> {
    let mut dirty = false;
    // Built once per drain: key handlers read the same frame the user saw.
    let mut view: Option<Option<DashboardView>> = None;
    apply_event(app, first, &mut view, &mut dirty);
    while crossterm::event::poll(Duration::ZERO)? {
        apply_event(app, crossterm::event::read()?, &mut view, &mut dirty);
    }
    Ok(dirty)
}

/// Dispatch one terminal event into the app, lazily building the per-drain view
/// the first time a key/mouse handler needs it and flipping `dirty` when the
/// event warrants a redraw.
fn apply_event(
    app: &mut App,
    event: Event,
    view: &mut Option<Option<DashboardView>>,
    dirty: &mut bool,
) {
    match event {
        Event::Key(key) => {
            let view = view.get_or_insert_with(|| app.view(SystemTime::now()));
            let held = app.copy_mode;
            app.on_key(key, view.as_ref());
            // A key copy mode swallowed changes nothing on screen: no redraw,
            // so the text under the terminal's selection holds still.
            if !(held && app.copy_mode) {
                *dirty = true;
            }
        }
        Event::Mouse(mouse) => {
            let view = view.get_or_insert_with(|| app.view(SystemTime::now()));
            if app.on_mouse(mouse, view.as_ref()) {
                *dirty = true;
            }
        }
        Event::Paste(text) => {
            let view = view.get_or_insert_with(|| app.view(SystemTime::now()));
            if app.on_paste(&text, view.as_ref()) {
                *dirty = true;
            }
        }
        Event::Resize(_, _) => *dirty = true,
        _ => {}
    }
}

/// Read the persisted raw-io log and fold it into a session timeline (issue #34).
///
/// The path is resolved exactly like the daemon's capture path
/// (`$XDG_STATE_HOME/llmux/raw-io.jsonl`). A missing/unreadable file, or no state
/// dir, yields an empty timeline — best-effort, never panics. Unparseable lines
/// are skipped (the same tolerance `raw_io::prune` applies on rewrite). Only the
/// metadata each record carries is folded; no prompt content is retained.
/// UI-5 infinite-scroll knobs: how many FOLDED render rows past the current
/// scroll depth [`App::grow_history_take`] targets, how many raw entries it
/// appends per fold-recheck step, how many such steps ONE state transition
/// may take (a folded wall then loads progressively across events instead of
/// traversing the whole history in one), and how close to the loaded end the
/// scroll must get before [`App::request_history`] arms.
const HISTORY_PAGE: usize = 300;
const HISTORY_CHUNK: usize = 512;
const HISTORY_GROW_CHUNKS: usize = 4;
const HISTORY_ARM_MARGIN: i64 = 40;

/// Status wording for an account add (`Upsert::Added` vs `Updated`). A login
/// or key-add over an EXISTING identity replaces that account's credentials in
/// place — reporting it as "added" reads like a second account appeared
/// (`docs/keys-history/spec.md` L-6).
fn add_account_status(name: &str, added: bool) -> String {
    let verb = if added { "added" } else { "updated" };
    format!("{verb} account {name}")
}

/// Status wording for a completed browser/API login, same distinction.
fn login_status(name: &str, added: bool) -> String {
    let verb = if added { "added" } else { "updated" };
    format!("logged in: {verb} {name}")
}

/// Read the `added` flag off an add/inject endpoint response. A daemon too old
/// to send the field keeps the historical "added" wording — never a claim of
/// an update it did not report.
fn response_added(value: &serde_json::Value) -> bool {
    value["added"].as_bool().unwrap_or(true)
}

/// Percent-encode one query-string VALUE (model names ride in from request
/// bodies — `anthropic/claude-x`, `gpt-4.1`, anything). Unreserved characters
/// pass through; everything else becomes `%XX`, so the comma the `models`
/// parameter separates on can never be forged by a name.
fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Lines the input modal (UI-6 item 3) scrolls per PgUp/PgDn keystroke.
const MODAL_PAGE: u16 = 10;

/// Cap on queued (not-yet-dispatched) exports (UI-8). Single-flight dispatch
/// already bounds concurrent work to one; this bounds the BACKLOG so a user
/// spamming the button against a slow/wedged exporter can't grow the deque
/// without limit. Human-paced presses never approach it.
const CLIP_QUEUE_MAX: usize = 8;

/// Ceiling on hydrated history entries — far beyond any real scrolling
/// session, purely a memory backstop against a huge persisted file.
const HISTORY_CAP: usize = 100_000;

/// True when an attach base URL points at THIS machine (review M1): host is
/// `localhost` or a loopback IP (v4 `127.0.0.0/8`, v6 `::1`, bracketed or
/// not). No `url`-crate dependency — the accepted inputs are the daemon
/// base URLs llmux itself builds (`http://host:port`). Unparseable → false
/// (fail closed: no local-history splice under an unknown remote).
fn base_url_is_loopback(base_url: &str) -> bool {
    let rest = match base_url.split_once("://") {
        Some((_, rest)) => rest,
        None => base_url,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Strip :port — for bracketed IPv6 the bracket closes the host; for
    // everything else the LAST ':' starts the port (bare IPv6 has many).
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        match authority.rsplit_once(':') {
            // `a:b:c` with multiple ':' and no brackets = a bare IPv6 host.
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.contains(':') => h,
            _ => authority,
        }
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Pure merge for [`App::extend_with_history`] (unit-tested): append up to
/// `take` history rows behind the live newest-first list — only entries
/// strictly OLDER than the oldest loaded row (the persisted tail overlaps
/// the live ring; the timestamp cut dedupes). A single linear pass, NO
/// folding — how many rows to materialize (`take`) is decided on state
/// transitions by [`App::grow_history_take`], keeping the per-frame path a
/// bounded clone (review R3-2).
fn extend_completed_with_history(
    completed: &mut Vec<activity::Completed>,
    history: &[activity::Completed],
    take: usize,
) {
    if take == 0 {
        return;
    }
    let oldest = completed.last().map(|c| c.at);
    completed.extend(
        history
            .iter()
            .filter(|c| oldest.is_none_or(|o| c.at < o))
            .take(take)
            .cloned(),
    );
}

/// Blocking read+replay of the persisted activity log
/// (`$XDG_STATE_HOME/llmux/activity.jsonl`) into a newest-first completed
/// list for the infinite-scroll history (UI-5). Reuses the same
/// `PersistedRequest` replay the daemon boots with, so schema/versioning
/// tolerance is identical. Missing state dir / file → empty. NOTE: resolves
/// the LOCAL state path — attaching to a daemon on another host yields
/// nothing (that host's file isn't here); remote history paging is the
/// sqlite/storage follow-up issue.
fn load_history() -> Vec<activity::Completed> {
    let Some(path) = crate::cli::daemon::activity_log_path() else {
        return Vec::new();
    };
    let mut log = activity::ActivityLog::new(HISTORY_CAP);
    // Unbounded replay (`up_to = u64::MAX`): unlike daemon boot hydration
    // there is no concurrent-append double-count here — the ring is a
    // point-in-time snapshot and the strictly-older merge cut dedupes any
    // overlap with live rows.
    let _ = log.load_persisted_prefix(&path, u64::MAX);
    log.completed().cloned().collect()
}

/// One progressive delivery from the streaming session loader
/// (`stream_sessions`). Each carries the fold of ALL records read so far (fold
/// is pure and cheap, so re-folding the accumulator per chunk is fine), a `pct`
/// of the file consumed for the overlay title, and `done` on the final (EOF)
/// delivery so the receiver drops the loading state.
struct SessionsLoad {
    sessions: Vec<crate::session::Session>,
    done: bool,
    pct: u8,
}

/// Records accumulated between folds. Large enough that the per-chunk re-fold
/// of the whole accumulator stays negligible (fold is a single linear pass)
/// while partials still arrive often enough to feel progressive on a multi-MB
/// log.
const SESSIONS_CHUNK_RECORDS: usize = 4096;

/// `bytes_read*100/file_len`, clamped to `0..=100`; 100 for an empty/unknown
/// file so the title never shows a bogus overshoot.
fn sessions_load_pct(bytes_read: u64, file_len: u64) -> u8 {
    if file_len == 0 {
        return 100;
    }
    (bytes_read.saturating_mul(100) / file_len).min(100) as u8
}

/// Streaming, progressive variant of the session load: folds the raw-io
/// store's hot partitions (metadata columns only — no bodies), newest day
/// first, delivering a partial after each day; then, while a legacy
/// `raw-io.jsonl` is still awaiting import, its records too (the store's
/// imported copies are skipped then, so nothing is counted twice). The final
/// partial carries `done = true`; a missing store delivers a single empty,
/// done partial so the overlay's loading state always clears. Runs on the
/// blocking pool.
fn stream_sessions(tx: &mpsc::Sender<SessionsLoad>) {
    let send_empty_done = || {
        let _ = tx.blocking_send(SessionsLoad {
            sessions: Vec::new(),
            done: true,
            pct: 100,
        });
    };
    let Some(path) = crate::cli::daemon::raw_io_path() else {
        send_empty_done();
        return;
    };
    let store = crate::proxy::raw_store::store_for(&path);
    let legacy_pending = path.exists();
    let total_days = store.hot_days().len().max(1) as u64;
    let mut rows: Vec<crate::session::SessionRow> = Vec::new();
    let mut days_read: u64 = 0;
    let mut receiver_gone = false;
    store.for_each_session_rows(legacy_pending, |day_rows| {
        rows.extend(day_rows);
        days_read += 1;
        let partial = SessionsLoad {
            sessions: crate::session::fold_session_rows(&rows),
            done: false,
            pct: sessions_load_pct(days_read, total_days + u64::from(legacy_pending)),
        };
        receiver_gone = tx.blocking_send(partial).is_err();
        !receiver_gone
    });
    if receiver_gone {
        return;
    }
    if legacy_pending {
        if let Ok(file) = std::fs::File::open(&path) {
            let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
            let mut reader = std::io::BufReader::new(file);
            let mut line = String::new();
            let mut bytes_read: u64 = 0;
            let mut since_fold: usize = 0;
            loop {
                line.clear();
                match std::io::BufRead::read_line(&mut reader, &mut line) {
                    Ok(0) => break,
                    Ok(n) => bytes_read = bytes_read.saturating_add(n as u64),
                    Err(_) => break,
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(rec) = serde_json::from_str::<crate::proxy::raw_io::RawIoRecord>(trimmed)
                {
                    rows.push(crate::session::SessionRow::from_record(&rec));
                    since_fold += 1;
                }
                if since_fold >= SESSIONS_CHUNK_RECORDS {
                    since_fold = 0;
                    let partial = SessionsLoad {
                        sessions: crate::session::fold_session_rows(&rows),
                        done: false,
                        pct: sessions_load_pct(bytes_read, file_len),
                    };
                    if tx.blocking_send(partial).is_err() {
                        return;
                    }
                }
            }
        }
    }
    let _ = tx.blocking_send(SessionsLoad {
        sessions: crate::session::fold_session_rows(&rows),
        done: true,
        pct: 100,
    });
}

/// Parse the limits-editor input: comma/space-separated percents in order
/// `5h, 7d, fbl` — `"90,98,98"`, `"90"` (5h only), `"90,,98"` (skip 7d),
/// `""` (all global). Values are percents; `>1` divides by 100, `<=1` is
/// taken as a fraction, so `0.9` and `90` mean the same ceiling.
/// Human status line for a settings apply: live fields vs restart-required —
/// a wrote-but-needs-restart change must never read as applied (contract C6).
///
/// Outcome: `ok` only when everything is live; a value that waits for a
/// restart is a `warn` (saved, not applied), and so is a change that changed
/// nothing.
fn settings_status(applied: &[&str], restart: &[&str]) -> StatusMsg {
    match (applied.is_empty(), restart.is_empty()) {
        (false, true) => StatusMsg::ok(format!("config: applied live — {}", applied.join(", "))),
        (true, false) => StatusMsg::warn(format!(
            "config: saved — takes effect on daemon restart ({})",
            restart.join(", ")
        )),
        (false, false) => StatusMsg::warn(format!(
            "config: {} applied live; {} on restart",
            applied.join(", "),
            restart.join(", ")
        )),
        (true, true) => StatusMsg::warn("config: no change"),
    }
}

/// Raw-io retention in days as the confirm gate says it.
fn retention_label(days: u64) -> String {
    if days == 0 {
        "keep forever".into()
    } else {
        format!("{days}d")
    }
}

/// The confirm gate's question for a blast-radius config change: the
/// setting, its current value `old` → the new one, and what applying it
/// does — on two lines, so the card keeps the consequence in view.
fn confirm_prompt(
    action: ui::ConfigAction,
    req: &crate::proxy::server::SettingsRequest,
    old: &str,
    view: Option<&DashboardView>,
) -> String {
    use ui::ConfigAction as A;
    let onoff = |v: Option<bool>| if v.unwrap_or_default() { "on" } else { "off" };
    let pct = |v: Option<f64>| format!("{:.0}%", v.unwrap_or_default() * 100.0);
    let text = |v: &Option<String>| v.clone().unwrap_or_default();
    let (change, effect) = match action {
        A::SchedMode => {
            use crate::config::SchedulerMode;
            let current = view.map_or(SchedulerMode::Default, |v| v.select_params.mode);
            let next = match current {
                SchedulerMode::Default => SchedulerMode::RoundRobin,
                SchedulerMode::RoundRobin => SchedulerMode::Default,
            };
            (
                format!(
                    "Change scheduler mode: {} → {}?",
                    current.label(),
                    next.label()
                ),
                "It changes the live scheduling policy.".to_string(),
            )
        }
        A::RoutingEnabled => (
            format!(
                "Set routing.enabled: {old} → {}?",
                onoff(req.routing_enabled)
            ),
            "It reroutes LIVE traffic.".to_string(),
        ),
        A::RawIoEnabled => (
            format!("Set raw-io capture: {old} → {}?", onoff(req.raw_io_enabled)),
            if req.raw_io_enabled.unwrap_or_default() {
                "It writes FULL request/response payloads to disk.".to_string()
            } else {
                "Request/response payloads stop being written to disk.".to_string()
            },
        ),
        A::Upstream => (
            format!("Set upstream: {old} → {}?", text(&req.upstream)),
            "Restart required; a wrong URL breaks ALL requests.".to_string(),
        ),
        A::CodexUpstream => (
            format!("Set codex upstream: {old} → {}?", text(&req.codex_upstream)),
            "Restart required.".to_string(),
        ),
        A::Ceiling5h => (
            format!("Set 5h ceiling: {old} → {}?", pct(req.five_hour_max)),
            "A 0% ceiling locks that window's scheduling.".to_string(),
        ),
        A::Ceiling7d => (
            format!("Set 7d ceiling: {old} → {}?", pct(req.seven_day_max)),
            "A 0% ceiling locks that window's scheduling.".to_string(),
        ),
        A::CeilingFbl => (
            format!("Set fbl ceiling: {old} → {}?", pct(req.fable_weekly_max)),
            "A 0% ceiling locks that window's scheduling.".to_string(),
        ),
        A::RawIoRetention => (
            format!(
                "Reduce raw-io retention: {old} → {}?",
                retention_label(req.raw_io_retention_days.unwrap_or_default())
            ),
            "Older history will be pruned.".to_string(),
        ),
        _ => ("Apply this change?".to_string(), String::new()),
    };
    if effect.is_empty() {
        change
    } else {
        format!("{change}\n{effect}")
    }
}

/// Parse the edit-prompt text for `action` into a validated-enough
/// [`SettingsRequest`] (the daemon re-validates authoritatively).
fn parse_config_input(
    action: ui::ConfigAction,
    raw: &str,
    _view: Option<&DashboardView>,
) -> Result<crate::proxy::server::SettingsRequest, String> {
    use crate::proxy::server::SettingsRequest;
    use ui::ConfigAction as A;
    let mut req = SettingsRequest::default();
    let pct = || -> Result<f64, String> {
        let v: f64 = raw.parse().map_err(|_| format!("not a number: {raw:?}"))?;
        if !(0.0..=100.0).contains(&v) {
            return Err("percent must be 0-100".into());
        }
        Ok(v / 100.0)
    };
    match action {
        A::Ceiling5h => req.five_hour_max = Some(pct()?),
        A::Ceiling7d => req.seven_day_max = Some(pct()?),
        A::CeilingFbl => req.fable_weekly_max = Some(pct()?),
        A::UsageMaxAge => {
            req.usage_max_age_secs = Some(raw.parse().map_err(|_| format!("not seconds: {raw:?}"))?)
        }
        A::GradientSpeed => {
            req.tui_gradient_speed =
                Some(raw.parse().map_err(|_| format!("not a number: {raw:?}"))?)
        }
        A::RoutingDefaultGroup => req.routing_default_group = Some(raw.to_string()),
        A::RoutingOnEmptyGroup => req.routing_on_empty_group = Some(raw.to_string()),
        A::RawIoRetention => {
            req.raw_io_retention_days = Some(raw.parse().map_err(|_| format!("not days: {raw:?}"))?)
        }
        A::RawIoMaxBody => {
            req.raw_io_max_body_bytes =
                Some(raw.parse().map_err(|_| format!("not bytes: {raw:?}"))?)
        }
        A::Upstream => req.upstream = Some(raw.to_string()),
        A::CodexUpstream => req.codex_upstream = Some(raw.to_string()),
        A::ProxyPort => {
            req.proxy_port = Some(raw.parse().map_err(|_| format!("not a port: {raw:?}"))?)
        }
        A::ProxyMaxBody => {
            req.proxy_max_request_bytes =
                Some(raw.parse().map_err(|_| format!("not bytes: {raw:?}"))?)
        }
        _ => return Err("not an editable value".into()),
    }
    Ok(req)
}

fn parse_limits_input(raw: &str) -> Result<crate::config::AccountLimits, String> {
    let mut vals: [Option<f64>; 3] = [None, None, None];
    let cleaned = raw.trim();
    if !cleaned.is_empty() {
        let parts: Vec<&str> = cleaned.split(',').collect();
        if parts.len() > 3 {
            return Err("at most 3 values: 5h,7d,fbl".into());
        }
        for (i, part) in parts.iter().enumerate() {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let n: f64 = part
                .parse()
                .map_err(|_| format!("not a number: {part:?}"))?;
            let frac = if n > 1.0 { n / 100.0 } else { n };
            if !(frac > 0.0 && frac <= 1.0) {
                return Err(format!("{part:?} out of range (1..=100%)"));
            }
            vals[i] = Some(frac);
        }
    }
    Ok(crate::config::AccountLimits {
        five_hour_max: vals[0],
        seven_day_max: vals[1],
        fable_weekly_max: vals[2],
    })
}

#[cfg(test)]
mod tests {
    /// `LLMUX_AMBIGUOUS_WIDE=1` turns the ASCII rules on; unset, empty,
    /// `0` or any other value leaves the box-drawn rules.
    #[test]
    fn ambiguous_wide_is_on_only_for_1() {
        use std::ffi::OsStr;
        let on = super::ambiguous_wide_from;
        assert!(on(Some(OsStr::new("1"))));
        for off in ["", "0", "true", " 1"] {
            assert!(!on(Some(OsStr::new(off))), "{off:?}");
        }
        assert!(!on(None));
    }

    #[test]
    fn parse_limits_input_covers_percent_fraction_partial_and_clear() {
        let p = super::parse_limits_input;
        let l = p("90,98,98").unwrap();
        assert_eq!(l.five_hour_max, Some(0.90));
        assert_eq!(l.seven_day_max, Some(0.98));
        assert_eq!(l.fable_weekly_max, Some(0.98));
        // Fractions work too; positions may be skipped.
        let l = p("0.5,,97").unwrap();
        assert_eq!(l.five_hour_max, Some(0.5));
        assert_eq!(l.seven_day_max, None);
        assert_eq!(l.fable_weekly_max, Some(0.97));
        // Empty = clear all overrides.
        assert!(p("").unwrap().is_empty());
        assert!(p("  ").unwrap().is_empty());
        // Errors: junk, out of range, too many.
        assert!(p("abc").is_err());
        assert!(p("0").is_err());
        assert!(p("101").is_err());
        assert!(p("1,2,3,4").is_err());
    }

    use super::*;

    /// An `App` on a remote backend — buildable without a terminal, so the
    /// key-handling state machine (issue #4 new-login flow) is unit-testable.
    fn remote_app() -> App {
        let client = reqwest::Client::new();
        App::new(Backend::Remote(Box::new(Remote {
            client,
            base_url: "http://localhost:3456".into(),
            api_key: None,
            pid: None,
            doc: None,
            connected: false,
            pending_codex: None,
            pending_settings: None,
            pending_pause: None,
            pending_limits: None,
            pending_mode: None,
            pending_add: None,
            pending_remove: None,
            pending_grok: None,
        })))
    }

    #[test]
    fn new_login_picker_moves_and_enter_queues_chosen_provider() {
        let mut app = remote_app();
        // Enter the picker (bypassing the env-dependent browser check).
        app.mode = Mode::NewLogin { idx: 0 };

        // Down moves to the Codex row.
        app.on_key_new_login(KeyCode::Down, 0);
        assert_eq!(app.mode, Mode::NewLogin { idx: 1 });

        // Enter queues that provider for the event loop and returns to Normal.
        app.on_key_new_login(KeyCode::Enter, 1);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.take_pending_login(), Some(LoginKind::Codex));
        // Drained exactly once.
        assert_eq!(app.take_pending_login(), None);
    }

    #[test]
    fn new_login_picker_enter_on_first_row_picks_anthropic() {
        let mut app = remote_app();
        app.mode = Mode::NewLogin { idx: 0 };
        app.on_key_new_login(KeyCode::Enter, 0);
        assert_eq!(app.take_pending_login(), Some(LoginKind::Anthropic));
    }

    #[test]
    fn new_login_picker_esc_cancels_without_queueing() {
        let mut app = remote_app();
        app.mode = Mode::NewLogin { idx: 1 };
        app.on_key_new_login(KeyCode::Esc, 1);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.take_pending_login(), None, "cancel queues nothing");
    }

    #[test]
    fn new_login_picker_up_clamps_at_top() {
        let mut app = remote_app();
        app.mode = Mode::NewLogin { idx: 0 };
        app.on_key_new_login(KeyCode::Up, 0);
        assert_eq!(app.mode, Mode::NewLogin { idx: 0 });
    }

    /// Down stops on the LAST provider row, so the picker can never queue an
    /// out-of-range `LoginKind::ALL` index (which would panic on Enter).
    #[test]
    fn new_login_picker_down_clamps_at_last_row() {
        let mut app = remote_app();
        let last = LoginKind::ALL.len() - 1;
        app.mode = Mode::NewLogin { idx: last };
        app.on_key_new_login(KeyCode::Down, last);
        assert_eq!(app.mode, Mode::NewLogin { idx: last });
        assert_eq!(
            last, 4,
            "picker rows: anthropic, codex, grok, openrouter, gemini"
        );
    }

    /// Rows 2–4 are the providers added alongside Claude/Codex: Enter on
    /// each queues THAT provider, so the picker order and `ALL` cannot drift.
    #[test]
    fn new_login_picker_enter_queues_grok_openrouter_and_gemini_rows() {
        let mut app = remote_app();
        app.mode = Mode::NewLogin { idx: 2 };
        app.on_key_new_login(KeyCode::Enter, 2);
        assert_eq!(app.take_pending_login(), Some(LoginKind::Grok));

        app.mode = Mode::NewLogin { idx: 3 };
        app.on_key_new_login(KeyCode::Enter, 3);
        assert_eq!(app.take_pending_login(), Some(LoginKind::OpenRouter));

        app.mode = Mode::NewLogin { idx: 4 };
        app.on_key_new_login(KeyCode::Enter, 4);
        assert_eq!(app.take_pending_login(), Some(LoginKind::Gemini));
    }

    /// The picker is a select list: a digit picks its provider at once, and
    /// any key that is not a picker key still cancels.
    #[test]
    fn new_login_picker_digits_pick_and_other_keys_cancel() {
        let mut app = remote_app();
        app.mode = Mode::NewLogin { idx: 0 };
        app.on_key_new_login(KeyCode::Char('4'), 0);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.take_pending_login(), Some(LoginKind::OpenRouter));

        app.mode = Mode::NewLogin { idx: 2 };
        app.on_key_new_login(KeyCode::Char('j'), 2);
        assert_eq!(app.mode, Mode::NewLogin { idx: 3 }, "j moves down");
        app.on_key_new_login(KeyCode::End, 3);
        assert_eq!(app.mode, Mode::NewLogin { idx: 4 }, "End: last provider");
        app.on_key_new_login(KeyCode::Char('x'), 4);
        assert_eq!(app.mode, Mode::Normal, "any other key cancels");
        assert_eq!(app.take_pending_login(), None);
    }

    /// The config cursor moves over editable rows only (read-only rows are
    /// the list's disabled rows) and stops at both ends.
    #[test]
    fn config_cursor_skips_read_only_rows_and_stops_at_the_ends() {
        let view = control_view();
        let mut app = remote_app();
        app.overlay = Overlay::Config;
        let chrome = app.chrome();
        let editable: Vec<usize> = (0..60)
            .filter(|&i| ui::config_row_action(&view, &chrome, i).is_some())
            .collect();
        // Rows 5 and 6 (per-account limits, paused accounts) are read-only.
        assert!(!editable.contains(&5) && !editable.contains(&6));
        app.config_cursor = 4;
        app.on_key_config(KeyCode::Down, Some(&view));
        assert_eq!(app.config_cursor, 7, "Down skips the read-only rows");
        app.on_key_config(KeyCode::Char('k'), Some(&view));
        assert_eq!(app.config_cursor, 4, "k skips them going up");
        app.on_key_config(KeyCode::Home, Some(&view));
        assert_eq!(app.config_cursor, editable[0]);
        app.on_key_config(KeyCode::Up, Some(&view));
        assert_eq!(app.config_cursor, editable[0], "Up stops at the top");
        app.on_key_config(KeyCode::End, Some(&view));
        let last = *editable.last().expect("editable rows");
        assert_eq!(app.config_cursor, last);
        app.on_key_config(KeyCode::Down, Some(&view));
        assert_eq!(app.config_cursor, last, "Down stops at the bottom");
    }

    #[test]
    fn headless_fallback_names_llmux_login_per_mode() {
        // Attached: point the operator at the daemon host.
        let remote = headless_login_hint(true);
        assert!(remote.contains("llmux login"), "{remote}");
        assert!(remote.contains("daemon host"), "{remote}");
        // Local: this host.
        let local = headless_login_hint(false);
        assert!(local.contains("llmux login"), "{local}");
        assert!(local.contains("this host"), "{local}");
    }

    /// Every picker row names its provider distinctly — the label is the only
    /// thing the operator reads before Enter starts an irreversible browser
    /// flow, so no two rows may look alike.
    #[test]
    fn login_kind_labels_distinguish_providers() {
        assert!(LoginKind::Anthropic.label().contains("Anthropic"));
        assert!(LoginKind::Codex.label().contains("Codex"));
        assert!(LoginKind::Grok.label().contains("Grok"));
        assert!(LoginKind::OpenRouter.label().contains("OpenRouter"));
        assert!(LoginKind::Gemini.label().contains("Gemini"));
        assert_eq!(LoginKind::ALL.len(), 5);
        let labels: std::collections::BTreeSet<&str> =
            LoginKind::ALL.iter().map(|k| k.label()).collect();
        assert_eq!(labels.len(), LoginKind::ALL.len(), "labels are unique");
    }

    // --- issue #5: overlay key routing -------------------------------------

    /// Issue #5 acceptance (state machine): every summoned overlay follows the
    /// same open → `Esc` → closed cycle, and MAIN state held on `App` (the
    /// activity scroll, the click-expanded row — the things that "keep updating
    /// underneath") is preserved across the open and the close, never reset.
    /// `Esc` always lands back on `Overlay::None` (MAIN), from any overlay.
    #[test]
    fn open_overlay_preserves_main_state_then_esc_returns_to_main() {
        let view = stats_view_with_account();
        // Seed MAIN-owned state so we can prove the overlay round-trip leaves it
        // untouched (MAIN keeps its place / expansion underneath the overlay).
        let expanded = activity::ActivityKey {
            at_ms: 7,
            method: "POST".into(),
            path: "/v1/messages".into(),
            status: 200,
        };

        // Each shortcut summons one overlay; `Esc` always closes it. The whole
        // round-trip is driven through the unified `on_key` entry point, so the
        // production overlay-aware routing (open via MAIN, close via the active
        // overlay's `Esc`) is what's under test.
        for open_key in [KeyCode::Char('a'), KeyCode::Char('g'), KeyCode::Char('l')] {
            let mut app = remote_app();
            app.activity_scroll = 3;
            app.expanded_activity = Some(expanded.clone());
            assert_eq!(app.overlay, Overlay::None, "starts on MAIN");

            // Open: the active overlay is set; MAIN-owned state is untouched.
            app.on_key(press(open_key), Some(&view));
            assert_ne!(
                app.overlay,
                Overlay::None,
                "{open_key:?} summons an overlay"
            );
            assert_eq!(
                app.activity_scroll, 3,
                "MAIN scroll preserved under overlay"
            );
            assert_eq!(
                app.expanded_activity.as_ref(),
                Some(&expanded),
                "MAIN expansion preserved under overlay"
            );

            // Esc: back to MAIN, with MAIN state still intact.
            app.on_key(press(KeyCode::Esc), Some(&view));
            assert_eq!(app.overlay, Overlay::None, "Esc returns to MAIN");
            assert_eq!(
                app.activity_scroll, 3,
                "MAIN scroll survives the round-trip"
            );
            assert_eq!(
                app.expanded_activity.as_ref(),
                Some(&expanded),
                "MAIN expansion survives the round-trip"
            );
        }
    }

    /// UI-3 U6: tab clicks switch surfaces from ANY overlay; clicking the
    /// active tab returns to MAIN; text-entry modes keep the mouse out.
    #[test]
    fn tab_click_switches_overlay_and_active_tab_toggles_back() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut app = remote_app();
        app.hits.push(
            ratatui::layout::Rect {
                x: 1,
                y: 2,
                width: 9,
                height: 1,
            },
            ui::ClickTarget::Tab(Overlay::None),
        );
        app.hits.push(
            ratatui::layout::Rect {
                x: 13,
                y: 2,
                width: 8,
                height: 1,
            },
            ui::ClickTarget::Tab(Overlay::Config),
        );
        let click = |x: u16, y: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        // Click config tab from MAIN → Config opens.
        assert!(app.on_mouse(click(14, 2), None));
        assert_eq!(app.overlay, Overlay::Config);
        // Tab clicks still work WITH an overlay open: click dashboard → MAIN.
        assert!(app.on_mouse(click(2, 2), None));
        assert_eq!(app.overlay, Overlay::None);
        // Clicking the active tab toggles back to MAIN too.
        app.overlay = Overlay::Config;
        assert!(app.on_mouse(click(14, 2), None));
        assert_eq!(app.overlay, Overlay::None);
        // A pending text-entry mode keeps the mouse out entirely.
        app.mode = Mode::AddKey;
        assert!(!app.on_mouse(click(14, 2), None));
        assert_eq!(app.overlay, Overlay::None);
    }

    /// UI-3 U7/U8: pressing a separator row arms a drag; dragging resizes the
    /// pane above it (clamped); release disarms.
    #[test]
    fn separator_drag_resizes_the_pane_above() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut app = remote_app();
        app.hits.push(
            ratatui::layout::Rect::new(0, 10, 80, 1),
            ui::ClickTarget::Separator(ui::SeparatorHit {
                y: 10,
                pane: ui::PaneId::Accounts,
                pane_top: 3,
            }),
        );
        let ev = |kind, row| MouseEvent {
            kind,
            column: 5,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Press on the separator row arms the drag.
        assert!(app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), 10), None));
        assert!(app.drag.is_some());
        // Dragging to row 15 → accounts height 15 - 3 = 12.
        assert!(app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), 15), None));
        assert_eq!(app.pane_heights.accounts, Some(12));
        // Clamped at the minimum: dragging above the pane top.
        assert!(app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), 2), None));
        assert_eq!(app.pane_heights.accounts, Some(ui::PANE_MIN_HEIGHT));
        // Release disarms; further drags do nothing.
        app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), 2), None);
        assert!(app.drag.is_none());
        assert!(!app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), 20), None));
        assert_eq!(app.pane_heights.accounts, Some(ui::PANE_MIN_HEIGHT));
    }

    /// UI-3 U11: right-click on an accounts row opens the context menu; the
    /// items drive the SAME flows as the keys (pause queues the POST, set
    /// limit opens the editor, delete opens the y/N confirm); Esc closes.
    #[test]
    fn right_click_menu_runs_the_account_flows() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let view = stats_view_with_account();
        let row_rect = ratatui::layout::Rect {
            x: 0,
            y: 5,
            width: 80,
            height: 1,
        };
        let rclick = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };

        // pause (item 1): queues the remote pause POST.
        let mut app = remote_app();
        app.hits.push(row_rect, ui::ClickTarget::AccountRow(0));
        assert!(app.on_mouse(rclick, Some(&view)));
        assert_eq!(app.mode, Mode::ContextMenu { idx: 0, item: 0 });
        assert!(app.menu_anchor.is_some());
        app.on_key(press(KeyCode::Down), Some(&view));
        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(app.mode, Mode::Normal, "menu closed after running");
        assert_eq!(
            app.take_pending_pause(),
            Some(("claude:me@example.com".into(), true))
        );

        // set limit (item 2): opens the limits editor; Esc exits to Normal
        // (not into the switcher) because the menu opened it.
        let mut app = remote_app();
        app.hits.push(row_rect, ui::ClickTarget::AccountRow(0));
        assert!(app.on_mouse(rclick, Some(&view)));
        app.on_key(press(KeyCode::Down), Some(&view));
        app.on_key(press(KeyCode::Down), Some(&view));
        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(app.mode, Mode::EditLimits { idx: 0 });
        app.on_key(press(KeyCode::Esc), Some(&view));
        assert_eq!(app.mode, Mode::Normal);

        // delete (item 3): opens the destructive confirm, never silent.
        let mut app = remote_app();
        app.hits.push(row_rect, ui::ClickTarget::AccountRow(0));
        assert!(app.on_mouse(rclick, Some(&view)));
        // A key that is no item's own (`x`) closes; End moves, it does not.
        app.on_key(press(KeyCode::Char('x')), Some(&view));
        assert_eq!(app.mode, Mode::Normal, "unknown key dismisses");
        assert!(app.on_mouse(rclick, Some(&view)));
        for _ in 0..3 {
            app.on_key(press(KeyCode::Down), Some(&view));
        }
        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(app.mode, Mode::ConfirmRemove { idx: 0 });

        // Click outside the open menu dismisses it.
        let mut app = remote_app();
        app.hits.push(row_rect, ui::ClickTarget::AccountRow(0));
        assert!(app.on_mouse(rclick, Some(&view)));
        app.menu_chrome = Some(twolab_tui::menu::MenuLayout {
            area: ratatui::layout::Rect {
                x: 10,
                y: 6,
                width: 14,
                height: 5,
            },
            ..Default::default()
        });
        let lclick_outside = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 70,
            row: 20,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.on_mouse(lclick_outside, Some(&view)));
        assert_eq!(app.mode, Mode::Normal);
    }

    /// Catalog `menu` (plan batch 3): an item's own key runs it (`p`, `L`,
    /// `r` — the switcher's keys), ↑↓ and Home/End skip what cannot run
    /// (`switch now` on a paused account), and a click on a dim item or the
    /// border keeps the menu open instead of dismissing it.
    #[test]
    fn context_menu_item_keys_run_and_dim_items_are_skipped() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut view = stats_view_with_account();
        view.snapshot.accounts[0].paused = true;
        let open = |app: &mut App| {
            app.hits.push(
                ratatui::layout::Rect {
                    x: 0,
                    y: 5,
                    width: 80,
                    height: 1,
                },
                ui::ClickTarget::AccountRow(0),
            );
            let rclick = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 10,
                row: 5,
                modifiers: KeyModifiers::NONE,
            };
            assert!(app.on_mouse(rclick, Some(&view)));
        };

        // `switch now` is dim on a paused account: ↑ and Home stay on
        // `resume`, End reaches `delete`, Enter on `resume` resumes.
        let mut app = remote_app();
        open(&mut app);
        app.on_key(press(KeyCode::Up), Some(&view));
        assert_eq!(app.mode, Mode::ContextMenu { idx: 0, item: 1 });
        app.on_key(press(KeyCode::Home), Some(&view));
        assert_eq!(app.mode, Mode::ContextMenu { idx: 0, item: 1 });
        app.on_key(press(KeyCode::End), Some(&view));
        assert_eq!(app.mode, Mode::ContextMenu { idx: 0, item: 3 });
        app.on_key(press(KeyCode::Up), Some(&view));
        app.on_key(press(KeyCode::Up), Some(&view));
        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(
            app.take_pending_pause(),
            Some(("claude:me@example.com".into(), false)),
            "resume ran"
        );
        assert!(app.take_pending_control().is_none(), "no switch queued");

        // Item keys: `p` resumes, `L` opens the limits editor, `r` opens
        // the delete gate — never a silent delete.
        let mut app = remote_app();
        open(&mut app);
        app.on_key(press(KeyCode::Char('p')), Some(&view));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(
            app.take_pending_pause(),
            Some(("claude:me@example.com".into(), false))
        );
        open(&mut app);
        app.on_key(press(KeyCode::Char('L')), Some(&view));
        assert_eq!(app.mode, Mode::EditLimits { idx: 0 });
        app.on_key(press(KeyCode::Esc), Some(&view));
        open(&mut app);
        app.on_key(press(KeyCode::Char('r')), Some(&view));
        assert_eq!(app.mode, Mode::ConfirmRemove { idx: 0 });

        // A click inside the menu that hits no runnable item keeps it open;
        // a click on an item rect runs that item.
        let mut app = remote_app();
        open(&mut app);
        let rect = |y| ratatui::layout::Rect {
            x: 10,
            y,
            width: 14,
            height: 1,
        };
        app.menu_chrome = Some(twolab_tui::menu::MenuLayout {
            area: ratatui::layout::Rect {
                x: 9,
                y: 6,
                width: 16,
                height: 6,
            },
            items: (1..4)
                .map(|index| twolab_tui::menu::MenuHit {
                    rect: rect(7 + index as u16),
                    index,
                })
                .collect(),
            ..Default::default()
        });
        let lclick = |row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.on_mouse(lclick(7), Some(&view)), "dim switch row");
        assert_eq!(app.mode, Mode::ContextMenu { idx: 0, item: 0 });
        assert!(app.take_pending_control().is_none());
        assert!(app.on_mouse(lclick(10), Some(&view)), "delete row");
        assert_eq!(app.mode, Mode::ConfirmRemove { idx: 0 });
    }

    /// req8.1: `m` rotates the codex quick-pick set, and `gpt-6-astra` is
    /// reachable from the shipped pin — the dashboard IS the model switch, so
    /// a model llmux can route but cannot be cycled to is not switchable.
    #[test]
    fn codex_model_cycle_reaches_gpt_6_astra_from_the_sol_pin() {
        // Newest generation first (gpt-6.1-sol, the openai/codex catalog's
        // priority-1 default since 2026-09-29), and every entry is distinct (a
        // duplicate would make the modulo cycle skip a model forever).
        assert_eq!(CODEX_MODELS[0], "gpt-6.1-sol");
        assert_eq!(CODEX_MODELS.len(), 9);
        let mut sorted = CODEX_MODELS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), CODEX_MODELS.len(), "no duplicate quick-pick");

        let mut view = stats_view_with_account();
        view.codex.available = true;
        view.codex.model = "gpt-5.6-sol".into();
        let mut app = remote_app();
        // Walk the whole loop from the current pin; astra must appear within
        // one full rotation, and the walk must return to where it started.
        let mut seen = Vec::new();
        for _ in 0..CODEX_MODELS.len() {
            app.cycle_codex_model(Some(&view));
            let next = app
                .take_pending_codex()
                .expect("model change queued for the control endpoint")
                .model;
            view.codex.model = next.clone();
            seen.push(next);
        }
        assert!(
            seen.contains(&"gpt-6-astra".to_string()),
            "astra reachable by cycling: {seen:?}"
        );
        assert_eq!(
            seen.last().map(String::as_str),
            Some("gpt-5.6-sol"),
            "one full rotation returns to the starting pin: {seen:?}"
        );
        // An out-of-set pin (config / control endpoint) lands on the first
        // entry, i.e. the newest generation.
        view.codex.model = "gpt-5-codex-preview".into();
        app.cycle_codex_model(Some(&view));
        assert_eq!(
            app.take_pending_codex().map(|c| c.model),
            Some("gpt-6.1-sol".to_string())
        );
    }

    /// UI-3 U9/U10/U12: clicking a group-settings segment rotates that
    /// setting; the codex effort cycle now includes `max`; the grok effort
    /// cycle queues the `POST /llmux/grok` change with bypass in the loop.
    #[test]
    fn settings_bar_click_rotates_settings() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut view = stats_view_with_account();
        view.grok.available = true;
        view.grok.effort = None; // bypass
        view.codex.available = true;
        view.codex.model = "gpt-5.6-sol".into();
        view.codex.effort = Some("xhigh".into());
        let mut app = remote_app();
        app.hits.push(
            ratatui::layout::Rect {
                x: 0,
                y: 30,
                width: 10,
                height: 1,
            },
            ui::ClickTarget::Setting(ui::SettingAction::CodexEffort),
        );
        app.hits.push(
            ratatui::layout::Rect {
                x: 20,
                y: 30,
                width: 6,
                height: 1,
            },
            ui::ClickTarget::Setting(ui::SettingAction::GrokEffort),
        );
        let click = |x: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: 30,
            modifiers: KeyModifiers::NONE,
        };
        // codex effort: xhigh → max (the new top of the cycle).
        assert!(app.on_mouse(click(3), Some(&view)));
        assert_eq!(
            app.take_pending_codex().map(|c| c.effort),
            Some(Some("max".to_string())),
            "xhigh cycles to max"
        );
        // grok effort: bypass → none (first concrete value), queued for the
        // grok endpoint.
        assert!(app.on_mouse(click(22), Some(&view)));
        assert_eq!(app.take_pending_grok(), Some(Some("none".to_string())));
        // grok effort: high is no longer the top — `xhigh` sits above it
        // (grok-4.6 takes it on the wire; grok-4.5 clamps at request time).
        view.grok.effort = Some("high".into());
        assert!(app.on_mouse(click(22), Some(&view)));
        assert_eq!(
            app.take_pending_grok(),
            Some(Some("xhigh".to_string())),
            "high cycles to xhigh"
        );
        // Cycling from the top of the grok list wraps back to bypass.
        view.grok.effort = Some("xhigh".into());
        assert!(app.on_mouse(click(22), Some(&view)));
        assert_eq!(app.take_pending_grok(), Some(None), "xhigh wraps to bypass");
    }

    /// Review R2 regression: rows reorder between menu-open and click — the
    /// action must land on the PINNED account, and a vanished pin aborts.
    #[test]
    fn menu_action_follows_pinned_account_across_reorder() {
        use crate::routing::BackendGroup;
        use crate::scheduler::{AccountId, AccountSnapshot};
        use crossterm::event::{MouseEvent, MouseEventKind};
        let acct = |name: &str| AccountSnapshot {
            id: AccountId(name.into()),
            healthy: true,
            auth_failure_status: None,
            credential_kind: "oauth",
            group: BackendGroup::Claude,
            five_hour: None,
            seven_day: None,
            scoped_limits: Vec::new(),
            scoped_cooldowns: Vec::new(),
            cooldown_until: None,
            cooldown_source: None,
            in_flight: 0,
            token_expires_at_ms: None,
            last_refresh_ms: None,
            paused: false,
            limits: crate::config::AccountLimits::default(),
        };
        let mut view = stats_view_with_account();
        view.snapshot.accounts = vec![acct("claude:a@x.com"), acct("claude:b@x.com")];
        let mut app = remote_app();
        app.hits.push(
            ratatui::layout::Rect {
                x: 0,
                y: 5,
                width: 80,
                height: 1,
            },
            ui::ClickTarget::AccountRow(0),
        );
        let rclick = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.on_mouse(rclick, Some(&view)));
        assert_eq!(app.menu_account.as_deref(), Some("claude:a@x.com"));

        // Reorder: in `next` order a state change still moves rows — `a` goes
        // auth-broken and sinks below `b`, so display row 0 is now `b`.
        // Running pause (item 1) must still act on `a`.
        app.account_sort = triage::AccountSort::Next;
        view.snapshot.accounts[0].healthy = false;
        let order = view.display_order(app.account_sort, SystemTime::now());
        assert_eq!(
            view.snapshot.accounts[order[0]].id.0, "claude:b@x.com",
            "the state change really reordered the table"
        );
        app.on_key(press(KeyCode::Down), Some(&view));
        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(
            app.take_pending_pause(),
            Some(("claude:a@x.com".into(), true)),
            "action follows the pinned account across the reorder"
        );

        // A pinned account that VANISHED aborts with a status, acts on no one.
        assert!(app.on_mouse(rclick, Some(&view)));
        app.menu_account = Some("claude:gone@x.com".into());
        app.on_key(press(KeyCode::Down), Some(&view));
        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(
            app.take_pending_pause(),
            None,
            "vanished pin acts on no one"
        );
        assert!(app.status_line().is_some_and(|s| s.contains("gone")));
    }

    /// Two accounts, so a click on display row 1 has a real id to resolve to.
    fn two_account_view() -> DashboardView {
        use crate::routing::BackendGroup;
        use crate::scheduler::{AccountId, AccountSnapshot};
        let acct = |name: &str| AccountSnapshot {
            id: AccountId(name.into()),
            healthy: true,
            auth_failure_status: None,
            credential_kind: "oauth",
            group: BackendGroup::Claude,
            five_hour: None,
            seven_day: None,
            scoped_limits: Vec::new(),
            scoped_cooldowns: Vec::new(),
            cooldown_until: None,
            cooldown_source: None,
            in_flight: 0,
            token_expires_at_ms: None,
            last_refresh_ms: None,
            paused: false,
            limits: crate::config::AccountLimits::default(),
        };
        let mut view = stats_view_with_account();
        view.snapshot.accounts = vec![acct("claude:a@x.com"), acct("claude:b@x.com")];
        view
    }

    /// .prd/19 rule 6: a left-click on an accounts row opens the detail modal
    /// pinned to the REAL account id at that display position (never the index
    /// — the table reorders), on MAIN *and* on the accounts overlay.
    #[test]
    fn account_click_opens_detail_modal_pinned_by_id() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let view = two_account_view();
        // Display row 1 under the active sort — asserted, not assumed.
        let order = view.display_order(triage::AccountSort::default(), SystemTime::now());
        let wanted = view.snapshot.accounts[order[1]].id.0.clone();
        assert_eq!(wanted, "claude:b@x.com");
        let lclick = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        for overlay in [Overlay::None, Overlay::Accounts] {
            let mut app = remote_app();
            app.overlay = overlay;
            app.hits.push(
                ratatui::layout::Rect {
                    x: 0,
                    y: 6,
                    width: 80,
                    height: 1,
                },
                ui::ClickTarget::AccountRow(1),
            );
            assert!(app.on_mouse(lclick, Some(&view)), "{overlay:?}");
            assert_eq!(
                app.account_modal.as_ref().map(|m| m.account.as_str()),
                Some(wanted.as_str()),
                "{overlay:?}: modal pinned to the real id"
            );
            assert_eq!(app.account_modal.as_ref().map(|m| m.state.scroll), Some(0));
            assert_eq!(app.mode, Mode::Normal, "{overlay:?}: no row cursor moved");
            assert_eq!(app.menu_account, None, "{overlay:?}: no context menu");
        }
    }

    /// A left-click on a models-strip entry (its rect from the last frame,
    /// one per entry of twolab-tui's packed grid) opens the Stats tab with
    /// that model under the cursor; a click beside every entry does not.
    #[test]
    fn strip_click_opens_the_model_in_stats() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut view = stats_view();
        let mut second = view.model_usage[0].clone();
        second.model = "gpt-5.6-sol".into();
        view.model_usage.push(second);
        let click = |column| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: 30,
            modifiers: KeyModifiers::NONE,
        };
        let entry = |x, index| {
            (
                ratatui::layout::Rect::new(x, 30, 70, 1),
                ui::ClickTarget::StripEntry(index),
            )
        };
        let mut app = remote_app();
        app.hits.extend([entry(1, 0), entry(74, 1)]);
        assert!(
            !app.on_mouse(click(72), Some(&view)),
            "the gap between entries"
        );
        assert_eq!(app.overlay, Overlay::None);
        assert!(app.on_mouse(click(80), Some(&view)));
        assert_eq!(app.overlay, Overlay::Stats);
        assert_eq!(app.model_cursor, 1, "the clicked model");
    }

    /// Review of #234: the mouse wiring of the twolab-tui modal — a click on
    /// `esc close` closes the account modal, a click on the scrollbar track
    /// jumps the body there, and a click outside the popup is swallowed
    /// without closing it.
    #[test]
    fn account_modal_close_button_and_scrollbar_take_clicks() {
        use crossterm::event::MouseEventKind;
        let view = two_account_view();
        let rect = |x, y, width, height| ratatui::layout::Rect {
            x,
            y,
            width,
            height,
        };
        let open = |app: &mut App| {
            let mut modal = AccountModal::new("claude:a@x.com".into());
            // What a frame drew: 60 body rows through a 20-row view, a
            // 20-row track at column 70 and `esc close` on the top border.
            modal.state.layout.outer = rect(10, 2, 62, 23);
            modal.state.layout.pos = ScrollPos::of(60, 20, 0);
            modal.state.layout.track = Some(rect(70, 4, 1, 20));
            modal.state.layout.close = Some(rect(60, 2, 9, 1));
            app.account_modal = Some(modal);
        };
        let click = |col, row| mouse(MouseEventKind::Down(MouseButton::Left), col, row);

        let mut app = remote_app();
        open(&mut app);
        assert!(
            app.on_mouse(click(1, 1), Some(&view)),
            "an outside click is swallowed"
        );
        assert!(app.account_modal.is_some(), "and does not close the modal");

        assert!(app.on_mouse(click(70, 23), Some(&view)));
        let scrolled = app.account_modal.as_ref().unwrap().state.scroll;
        assert!(
            scrolled > 0,
            "a click low on the track scrolls down ({scrolled})"
        );

        assert!(app.on_mouse(click(62, 2), Some(&view)));
        assert!(app.account_modal.is_none(), "`esc close` closes the modal");
    }

    /// .prd/19 rule 6: while the modal is open every key and click beneath it
    /// is swallowed — `q` closes the modal instead of quitting, a row click
    /// neither re-pins nor opens a menu, and Esc closes.
    #[test]
    fn account_modal_swallows_keys_and_clicks() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let view = two_account_view();
        let open = |app: &mut App| {
            let mut modal = AccountModal::new("claude:a@x.com".into());
            // What a frame drew: 60 body rows through a 20-row view.
            modal.state.layout.pos = ScrollPos::of(60, 20, 0);
            app.account_modal = Some(modal);
        };
        let mut app = remote_app();
        app.hits.push(
            ratatui::layout::Rect {
                x: 0,
                y: 6,
                width: 80,
                height: 1,
            },
            ui::ClickTarget::AccountRow(1),
        );
        open(&mut app);
        // `q` never reaches the quit handler beneath the modal.
        app.on_key(press(KeyCode::Char('q')), Some(&view));
        assert!(!app.should_quit, "q does not quit while the modal is open");
        assert!(app.account_modal.is_none(), "q closes the modal itself");

        open(&mut app);
        // An unbound key is swallowed: MAIN's activity scroll must not move.
        let before = app.activity_scroll;
        app.on_key(press(KeyCode::Char('x')), Some(&view));
        assert_eq!(app.activity_scroll, before, "keys don't leak beneath");
        assert!(app.account_modal.is_some());
        // Arrows scroll the modal, PgDn pages it by its own height.
        app.on_key(press(KeyCode::Down), Some(&view));
        assert_eq!(app.account_modal.as_ref().unwrap().state.scroll, 1);
        app.on_key(press(KeyCode::PageDown), Some(&view));
        assert_eq!(app.account_modal.as_ref().unwrap().state.scroll, 1 + 20);

        // A left-click on a row is consumed: the pin does not move, no menu
        // opens, the mode stays Normal.
        let lclick = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        assert!(
            app.on_mouse(lclick, Some(&view)),
            "the modal eats the click"
        );
        assert_eq!(
            app.account_modal.as_ref().map(|m| m.account.as_str()),
            Some("claude:a@x.com"),
            "the click did not re-pin the modal"
        );
        assert_eq!(app.menu_account, None);
        assert_eq!(app.mode, Mode::Normal);
        // The wheel scrolls it instead of the activity panel.
        let before = app.account_modal.as_ref().unwrap().state.scroll;
        app.on_mouse(mouse(MouseEventKind::ScrollDown, 10, 6), Some(&view));
        assert_eq!(app.account_modal.as_ref().unwrap().state.scroll, before + 3);
        assert_eq!(app.activity_scroll, 0, "the wheel did not reach MAIN");

        app.on_key(press(KeyCode::Esc), Some(&view));
        assert!(app.account_modal.is_none(), "esc closes the modal");
    }

    /// `?`/`c` open the Misc/Config overlays from MAIN; Esc returns.
    #[test]
    fn session_sort_cycles_and_reorders() {
        let mut app = remote_app();
        let sess =
            |uid: &str, last_ms: u64, tokens_out: u64, requests: u64| crate::session::Session {
                duration_ms_sum: 0,
                timed_requests: 0,
                tokens_out_timed: 0,
                user_id: Some(uid.into()),
                requests,
                tokens_in: 0,
                tokens_out,
                models: Vec::new(),
                accounts: Vec::new(),
                account_rotations: 0,
                first_ms: 0,
                last_ms,
                confidence: crate::session::Confidence::High,
            };
        // recent order: b (newest) first; tokens order: c; requests order: a.
        app.sessions = vec![
            sess("a", 10, 5, 9),
            sess("b", 30, 1, 1),
            sess("c", 20, 99, 2),
        ];
        app.session_sort.apply(&mut app.sessions);
        assert_eq!(app.sessions[0].user_id.as_deref(), Some("b"), "recent");
        app.on_key_sessions(KeyCode::Char('o'));
        assert_eq!(app.session_sort, SessionSort::Tokens);
        assert_eq!(app.sessions[0].user_id.as_deref(), Some("c"), "tokens");
        assert_eq!(app.session_cursor, 0, "cursor resets with the order");
        app.on_key_sessions(KeyCode::Char('o'));
        assert_eq!(app.sessions[0].user_id.as_deref(), Some("a"), "requests");
        app.on_key_sessions(KeyCode::Char('o'));
        assert_eq!(app.session_sort, SessionSort::Recent, "cycle wraps");
    }

    /// `o` toggles the accounts row order on MAIN *and* on the accounts
    /// overlay (the one session toggle both surfaces share), names the new
    /// mode in the status line, and starts on `name`.
    #[test]
    fn sort_key_toggles_mode() {
        let view = control_view();
        let mut app = remote_app();
        assert_eq!(
            app.account_sort,
            triage::AccountSort::Name,
            "name order is the default"
        );
        app.on_key_main(KeyCode::Char('o'), Some(&view));
        assert_eq!(app.account_sort, triage::AccountSort::Next);
        assert!(
            app.status_line()
                .is_some_and(|s| s.contains("accounts sorted by next")),
            "{:?}",
            app.status_line()
        );
        app.on_key_main(KeyCode::Char('o'), Some(&view));
        assert_eq!(app.account_sort, triage::AccountSort::Name, "toggles back");
        assert!(app
            .status_line()
            .is_some_and(|s| s.contains("accounts sorted by name")));
        // Same key, same state, from the accounts overlay.
        app.overlay = Overlay::Accounts;
        app.on_key_accounts(KeyCode::Char('o'), Some(&view));
        assert_eq!(app.account_sort, triage::AccountSort::Next);
        app.on_key_accounts(KeyCode::Char('o'), Some(&view));
        assert_eq!(app.account_sort, triage::AccountSort::Name);
    }

    /// `z` on MAIN folds the 7d-spent accounts: off by default, flipped by
    /// the key with a status line naming the state, carried on every
    /// frame's chrome, and session-local like `u` — an overlay round trip
    /// and the other display toggles leave it alone.
    #[test]
    fn fold_key_toggles_on_main_and_persists_like_u() {
        let view = control_view();
        let mut app = remote_app();
        assert!(
            !app.fold_exhausted && !app.chrome().fold_exhausted,
            "off by default"
        );
        app.on_key(press(KeyCode::Char('z')), Some(&view));
        assert!(app.fold_exhausted && app.chrome().fold_exhausted);
        // Plain state, never a claim that something folded (the accounts
        // title says what the fold did this frame).
        assert_eq!(app.status_line(), Some("fold on"));
        app.on_key(press(KeyCode::Char('a')), Some(&view));
        assert_eq!(app.overlay, Overlay::Accounts);
        app.on_key(press(KeyCode::Esc), Some(&view));
        app.on_key(press(KeyCode::Char('u')), Some(&view));
        app.on_key(press(KeyCode::Char('t')), Some(&view));
        assert!(app.fold_exhausted && app.chrome().fold_exhausted, "kept");
        app.on_key(press(KeyCode::Char('z')), Some(&view));
        assert!(
            !app.fold_exhausted && !app.chrome().fold_exhausted,
            "toggles back"
        );
        assert_eq!(app.status_line(), Some("fold off"));
    }

    /// `v` on the accounts tab switches between the table and the compact
    /// account list: off by default, a status line naming the view, carried
    /// on the frame's chrome, kept across a MAIN round trip; on MAIN `V`
    /// does nothing (MAIN picks the list by itself when its table is
    /// crowded).
    #[test]
    fn compact_key_toggles_the_account_list_on_the_accounts_tab() {
        let view = control_view();
        let mut app = remote_app();
        assert!(!app.compact_accounts && !app.chrome().compact_accounts);
        app.on_key(press(KeyCode::Char('V')), Some(&view));
        assert!(!app.compact_accounts, "MAIN leaves it alone");
        app.on_key(press(KeyCode::Char('a')), Some(&view));
        assert_eq!(app.overlay, Overlay::Accounts);
        app.on_key(press(KeyCode::Char('V')), Some(&view));
        assert!(app.compact_accounts && app.chrome().compact_accounts);
        assert_eq!(app.status_line(), Some("accounts: compact list (V: table)"));
        app.on_key(press(KeyCode::Esc), Some(&view));
        app.on_key(press(KeyCode::Char('a')), Some(&view));
        assert!(app.chrome().compact_accounts, "kept");
        app.on_key(press(KeyCode::Char('V')), Some(&view));
        assert!(!app.compact_accounts && !app.chrome().compact_accounts);
        assert_eq!(app.status_line(), Some("accounts: table (V: compact list)"));
    }

    #[test]
    fn config_sched_mode_confirms_then_applies() {
        // Review MUST-FIX: the scheduler-mode row must reach the confirm
        // gate and then the existing live toggle — never "value unavailable".
        let mut app = remote_app();
        app.overlay = Overlay::Config;
        app.activate_config_action(ui::ConfigAction::SchedMode, None);
        assert!(
            matches!(
                app.mode,
                Mode::ConfigConfirm {
                    action: ui::ConfigAction::SchedMode
                }
            ),
            "SchedMode enters the confirm gate, got {:?}",
            app.mode
        );
        app.on_key_config_confirm(KeyCode::Char('y'), ui::ConfigAction::SchedMode, None);
        assert_eq!(app.mode, Mode::Normal);
        assert!(
            app.take_pending_mode().is_some(),
            "confirming queues the live scheduler-mode toggle (remote path)"
        );
    }

    #[test]
    fn config_ceiling_zero_requires_confirm() {
        // Blast-radius rule: driving a ceiling to 0 locks that window's
        // scheduling — it must pass the y/n gate, a normal value must not.
        let mut app = remote_app();
        app.overlay = Overlay::Config;
        app.mode = Mode::ConfigEdit {
            action: ui::ConfigAction::Ceiling5h,
        };
        app.field = ComposerState::with_text("0").single_line();
        app.on_key_config_edit(press(KeyCode::Enter), ui::ConfigAction::Ceiling5h, None);
        assert!(
            matches!(app.mode, Mode::ConfigConfirm { .. }),
            "ceiling→0 needs confirm"
        );
        app.on_key_config_confirm(KeyCode::Esc, ui::ConfigAction::Ceiling5h, None);
        app.mode = Mode::ConfigEdit {
            action: ui::ConfigAction::Ceiling5h,
        };
        app.field = ComposerState::with_text("80").single_line();
        app.on_key_config_edit(press(KeyCode::Enter), ui::ConfigAction::Ceiling5h, None);
        assert_eq!(app.mode, Mode::Normal, "a normal percent applies directly");
    }

    /// One-shot HTTP responder for the attach settings round-trip tests:
    /// binds an ephemeral loopback port, answers the first request with the
    /// canned status+body, then closes. Small enough to not need a mock crate.
    async fn one_shot_settings_daemon(status_line: &'static str, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    /// Drive [`App::perform_remote_settings`] against the canned daemon and
    /// return the resulting status line. The app is seeded with a pending
    /// quota override and a request that carries `quota_display` + a
    /// restart-only origin, so every mutation site is observable.
    async fn attach_settings_round_trip(
        status_line: &'static str,
        body: &'static str,
    ) -> (App, StatusMsg) {
        let mut app = remote_app();
        let base = one_shot_settings_daemon(status_line, body).await;
        if let Backend::Remote(remote) = &mut app.backend {
            remote.base_url = base;
        }
        app.quota_display_override = Some(crate::config::QuotaDisplay::Used);
        app.config_saved.insert(
            ui::ConfigAction::RawIoRetention,
            "stale-from-last-time".into(),
        );
        let req = crate::proxy::server::SettingsRequest {
            quota_display: Some("amount".into()),
            raw_io_retention_days: Some(7),
            ..Default::default()
        };
        let origin = Some((ui::ConfigAction::RawIoRetention, "7".into()));
        app.perform_remote_settings(req, origin).await;
        let status = app
            .status
            .as_ref()
            .map(|(msg, _)| msg.clone())
            .expect("the round trip reports a status");
        (app, status)
    }

    /// Attach HTTP round-trip, success branch: a verified typed ack applies —
    /// the quota override drops (the doc now carries the truth) and the
    /// restart-required origin lands in `config_saved`.
    #[tokio::test]
    async fn attach_settings_success_ack_applies_local_state() {
        let (app, status) = attach_settings_round_trip(
            "200 OK",
            r#"{"ok":true,"applied":["quota_display"],"restart_required":["raw_io_retention_days"],"email_anonymous":false}"#,
        )
        .await;
        assert!(
            app.quota_display_override.is_none(),
            "verified ack clears the local quota override"
        );
        assert_eq!(
            app.config_saved.get(&ui::ConfigAction::RawIoRetention),
            Some(&"7".to_string()),
            "restart-required origin is noted for the config tab"
        );
        assert!(
            status.text.contains("applied live") && status.text.contains("restart"),
            "status reports the honest applied/restart split: {status:?}"
        );
        // Saved but not live yet is a warning, never a plain success.
        assert_eq!(status.outcome, twolab_tui::banner::Outcome::Warn);
    }

    /// Attach round-trip, ok=false 2xx: NOTHING local moves.
    #[tokio::test]
    async fn attach_settings_ok_false_mutates_nothing() {
        let (app, status) = attach_settings_round_trip(
            "200 OK",
            r#"{"ok":false,"applied":[],"restart_required":[],"email_anonymous":false}"#,
        )
        .await;
        assert!(app.quota_display_override.is_some(), "override untouched");
        assert_eq!(
            app.config_saved.get(&ui::ConfigAction::RawIoRetention),
            Some(&"stale-from-last-time".to_string()),
            "pending-restart note untouched"
        );
        assert!(status.text.contains("ok=false"), "status: {status:?}");
        assert_eq!(status.outcome, twolab_tui::banner::Outcome::Fail);
    }

    /// Attach round-trip, malformed 2xx (`{}` fails the typed parse): the
    /// apply is UNVERIFIED — no local mutation, honest "nothing assumed".
    #[tokio::test]
    async fn attach_settings_malformed_ack_mutates_nothing() {
        let (app, status) = attach_settings_round_trip("200 OK", "{}").await;
        assert!(app.quota_display_override.is_some(), "override untouched");
        assert_eq!(
            app.config_saved.get(&ui::ConfigAction::RawIoRetention),
            Some(&"stale-from-last-time".to_string()),
            "pending-restart note untouched"
        );
        assert!(
            status.text.contains("nothing assumed"),
            "status: {status:?}"
        );
        assert_eq!(status.outcome, twolab_tui::banner::Outcome::Warn);
    }

    /// Attach round-trip, HTTP error statuses: reported, nothing mutated.
    #[tokio::test]
    async fn attach_settings_http_errors_mutate_nothing() {
        for status_line in ["400 Bad Request", "500 Internal Server Error"] {
            let (app, status) =
                attach_settings_round_trip(status_line, r#"{"error":"nope"}"#).await;
            assert!(app.quota_display_override.is_some(), "override untouched");
            assert_eq!(
                app.config_saved.get(&ui::ConfigAction::RawIoRetention),
                Some(&"stale-from-last-time".to_string()),
                "pending-restart note untouched"
            );
            assert!(
                status.text.contains("config change failed"),
                "status for {status_line}: {status:?}"
            );
            assert_eq!(status.outcome, twolab_tui::banner::Outcome::Fail);
        }
    }

    #[test]
    fn misc_and_config_keys_round_trip() {
        let mut app = remote_app();
        app.on_key_main(KeyCode::Char('?'), None);
        assert_eq!(app.overlay, Overlay::Misc);
        app.on_key_misc(KeyCode::Esc);
        assert_eq!(app.overlay, Overlay::None);
        app.on_key_main(KeyCode::Char('c'), None);
        assert_eq!(app.overlay, Overlay::Config);
        app.on_key_config(KeyCode::Char('c'), None);
        assert_eq!(app.overlay, Overlay::None);
    }

    /// `a` opens the Accounts overlay; `Esc` returns to MAIN.
    #[test]
    fn a_opens_accounts_overlay_and_esc_returns_to_main() {
        let mut app = remote_app();
        assert_eq!(app.overlay, Overlay::None);
        app.on_key_main(KeyCode::Char('a'), None);
        assert_eq!(app.overlay, Overlay::Accounts);
        app.on_key_accounts(KeyCode::Esc, None);
        assert_eq!(app.overlay, Overlay::None);
    }

    /// `l` opens the Logs overlay; `l`/`Esc` close it.
    #[test]
    fn l_opens_logs_overlay_and_esc_returns_to_main() {
        let mut app = remote_app();
        app.on_key_main(KeyCode::Char('l'), None);
        assert_eq!(app.overlay, Overlay::Logs);
        app.on_key_logs(KeyCode::Esc);
        assert_eq!(app.overlay, Overlay::None);
        // `l` toggles back too.
        app.on_key_main(KeyCode::Char('l'), None);
        assert_eq!(app.overlay, Overlay::Logs);
        app.on_key_logs(KeyCode::Char('l'));
        assert_eq!(app.overlay, Overlay::None);
    }

    /// The Logs pager follows the tail until a key scrolls it back; Home goes
    /// to the oldest line, End (or scrolling down to the bottom) follows again,
    /// the wheel moves three rows, and reopening starts at the tail.
    #[test]
    fn logs_pager_scrolls_back_and_end_follows_the_tail_again() {
        let mut app = remote_app();
        app.on_key_main(KeyCode::Char('l'), None);
        assert_eq!(app.logs_scroll, None, "opens following the tail");
        // What the last frame drew: 120 rows through a 20-row view.
        app.logs_pos = ScrollPos::of(120, 20, 100);
        app.on_key_logs(KeyCode::Up);
        assert_eq!(app.logs_scroll, Some(99), "↑ leaves the tail");
        app.on_key_logs(KeyCode::PageUp);
        assert_eq!(app.logs_scroll, Some(79), "PgUp moves a page");
        app.on_key_logs(KeyCode::Home);
        assert_eq!(app.logs_scroll, Some(0), "Home: the oldest line");
        app.on_key_logs(KeyCode::Char('j'));
        assert_eq!(app.logs_scroll, Some(1), "j is ↓");
        app.on_key_logs(KeyCode::End);
        assert_eq!(app.logs_scroll, None, "End follows again");
        app.on_mouse(mouse(MouseEventKind::ScrollUp, 10, 10), None);
        assert_eq!(app.logs_scroll, Some(97), "wheel up three rows");
        app.on_mouse(mouse(MouseEventKind::ScrollDown, 10, 10), None);
        assert_eq!(app.logs_scroll, None, "back at the bottom: following");
        app.on_key_logs(KeyCode::Home);
        app.on_key_logs(KeyCode::Esc);
        assert_eq!(app.overlay, Overlay::None);
        app.on_key_main(KeyCode::Char('l'), None);
        assert_eq!(app.logs_scroll, None, "reopened at the tail");
    }

    /// Copy mode (`v`): MAIN or an overlay hands the mouse to the terminal;
    /// every key but `v` / Esc is swallowed (nothing quits, opens or
    /// scrolls under the selection), `v` or Esc takes the mouse back where
    /// it was, a field being typed in gets the `v` instead, and Ctrl-C
    /// still quits.
    #[test]
    fn v_releases_the_mouse_into_copy_mode_and_v_or_esc_take_it_back() {
        let mut app = remote_app();
        app.on_key(press(KeyCode::Char('v')), None);
        assert!(app.copy_mode && app.chrome().copy_mode, "the banner draws");
        for code in [
            KeyCode::Char('q'),
            KeyCode::Char('a'),
            KeyCode::Char('l'),
            KeyCode::Up,
        ] {
            app.on_key(press(code), None);
        }
        assert!(app.copy_mode, "still in copy mode");
        assert!(!app.should_quit, "q swallowed");
        assert_eq!(app.overlay, Overlay::None, "a / l swallowed");
        app.on_key(press(KeyCode::Char('v')), None);
        assert!(!app.copy_mode, "v takes the mouse back");

        app.on_key_main(KeyCode::Char('l'), None);
        app.on_key(press(KeyCode::Char('v')), None);
        assert!(app.copy_mode, "from an overlay too");
        app.on_key(press(KeyCode::Esc), None);
        assert!(!app.copy_mode, "Esc takes the mouse back");
        assert_eq!(
            app.overlay,
            Overlay::Logs,
            "Esc left copy mode, not the overlay"
        );

        app.overlay = Overlay::Accounts;
        app.mode = Mode::AddKey;
        app.on_key(press(KeyCode::Char('v')), None);
        assert!(!app.copy_mode, "a typed v is text");
        assert_eq!(app.field.text(), "v");

        app.mode = Mode::Normal;
        app.on_key(press(KeyCode::Char('v')), None);
        app.on_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            None,
        );
        assert!(app.should_quit, "Ctrl-C quits from copy mode");
    }

    /// Copy mode holds the frame still while it stays on: the render tick
    /// and background deliveries draw nothing, a key it swallows asks for no
    /// frame, a resize still draws, and entering and leaving draw.
    #[test]
    fn copy_mode_holds_the_frame_still_until_it_is_left() {
        assert!(
            frame_due(true, false, false, false),
            "normally the tick draws"
        );
        assert!(
            frame_due(true, false, true, true),
            "entering draws the banner"
        );
        assert!(!frame_due(true, true, true, false), "the tick waits");
        assert!(frame_due(true, true, true, true), "a resize draws");
        assert!(frame_due(true, true, false, true), "leaving draws");
        assert!(!frame_due(false, false, false, true), "nothing asked");

        let mut app = remote_app();
        app.copy_mode = true;
        let (mut view, mut dirty) = (Some(None), false);
        apply_event(
            &mut app,
            Event::Key(press(KeyCode::Down)),
            &mut view,
            &mut dirty,
        );
        assert!(!dirty, "a swallowed key asks for no frame");
        apply_event(&mut app, Event::Resize(80, 24), &mut view, &mut dirty);
        assert!(dirty, "a resize does");
        dirty = false;
        apply_event(
            &mut app,
            Event::Key(press(KeyCode::Esc)),
            &mut view,
            &mut dirty,
        );
        assert!(dirty && !app.copy_mode, "leaving does");
    }

    /// A drag across the Logs pager (80x24) selects from the press to the
    /// release with twolab-tui's Pager selection; the release queues exactly
    /// the selected text for the clipboard, the view holds still for it, the
    /// outcome lands on the status line, and a plain click clears it
    /// without copying anything.
    #[test]
    fn logs_drag_selects_and_the_release_copies_the_selected_text() {
        let mut view = stats_view_with_account();
        view.logs = (0..5)
            .map(|i| crate::logging::LogLine {
                level: tracing::Level::INFO,
                text: format!("log line {i:03}"),
            })
            .collect();
        let mut app = remote_app();
        app.on_key_main(KeyCode::Char('l'), None);
        let rows = draw_and_adopt(&mut app, &view, 80, 24);
        // Content rows from 3: `  INFO log line 000`, the text from col 7.
        assert_eq!(rows[3].trim_end(), "  INFO log line 000", "{rows:#?}");
        assert!(app.on_mouse(
            left(MouseEventKind::Down(MouseButton::Left), 7, 3),
            Some(&view)
        ));
        assert!(app.on_mouse(
            left(MouseEventKind::Drag(MouseButton::Left), 18, 4),
            Some(&view)
        ));
        assert!(
            app.logs_sel.is_some_and(|s| s.is_visible()),
            "dragging shows it"
        );
        assert_eq!(app.logs_scroll, Some(0), "held for the selection");
        app.on_mouse(
            left(MouseEventKind::Up(MouseButton::Left), 18, 4),
            Some(&view),
        );
        let req = app.pending_clip.pop_front().expect("the release copies");
        assert_eq!(req.payload, "log line 000\n  INFO log line 001");
        assert_eq!(req.origin, ClipOrigin::Logs { lines: 2 });
        let rows = draw_and_adopt(&mut app, &view, 80, 24);
        assert!(
            rows[2].contains("logs · 5 lines · held for the selection — End follows"),
            "{rows:#?}"
        );
        app.apply_clip_result(ClipResult {
            origin: ClipOrigin::Logs { lines: 2 },
            generation: 0,
            message: StatusMsg::ok("copied 2 lines → OSC52"),
        });
        assert_eq!(app.status_line(), Some("copied 2 lines → OSC52"));

        app.on_mouse(
            left(MouseEventKind::Down(MouseButton::Left), 10, 6),
            Some(&view),
        );
        app.on_mouse(
            left(MouseEventKind::Up(MouseButton::Left), 10, 6),
            Some(&view),
        );
        assert_eq!(app.logs_sel, None, "a plain click clears the selection");
        assert!(app.pending_clip.is_empty(), "and copies nothing");
    }

    /// A drag past the bottom of the Logs pager scrolls it (3 rows per row
    /// of distance) and the selection follows; closing the overlay forgets
    /// the selection.
    #[test]
    fn logs_drag_past_the_edge_scrolls_and_closing_forgets_the_selection() {
        let mut view = stats_view_with_account();
        view.logs = (0..100)
            .map(|i| crate::logging::LogLine {
                level: tracing::Level::INFO,
                text: format!("log line {i:03}"),
            })
            .collect();
        let mut app = remote_app();
        app.on_key_main(KeyCode::Char('l'), None);
        draw_and_adopt(&mut app, &view, 80, 24);
        app.on_key_logs(KeyCode::Home);
        draw_and_adopt(&mut app, &view, 80, 24);
        app.on_mouse(
            left(MouseEventKind::Down(MouseButton::Left), 7, 3),
            Some(&view),
        );
        // Content rows 3..=20; row 22 is two rows below the last one.
        app.on_mouse(
            left(MouseEventKind::Drag(MouseButton::Left), 7, 22),
            Some(&view),
        );
        assert_eq!(app.logs_scroll, Some(6), "two rows past the edge: six rows");
        app.on_mouse(
            left(MouseEventKind::Up(MouseButton::Left), 7, 20),
            Some(&view),
        );
        let req = app.pending_clip.pop_front().expect("copied");
        assert!(req.payload.starts_with("log line 000\n"), "{}", req.payload);
        assert!(
            req.payload.ends_with("  INFO l"),
            "to the pointer: {}",
            req.payload
        );
        assert_eq!(req.origin, ClipOrigin::Logs { lines: 24 });
        app.on_key_logs(KeyCode::Esc);
        assert_eq!(app.logs_sel, None, "closing forgets it");
    }

    /// `s` opens the Sessions overlay (issue #34); arrows move the cursor within
    /// the loaded session list and `s`/`Esc` close back to MAIN. The session list
    /// is injected directly (the real loader reads a file off disk, not under
    /// test here — `fold_sessions` is unit-tested in `crate::session`).
    #[test]
    fn s_opens_sessions_overlay_navigates_and_esc_returns_to_main() {
        use crate::session::{Confidence, Session};
        let session = |uid: &str| Session {
            user_id: Some(uid.into()),
            requests: 1,
            tokens_in: 0,
            tokens_out: 0,
            models: vec![],
            accounts: vec![],
            account_rotations: 0,
            first_ms: 0,
            last_ms: 0,
            duration_ms_sum: 0,
            timed_requests: 0,
            tokens_out_timed: 0,
            confidence: Confidence::High,
        };
        let mut app = remote_app();
        app.sessions = vec![session("u-1"), session("u-2"), session("u-3")];
        app.overlay = Overlay::Sessions;
        assert_eq!(app.session_cursor, 0);

        // Down/up move within bounds.
        app.on_key_sessions(KeyCode::Down);
        assert_eq!(app.session_cursor, 1);
        app.on_key_sessions(KeyCode::Up);
        assert_eq!(app.session_cursor, 0);
        // Up clamps at the top.
        app.on_key_sessions(KeyCode::Up);
        assert_eq!(app.session_cursor, 0);
        // End jumps to the last row; Down clamps there.
        app.on_key_sessions(KeyCode::End);
        assert_eq!(app.session_cursor, 2);
        app.on_key_sessions(KeyCode::Down);
        assert_eq!(app.session_cursor, 2);

        // s/Esc close back to MAIN.
        app.on_key_sessions(KeyCode::Char('s'));
        assert_eq!(app.overlay, Overlay::None);
    }

    /// Usage-overlay scroll clamps the STORED offset at the last bucket
    /// (review R1 MUST-FIX 2): pressing `j` at the bottom must not bank
    /// invisible overscroll debt that later `k` presses have to pay off.
    #[test]
    fn usage_scroll_clamps_stored_offset_no_overscroll_debt() {
        let row = |bucket: u64| crate::dashboard::UsageStatDoc {
            gran: "day".into(),
            bucket,
            label: format!("day-{bucket}"),
            group: "claude".into(),
            model: "m".into(),
            requests: 1,
            tokens_in: 1,
            tokens_out: 1,
            cache_read: 0,
            cache_creation: 0,
            cost_usd: 0.1,
            priced: true,
        };
        let mut view = empty_view();
        view.usage_stats = vec![row(3), row(2), row(1)]; // 3 day buckets
        let mut app = remote_app();
        app.overlay = Overlay::Usage;

        // Overscroll: 5 downs against 3 buckets pin at the LAST bucket (2).
        for _ in 0..5 {
            app.on_key_usage(KeyCode::Char('j'), Some(&view));
        }
        assert_eq!(app.usage_scroll, 2, "stored offset clamped at last bucket");
        // ONE up moves immediately — no invisible debt to pay first.
        app.on_key_usage(KeyCode::Char('k'), Some(&view));
        assert_eq!(app.usage_scroll, 1);
        // Granularity cycle resets the scroll; the next granularity (month)
        // has no rows in this view, so the offset stays pinned at 0.
        app.on_key_usage(KeyCode::Char('g'), Some(&view));
        assert_eq!(app.usage_scroll, 0);
        app.on_key_usage(KeyCode::Char('j'), Some(&view));
        assert_eq!(app.usage_scroll, 0, "empty granularity can't scroll");
        // U closes back to MAIN.
        app.on_key_usage(KeyCode::Char('U'), Some(&view));
        assert_eq!(app.overlay, Overlay::None);

        // Mouse wheel scrolls the Usage overlay (review CR) through the same
        // clamped path — cycle back to the day granularity first.
        use crossterm::event::{MouseEvent, MouseEventKind};
        app.overlay = Overlay::Usage;
        app.usage_gran = activity::UsageGran::Day;
        app.usage_scroll = 0;
        let wheel = |kind| MouseEvent {
            kind,
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.on_mouse(wheel(MouseEventKind::ScrollDown), Some(&view)));
        assert_eq!(app.usage_scroll, 1, "wheel down scrolls one bucket");
        assert!(app.on_mouse(wheel(MouseEventKind::ScrollUp), Some(&view)));
        assert_eq!(app.usage_scroll, 0, "wheel up scrolls back");
    }

    /// `open_sessions` must NOT block on the file read: it opens the overlay and
    /// flips into the loading state immediately, leaving `sessions` untouched.
    /// With no `sessions_tx` (the event loop never runs under test) the load is
    /// never kicked off, so the file is never read and nothing populates the
    /// list — proving the key handler returns instantly.
    #[test]
    fn open_sessions_is_non_blocking_and_enters_loading_state() {
        let mut app = remote_app();
        assert!(!app.sessions_loading);
        assert!(app.sessions.is_empty());

        app.open_sessions();

        assert_eq!(app.overlay, Overlay::Sessions);
        assert!(app.sessions_loading, "overlay enters the loading state");
        assert_eq!(app.session_cursor, 0);
        assert_eq!(app.sessions_pct, 0, "progress resets to 0 on a fresh open");
        assert!(
            app.sessions.is_empty(),
            "no tx under test → load not kicked off, sessions stay empty"
        );
    }

    /// `pct = bytes_read*100/file_len`, clamped, with an empty/unknown file
    /// pinned to 100 so the title never overshoots or divides by zero.
    #[test]
    fn sessions_load_pct_clamps_and_guards_empty() {
        assert_eq!(
            sessions_load_pct(0, 0),
            100,
            "empty file → 100, no div-by-0"
        );
        assert_eq!(sessions_load_pct(0, 200), 0, "nothing read yet → 0%");
        assert_eq!(sessions_load_pct(100, 200), 50, "half read → 50%");
        assert_eq!(sessions_load_pct(200, 200), 100, "fully read → 100%");
        assert_eq!(sessions_load_pct(300, 200), 100, "overshoot clamps to 100");
    }

    /// Reopening while a load is still in flight is a no-op guard, not a second
    /// load: it stays in the loading state and does not clear the cursor twice or
    /// touch `sessions`.
    #[test]
    fn open_sessions_reopen_while_loading_is_a_noop_guard() {
        let mut app = remote_app();
        app.open_sessions();
        assert!(app.sessions_loading);
        // Move the cursor as if a prior list were shown, then reopen.
        app.session_cursor = 5;
        app.open_sessions();
        // Still loading; the early-return guard ran AFTER resetting the cursor.
        assert!(app.sessions_loading);
        assert_eq!(app.overlay, Overlay::Sessions);
    }

    /// The loading labels' shimmer sweeps from when their load began, on the
    /// dashboard clock: opening the sessions overlay and the keys panel's
    /// first query stamp it; a keys re-query while one is still in flight
    /// keeps the stamp (the label has not changed), and a new load after the
    /// answer restarts it.
    #[test]
    fn loading_labels_sweep_from_when_their_load_began() {
        let ago = |secs| {
            Instant::now()
                .checked_sub(Duration::from_secs(secs))
                .expect("monotonic clock far enough from its origin")
        };
        let minute = Duration::from_secs(60);
        let mut app = remote_app();
        app.started = ago(60);
        app.open_sessions();
        let since = app.chrome().sessions_since;
        assert!(since >= minute && since < minute * 2, "{since:?}");

        app.open_keys();
        let first = app.keys.loading_since;
        assert!(first >= minute && first < minute * 2, "{first:?}");
        app.started = ago(180); // two minutes on
        app.on_key_keys(KeyCode::Char('r'), None);
        assert!(app.keys.loading, "a refresh is in flight");
        assert_eq!(app.keys.loading_since, first, "still the same loading…");
        let (_, doc) = keys_view_and_doc();
        app.apply_keys_load(KeysLoad {
            generation: app.keys.generation,
            result: Ok(doc),
        });
        app.on_key_keys(KeyCode::Char('r'), None);
        assert!(
            app.keys.loading_since > first + minute,
            "a new load restarts the sweep: {:?}",
            app.keys.loading_since
        );
    }

    // --- account upsert wording (re-login over an existing account) --------

    /// L-6: a login/add over an EXISTING identity replaces its credentials —
    /// the status must say `updated`, not `added`, or an operator re-logging
    /// in reads it as a duplicate account having appeared.
    #[test]
    fn account_upsert_status_distinguishes_updated_from_added() {
        assert_eq!(add_account_status("acme", true), "added account acme");
        assert_eq!(add_account_status("acme", false), "updated account acme");
        assert_eq!(login_status("acme", true), "logged in: added acme");
        assert_eq!(login_status("acme", false), "logged in: updated acme");
        assert!(
            !login_status("acme", false).contains("added"),
            "an update never reads as an add"
        );
    }

    /// The remote paths read the endpoint's `added` flag. A daemon too old to
    /// send it keeps the historical wording rather than claiming an update it
    /// cannot know about.
    #[test]
    fn remote_upsert_flag_defaults_to_added_when_absent() {
        assert!(!response_added(&serde_json::json!({ "added": false })));
        assert!(response_added(&serde_json::json!({ "added": true })));
        assert!(response_added(&serde_json::json!({ "ok": true })));
    }

    // --- keys panel (keys-history K) ---------------------------------------

    /// A view with one issued key, and a panel document giving it usage plus a
    /// builtin bucket — the panel's two data sources.
    fn keys_view_and_doc() -> (DashboardView, crate::key_usage::KeysUsageDoc) {
        let mut view = empty_view();
        view.client_keys = vec![crate::dashboard::KeyRowDoc {
            id: "k-aaaa".into(),
            name: "pc-b".into(),
            email: None,
            kind: "default".into(),
            key_prefix: "lmk-b1b2".into(),
            suspended: false,
            created_at_ms: 1,
            revoked_at_ms: None,
        }];
        let doc = crate::key_usage::KeysUsageDoc {
            window: "all".into(),
            available_models: vec!["model-a".into(), "model-b".into()],
            rows: 3,
            tenants: vec![crate::dashboard::TenantUsageDoc {
                tenant: "k-aaaa".into(),
                name: "pc-b".into(),
                email: None,
                requests: 3,
                ok: 3,
                errors: 0,
                tokens_in: 1,
                tokens_out: 1,
                cost_usd: 0.0,
                first_ms: 1,
                last_ms: 2,
                models: vec![crate::dashboard::TenantModelDoc {
                    group: "claude".into(),
                    model: "model-a".into(),
                    requests: 3,
                    tokens_in: 1,
                    tokens_out: 1,
                    cache_read: 0,
                    cache_creation: 0,
                    cost_usd: 0.0,
                }],
            }],
            ..Default::default()
        };
        (view, doc)
    }

    /// K-0: opening the panel asks the durable store — the overlay never
    /// renders whatever happened to be in memory from a previous question.
    #[test]
    fn opening_the_keys_overlay_dispatches_a_usage_query() {
        let mut app = remote_app();
        app.on_key_main(KeyCode::Char('K'), None);
        assert_eq!(app.overlay, Overlay::Keys);
        assert!(app.keys.loading, "the panel enters the loading state");
        let query = app.take_pending_keys().expect("a query was queued");
        assert_eq!(query.window, crate::key_usage::UsageWindow::All);
        assert!(query.models.is_empty(), "no filter by default");
        assert_eq!(query.generation, app.keys.generation);
    }

    /// K-2 `w`: the window cycles through the offered set and each press asks
    /// the store again (a window change is a NEW question, not a re-label).
    #[test]
    fn w_cycles_the_keys_window_and_requeries() {
        let mut app = remote_app();
        app.on_key_main(KeyCode::Char('K'), None);
        app.take_pending_keys();
        app.keys.scroll = 7;

        app.on_key_keys(KeyCode::Char('w'), None);
        assert_eq!(app.keys.window, crate::key_usage::UsageWindow::H1);
        assert_eq!(app.keys.scroll, 0, "a new window starts at the top");
        let query = app.take_pending_keys().expect("re-queried");
        assert_eq!(query.window, crate::key_usage::UsageWindow::H1);

        for want in [
            crate::key_usage::UsageWindow::H24,
            crate::key_usage::UsageWindow::D7,
            crate::key_usage::UsageWindow::D14,
            crate::key_usage::UsageWindow::D30,
            crate::key_usage::UsageWindow::D90,
            crate::key_usage::UsageWindow::All,
        ] {
            app.on_key_keys(KeyCode::Char('w'), None);
            assert_eq!(app.keys.window, want, "cycle wraps back to all");
        }
    }

    /// K-0 `위아래 키로`: Down/PageDown/End reach the LAST flattened row and
    /// clamp there; Home/Up return to the top.
    #[test]
    fn keys_scrolling_covers_every_row_and_clamps_at_both_ends() {
        let (view, doc) = keys_view_and_doc();
        let mut app = remote_app();
        app.overlay = Overlay::Keys;
        app.keys.doc = Some(std::sync::Arc::new(doc));
        // 1 key row + 1 model row = 2 rows → last index 1.
        assert_eq!(ui::keys_row_count(&view, &app.chrome()), 2);

        app.on_key_keys(KeyCode::Down, Some(&view));
        assert_eq!(app.keys.scroll, 1);
        app.on_key_keys(KeyCode::Down, Some(&view));
        assert_eq!(app.keys.scroll, 1, "clamped at the last row");
        app.on_key_keys(KeyCode::PageDown, Some(&view));
        assert_eq!(app.keys.scroll, 1, "a page past the end still clamps");
        app.on_key_keys(KeyCode::Home, Some(&view));
        assert_eq!(app.keys.scroll, 0);
        app.on_key_keys(KeyCode::End, Some(&view));
        assert_eq!(app.keys.scroll, 1, "End lands on the final row");
        app.on_key_keys(KeyCode::Up, Some(&view));
        assert_eq!(app.keys.scroll, 0);
        app.on_key_keys(KeyCode::PageUp, Some(&view));
        assert_eq!(app.keys.scroll, 0, "PageUp at the top stays put");
    }

    /// Draw the app's current frame and keep the hit record the mouse
    /// handler reads, as the event loop does after each draw.
    fn draw_and_adopt(app: &mut App, view: &DashboardView, w: u16, h: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).expect("terminal");
        let chrome = app.chrome();
        let mut hits = None;
        terminal
            .draw(|frame| ui::draw(frame, Some(view), &chrome, &mut hits))
            .expect("draw");
        let main = hits.unwrap_or_default();
        app.sessions_chrome = main.sessions_table;
        app.table_scroll = main.table_scroll;
        app.logs_area = main.logs_area;
        if let Some(pos) = main.logs {
            app.logs_pos = pos;
        }
        app.raw_chrome = main.raw_modal.clone().unwrap_or_default();
        if let (Some(modal), Some(raw)) = (app.raw_modal.as_mut(), main.raw_modal.as_ref()) {
            modal.scroll = modal.scroll.min(raw.max_scroll);
        }
        let buffer = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect()
    }

    fn left(kind: MouseEventKind, column: u16, row: u16) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// The keys table's scrollbar (batch 2): a press low on the track jumps
    /// the table there, the thumb can be grabbed and dragged back to the top,
    /// and the release ends the drag — the wheel and keys still scroll.
    #[test]
    fn keys_table_scrollbar_jumps_on_a_track_click_and_drags_with_the_thumb() {
        let (view, mut doc) = keys_view_and_doc();
        let tenant = doc.tenants[0].clone();
        doc.tenants = (0..40)
            .map(|i| crate::dashboard::TenantUsageDoc {
                tenant: format!("k-{i:04}"),
                name: format!("tenant-{i:02}"),
                ..tenant.clone()
            })
            .collect();
        let mut app = remote_app();
        app.overlay = Overlay::Keys;
        app.keys.doc = Some(std::sync::Arc::new(doc));
        let rows = draw_and_adopt(&mut app, &view, 120, 24);
        let total = ui::keys_row_count(&view, &app.chrome());
        assert!(total >= 80, "40 tenants and their model rows");
        let bar = app.table_scroll.expect("the rows overflow the panel");
        assert_eq!((bar.pos.top, bar.pos.max), (0, total - bar.pos.viewport));
        assert_eq!(bar.track.x, 119, "on the panel's right border");
        let col = |y: u16| rows[usize::from(y)].chars().last().unwrap_or(' ');
        assert_eq!(
            col(bar.track.y),
            '▐',
            "thumb at the top:\n{}",
            rows.join("\n")
        );
        assert_eq!(col(bar.track.bottom() - 1), '╎', "track below it");

        // A press on the last track row jumps to the end.
        let last = bar.track.bottom() - 1;
        assert!(app.on_mouse(
            left(MouseEventKind::Down(MouseButton::Left), 119, last),
            Some(&view)
        ));
        assert_eq!(app.keys.scroll, bar.pos.max);
        let rows = draw_and_adopt(&mut app, &view, 120, 24);
        assert_eq!(
            rows[usize::from(last)].chars().last(),
            Some('▐'),
            "thumb at the bottom"
        );
        assert!(
            rows.join("\n").contains("tenant-39"),
            "the last rows are on screen"
        );

        // The same press keeps dragging: back to the top row of the track.
        let bar = app.table_scroll.expect("still overflowing");
        app.on_mouse(
            left(MouseEventKind::Drag(MouseButton::Left), 40, 0),
            Some(&view),
        );
        assert_eq!(
            app.keys.scroll, 0,
            "dragged past the top end reaches the top"
        );
        app.on_mouse(
            left(MouseEventKind::Up(MouseButton::Left), 40, 0),
            Some(&view),
        );
        assert!(
            !app.on_mouse(
                left(MouseEventKind::Drag(MouseButton::Left), 119, last),
                Some(&view)
            ),
            "released: a drag no longer scrolls"
        );
        assert_eq!(app.keys.scroll, 0);
        let _ = bar;

        // Keys and the wheel still scroll.
        app.on_key_keys(KeyCode::Down, Some(&view));
        assert_eq!(app.keys.scroll, 1);
        app.on_mouse(left(MouseEventKind::ScrollDown, 50, 10), Some(&view));
        assert_eq!(app.keys.scroll, 2, "the wheel scrolls like ↓");
    }

    /// The raw viewer's body scrolls by wrapped rows: PgDn moves a view's
    /// height, a press low on its scrollbar jumps to the end and the same
    /// press drags back to the top, and a click on a tab — at the rect the
    /// strip drew — switches to it and starts at the top.
    #[test]
    fn raw_viewer_scrollbar_drags_and_tabs_click_where_drawn() {
        let mut app = remote_app();
        let key = seed_one_hit(&mut app);
        let body = format!(r#"{{"content":"{}"}}"#, "ab".repeat(600));
        let record = crate::proxy::raw_io::RawIoRecord::new(
            7,
            0,
            None,
            None,
            None,
            Some(200),
            body.as_bytes(),
            b"{}",
            1 << 20,
            None,
            None,
            None,
        );
        let content = ui::raw_content_from_record(
            ui::RawGeneral {
                lines: Vec::new(),
                method: "POST".into(),
                path: "/v1/messages".into(),
                base_url: "http://localhost:3456".into(),
            },
            &record,
        );
        app.raw_modal = Some(RawModal {
            key,
            id: 7,
            generation: 1,
            title: "raw".into(),
            tab: 0,
            scroll: 0,
            view: Default::default(),
            spin: 0,
            since: Duration::ZERO,
            flash: None,
            state: RawModalState::Ready(std::sync::Arc::new(content)),
        });
        let view = empty_view();
        draw_and_adopt(&mut app, &view, 80, 24);
        let raw = app.raw_chrome.clone();
        let bar = raw.bar.expect("1200 cells of string overflow 80x24");
        let scroll = |app: &App| app.raw_modal.as_ref().map(|m| m.scroll);

        app.on_key(press(KeyCode::PageDown), None);
        assert_eq!(scroll(&app), Some(raw.page), "a page is the view's height");
        app.on_key(press(KeyCode::PageUp), None);
        assert_eq!(scroll(&app), Some(0));

        let last = bar.track.bottom() - 1;
        app.on_mouse(
            left(MouseEventKind::Down(MouseButton::Left), bar.track.x, last),
            None,
        );
        assert_eq!(
            scroll(&app),
            Some(raw.max_scroll),
            "track click jumps to the end"
        );
        app.on_mouse(left(MouseEventKind::Drag(MouseButton::Left), 3, 0), None);
        assert_eq!(scroll(&app), Some(0), "the held thumb drags to the top");
        app.on_mouse(left(MouseEventKind::Up(MouseButton::Left), 3, 0), None);
        app.on_mouse(
            left(MouseEventKind::Drag(MouseButton::Left), bar.track.x, last),
            None,
        );
        assert_eq!(scroll(&app), Some(0), "released: no drag");

        app.on_key(press(KeyCode::End), None);
        draw_and_adopt(&mut app, &view, 80, 24);
        assert_eq!(
            scroll(&app),
            Some(raw.max_scroll),
            "End clamps to the last row"
        );
        let (index, rect) = raw.tabs[1];
        app.on_mouse(
            left(MouseEventKind::Down(MouseButton::Left), rect.x, rect.y),
            None,
        );
        let modal = app.raw_modal.as_ref().expect("still open");
        assert_eq!(
            (modal.tab, modal.scroll),
            (index, 0),
            "Response, from the top"
        );
    }

    /// The sessions table follows its cursor, so its scrollbar moves the
    /// cursor (as the wheel does) to the row that puts the dragged-to place
    /// first; a click on a row still selects that row.
    #[test]
    fn sessions_table_scrollbar_drag_moves_the_view_through_the_cursor() {
        let session = |i: u64| crate::session::Session {
            user_id: Some(format!("u-{i:02}")),
            requests: 1,
            tokens_in: 1,
            tokens_out: 1,
            models: vec![],
            accounts: vec![],
            account_rotations: 0,
            first_ms: i,
            last_ms: i,
            duration_ms_sum: 0,
            timed_requests: 0,
            tokens_out_timed: 0,
            confidence: crate::session::Confidence::High,
        };
        let view = empty_view();
        let mut app = remote_app();
        app.overlay = Overlay::Sessions;
        app.sessions = (0..60).map(session).collect();
        draw_and_adopt(&mut app, &view, 80, 24);
        let bar = app.table_scroll.expect("60 sessions overflow");
        let page = bar.pos.viewport;
        let rows_hit = app.sessions_chrome.expect("rows drawn").rows;
        assert!(
            rows_hit.right() <= bar.track.x,
            "the bar's column is not a row"
        );

        // Press on the bottom of the track: the last page, cursor on the last row.
        let last = bar.track.bottom() - 1;
        app.on_mouse(
            left(MouseEventKind::Down(MouseButton::Left), bar.track.x, last),
            Some(&view),
        );
        assert_eq!(app.session_cursor, 59);
        let rows = draw_and_adopt(&mut app, &view, 80, 24);
        assert!(rows.join("\n").contains("u-59"), "{}", rows.join("\n"));
        assert_eq!(app.table_scroll.map(|b| b.pos.top), Some(60 - page));
        app.on_mouse(
            left(MouseEventKind::Up(MouseButton::Left), bar.track.x, last),
            Some(&view),
        );

        // A row click still selects that row, not the bar.
        let rows_hit = app.sessions_chrome.expect("rows drawn").rows;
        app.on_mouse(
            left(MouseEventKind::Down(MouseButton::Left), 5, rows_hit.y),
            Some(&view),
        );
        assert_eq!(app.session_cursor, 60 - page, "the first row in view");
    }

    /// The usage table scrolls by bucket while its scrollbar counts rows: a
    /// press at the bottom of the track lands on the first bucket that shows
    /// the last rows; one at the top returns to the newest bucket.
    #[test]
    fn usage_table_scrollbar_lands_on_buckets() {
        let row = |bucket: u64, model: &str| crate::dashboard::UsageStatDoc {
            gran: "day".into(),
            bucket,
            label: format!("day-{bucket:02}"),
            group: "claude".into(),
            model: model.into(),
            requests: 1,
            tokens_in: 1,
            tokens_out: 1,
            cache_read: 0,
            cache_creation: 0,
            cost_usd: 0.1,
            priced: true,
        };
        let mut view = empty_view();
        // 20 day buckets of two models each: 60 table rows.
        view.usage_stats = (0..20u64)
            .rev()
            .flat_map(|b| [row(b, "m1"), row(b, "m2")])
            .collect();
        let mut app = remote_app();
        app.overlay = Overlay::Usage;
        app.usage_gran = activity::UsageGran::Day;
        draw_and_adopt(&mut app, &view, 120, 24);
        let bar = app.table_scroll.expect("60 rows overflow");
        assert_eq!(bar.pos.max, 60 - bar.pos.viewport);
        let last = bar.track.bottom() - 1;
        app.on_mouse(
            left(MouseEventKind::Down(MouseButton::Left), bar.track.x, last),
            Some(&view),
        );
        app.on_mouse(
            left(MouseEventKind::Up(MouseButton::Left), bar.track.x, last),
            Some(&view),
        );
        // Rows before bucket k: 3k. The first bucket at or after max.
        assert_eq!(app.usage_scroll, bar.pos.max.div_ceil(3));
        let rows = draw_and_adopt(&mut app, &view, 120, 24);
        assert!(
            rows.join("\n").contains("day-00"),
            "the oldest bucket shows"
        );
        app.on_mouse(
            left(
                MouseEventKind::Down(MouseButton::Left),
                bar.track.x,
                bar.track.y,
            ),
            Some(&view),
        );
        assert_eq!(
            app.usage_scroll, 0,
            "the top of the track is the newest bucket"
        );
    }

    /// K-3 `f`: Space toggles, Enter applies the selection AND re-queries;
    /// the picker offers the observed models unioned with the live selection.
    #[test]
    fn f_picker_toggles_models_and_enter_applies_the_filter() {
        let (_, doc) = keys_view_and_doc();
        let mut app = remote_app();
        app.open_keys();
        app.apply_keys_load(KeysLoad {
            generation: app.keys.generation,
            result: Ok(doc),
        });
        app.take_pending_keys();

        app.on_key_keys(KeyCode::Char('f'), None);
        let picker = app.keys.picker.as_ref().expect("picker opened");
        assert_eq!(picker.options(), vec!["model-a", "model-b"]);
        assert!(picker.selected().is_empty(), "starts from the live filter");

        app.on_key_keys(KeyCode::Char(' '), None); // select model-a
        app.on_key_keys(KeyCode::Down, None);
        app.on_key_keys(KeyCode::Char(' '), None); // select model-b
        app.on_key_keys(KeyCode::Char(' '), None); // …and unselect it again
        assert!(
            app.pending_keys.is_none(),
            "nothing is applied before Enter"
        );

        app.on_key_keys(KeyCode::Enter, None);
        assert!(app.keys.picker.is_none(), "Enter closes the picker");
        assert_eq!(
            app.keys.models.iter().cloned().collect::<Vec<_>>(),
            vec!["model-a".to_string()]
        );
        let query = app.take_pending_keys().expect("applied filter re-queries");
        assert_eq!(query.models, vec!["model-a".to_string()]);
    }

    /// K-3: `a` selects every offered model, `c` clears — and Esc abandons the
    /// working selection without touching the active filter.
    #[test]
    fn picker_all_clear_and_escape_leave_the_active_filter_alone() {
        let (_, doc) = keys_view_and_doc();
        let mut app = remote_app();
        app.open_keys();
        app.apply_keys_load(KeysLoad {
            generation: app.keys.generation,
            result: Ok(doc),
        });
        app.take_pending_keys();
        app.keys.models = ["model-a".to_string()].into_iter().collect();

        app.on_key_keys(KeyCode::Char('f'), None);
        app.on_key_keys(KeyCode::Char('a'), None);
        assert_eq!(app.keys.picker.as_ref().unwrap().selected().len(), 2, "all");
        app.on_key_keys(KeyCode::Char('c'), None);
        assert!(
            app.keys.picker.as_ref().unwrap().selected().is_empty(),
            "clear"
        );

        app.on_key_keys(KeyCode::Esc, None);
        assert!(app.keys.picker.is_none(), "Esc closes the picker…");
        assert_eq!(app.overlay, Overlay::Keys, "…not the overlay");
        assert_eq!(
            app.keys.models.iter().cloned().collect::<Vec<_>>(),
            vec!["model-a".to_string()],
            "the abandoned selection never applied"
        );
        assert!(app.pending_keys.is_none(), "cancel does not re-query");
    }

    /// K-5: while a CHANGED question is in flight the old answer is dropped —
    /// the title already says the new window/filter, so leaving the previous
    /// totals on screen would label them with a question they never answered.
    #[test]
    fn changing_the_question_drops_the_previous_answer_immediately() {
        let (_, doc) = keys_view_and_doc();
        let mut app = remote_app();
        app.open_keys();
        app.apply_keys_load(KeysLoad {
            generation: app.keys.generation,
            result: Ok(doc),
        });
        assert!(app.keys.doc.is_some());

        app.on_key_keys(KeyCode::Char('w'), None); // all → 1h
        assert!(
            app.keys.doc.is_none(),
            "the 24h-labelled frame must not render the all-time rows"
        );
        assert!(app.keys.loading);
    }

    /// …but a plain refresh (`r`, same window and filter) keeps the rows on
    /// screen: the label still describes exactly what is drawn, so blanking
    /// the table would only flicker.
    #[test]
    fn refreshing_the_same_question_keeps_the_rows_on_screen() {
        let (_, doc) = keys_view_and_doc();
        let mut app = remote_app();
        app.open_keys();
        app.apply_keys_load(KeysLoad {
            generation: app.keys.generation,
            result: Ok(doc),
        });
        app.on_key_keys(KeyCode::Char('r'), None);
        assert!(app.keys.doc.is_some(), "same question → same rows stay");
        assert!(app.keys.loading, "and a refresh is still in flight");
    }

    /// The picker keeps offering the observed models while a query is in
    /// flight: the option list is cached separately from the answer, so
    /// dropping stale rows cannot empty `f`.
    #[test]
    fn the_model_picker_still_offers_options_while_a_query_is_in_flight() {
        let (_, doc) = keys_view_and_doc();
        let mut app = remote_app();
        app.open_keys();
        app.apply_keys_load(KeysLoad {
            generation: app.keys.generation,
            result: Ok(doc),
        });
        app.on_key_keys(KeyCode::Char('w'), None); // in-flight, rows dropped
        assert!(app.keys.doc.is_none());

        app.on_key_keys(KeyCode::Char('f'), None);
        let picker = app.keys.picker.as_ref().expect("picker opened");
        assert_eq!(
            picker.options(),
            vec!["model-a".to_string(), "model-b".to_string()],
            "cached options survive the in-flight query"
        );
    }

    /// The picker refreshes its options when the query lands: `f` pressed
    /// during the first load used to keep an empty list ("no models
    /// observed") over a panel full of models. Checks and the cursor row
    /// survive the refresh.
    #[test]
    fn the_model_picker_refreshes_its_options_when_the_query_lands() {
        let (view, doc) = keys_view_and_doc();
        let mut app = remote_app();
        app.open_keys();
        app.overlay = Overlay::Keys;
        app.on_key_keys(KeyCode::Char('f'), None);
        assert!(app.keys.picker.as_ref().expect("open").options().is_empty());
        let frame = render_app(&app, &view, 80, 24);
        assert!(
            frame.contains("loading the models in this window"),
            "{frame}"
        );

        app.apply_keys_load(KeysLoad {
            generation: app.keys.generation,
            result: Ok(doc.clone()),
        });
        let picker = app.keys.picker.as_ref().expect("still open");
        assert_eq!(picker.options(), vec!["model-a", "model-b"]);
        app.on_key_keys(KeyCode::Down, None);
        app.on_key_keys(KeyCode::Char(' '), None); // check model-b
                                                   // A later answer (a refresh) rebuilds again and keeps the check and
                                                   // the cursor on model-b.
        app.request_keys_usage();
        app.apply_keys_load(KeysLoad {
            generation: app.keys.generation,
            result: Ok(doc),
        });
        let picker = app.keys.picker.as_ref().expect("still open");
        assert_eq!(
            picker.selected().into_iter().collect::<Vec<_>>(),
            vec!["model-b"]
        );
        assert_eq!(picker.list.selected(), Some(1), "cursor stays on model-b");
        let frame = render_app(&app, &view, 80, 24);
        assert!(frame.contains("1 of 2 selected"), "{frame}");
    }

    /// Type-to-filter starts on `/` (letters are picker keys until then):
    /// the count still covers every model and says how many the filter
    /// hides; Esc closes the filter, not the picker.
    #[test]
    fn the_model_picker_filters_after_slash_and_counts_every_model() {
        let (view, doc) = keys_view_and_doc();
        let mut app = remote_app();
        app.open_keys();
        app.overlay = Overlay::Keys;
        app.apply_keys_load(KeysLoad {
            generation: app.keys.generation,
            result: Ok(doc),
        });
        app.take_pending_keys();
        app.on_key_keys(KeyCode::Char('f'), None);
        app.on_key_keys(KeyCode::Char('b'), None);
        assert_eq!(
            app.keys.picker.as_ref().unwrap().list.search_query(),
            "",
            "a letter before `/` types nothing"
        );
        app.on_key_keys(KeyCode::Char('/'), None);
        for c in "del-b".chars() {
            app.on_key_keys(KeyCode::Char(c), None);
        }
        let picker = app.keys.picker.as_ref().unwrap();
        assert_eq!(picker.list.search_query(), "del-b");
        assert_eq!(picker.list.visible_len(), 1);
        app.on_key_keys(KeyCode::Char(' '), None); // check model-b under the filter
        let frame = render_app(&app, &view, 80, 24);
        assert!(frame.contains("1 of 2 selected"), "{frame}");
        assert!(frame.contains("(1 hidden by the filter)"), "{frame}");
        assert!(frame.contains("Esc stop filtering"), "{frame}");
        // `a` while filtering is a letter of the filter, not "select all".
        app.on_key_keys(KeyCode::Char('a'), None);
        assert_eq!(app.keys.picker.as_ref().unwrap().selected().len(), 1);
        app.on_key_keys(KeyCode::Esc, None);
        let picker = app
            .keys
            .picker
            .as_ref()
            .expect("Esc closes the filter only");
        assert!(!picker.filtering && picker.list.search_query().is_empty());
        app.on_key_keys(KeyCode::Enter, None);
        assert_eq!(
            app.keys.models.iter().cloned().collect::<Vec<_>>(),
            vec!["model-b".to_string()]
        );
    }

    /// K-5: a late answer to a superseded question is dropped — a fast
    /// `w w` can never leave the panel showing the older window's rows.
    #[test]
    fn a_stale_keys_answer_is_ignored() {
        let (_, doc) = keys_view_and_doc();
        let mut app = remote_app();
        app.open_keys(); // generation 1
        app.on_key_keys(KeyCode::Char('w'), None); // generation 2
        app.apply_keys_load(KeysLoad {
            generation: 1,
            result: Ok(doc),
        });
        assert!(app.keys.doc.is_none(), "the superseded answer is dropped");
        assert!(app.keys.loading, "still waiting for the current one");
    }

    /// K-5: a failed query drops the previous document — stale rows must never
    /// sit under a label describing a different question.
    #[test]
    fn a_failed_keys_query_replaces_the_rows_with_the_failure() {
        let (_, doc) = keys_view_and_doc();
        let mut app = remote_app();
        app.open_keys();
        app.apply_keys_load(KeysLoad {
            generation: app.keys.generation,
            result: Ok(doc),
        });
        assert!(app.keys.doc.is_some());

        app.on_key_keys(KeyCode::Char('w'), None);
        app.apply_keys_load(KeysLoad {
            generation: app.keys.generation,
            result: Err("database is locked".into()),
        });
        assert!(app.keys.doc.is_none(), "no rows under a failed query");
        assert_eq!(app.keys.error.as_deref(), Some("database is locked"));
        assert!(!app.keys.loading);
    }

    /// The `models` query parameter is percent-encoded, so a model name with a
    /// separator or a space cannot forge extra filter entries.
    #[test]
    fn model_names_are_percent_encoded_for_the_query_string() {
        assert_eq!(encode_query_value("claude-opus-4-8"), "claude-opus-4-8");
        assert_eq!(
            encode_query_value("anthropic/claude,x y"),
            "anthropic%2Fclaude%2Cx%20y"
        );
    }

    /// `g` opens the Stats overlay only when model usage exists; `g`/`Esc`
    /// close it. The no-data guard keeps MAIN (matching the old `show_models`
    /// behavior).
    #[test]
    fn g_opens_stats_overlay_only_with_model_data() {
        let mut app = remote_app();
        // No view → no model data → stays on MAIN with a hint.
        app.on_key_main(KeyCode::Char('g'), None);
        assert_eq!(app.overlay, Overlay::None);

        let view = stats_view();
        app.on_key_main(KeyCode::Char('g'), Some(&view));
        assert_eq!(app.overlay, Overlay::Stats);
        app.on_key_stats(KeyCode::Esc, Some(&view));
        assert_eq!(app.overlay, Overlay::None);
    }

    /// `w` in the Stats overlay cycles the heatmap window 24h ↔ 72h (issue #23)
    /// without closing the overlay.
    #[test]
    fn w_cycles_the_stats_heatmap_window() {
        let mut app = remote_app();
        let view = stats_view();
        app.on_key_main(KeyCode::Char('g'), Some(&view));
        assert_eq!(app.overlay, Overlay::Stats);
        assert_eq!(app.stats_window, activity::StatsWindow::Day);
        app.on_key_stats(KeyCode::Char('w'), Some(&view));
        assert_eq!(app.stats_window, activity::StatsWindow::ThreeDay);
        assert_eq!(app.overlay, Overlay::Stats, "w stays in the overlay");
        app.on_key_stats(KeyCode::Char('w'), Some(&view));
        assert_eq!(app.stats_window, activity::StatsWindow::Day, "cycles back");
    }

    /// The Accounts overlay houses the #3/#4 affordances: `a`→AddKey,
    /// `r`→ConfirmRemove, `s`→Select, all entering their own `Mode` over the
    /// overlay (which stays open).
    #[test]
    fn accounts_overlay_houses_add_remove_switch_modes() {
        let view = stats_view_with_account();
        let mut app = remote_app();
        app.overlay = Overlay::Accounts;

        app.on_key_accounts(KeyCode::Char('a'), Some(&view));
        assert_eq!(app.mode, Mode::AddKey);
        assert_eq!(
            app.overlay,
            Overlay::Accounts,
            "overlay stays open over Mode"
        );
        app.on_key_add(press(KeyCode::Esc)); // cancel back to Normal
        assert_eq!(app.mode, Mode::Normal);

        app.on_key_accounts(KeyCode::Char('r'), Some(&view));
        assert_eq!(app.mode, Mode::ConfirmRemove { idx: 0 });
        app.on_key_confirm_remove(KeyCode::Esc, 0, Some(&view));
        assert_eq!(app.mode, Mode::Normal);

        app.on_key_accounts(KeyCode::Char('s'), Some(&view));
        assert_eq!(app.mode, Mode::Select { idx: 0 });
    }

    /// A pending `Mode` interaction takes the key before the overlay handler,
    /// so add/remove/login keep working while Accounts is open (issues #3/#4).
    #[test]
    fn pending_mode_takes_keys_over_the_overlay() {
        let mut app = remote_app();
        app.overlay = Overlay::Accounts;
        app.mode = Mode::AddKey;
        // A printable char goes to the key buffer, not the overlay handler.
        app.on_key(press(KeyCode::Char('x')), None);
        assert_eq!(app.mode, Mode::AddKey);
        assert_eq!(app.field.take_secret(), "x");
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(app: &mut App, view: &DashboardView, text: &str) {
        for c in text.chars() {
            app.on_key(press(KeyCode::Char(c)), Some(view));
        }
    }

    /// Draw a frame as the runtime loop does, writing the open field's
    /// window back so the next frame scrolls from where this one drew.
    fn render_tracked(app: &mut App, view: &DashboardView, w: u16, h: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).expect("terminal");
        let chrome = app.chrome();
        let mut hits = None;
        terminal
            .draw(|frame| ui::draw(frame, Some(view), &chrome, &mut hits))
            .expect("draw");
        app.track_field(hits.unwrap_or_default().field_width);
        let buffer = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The add-key field is a masked composer: typing, a paste, caret moves,
    /// mid-value edits and Ctrl+U/K/W/Y all edit the real key, and no frame
    /// at 80×24 or 179×43 ever shows a piece of it — only `∙` glyphs. The key
    /// comes out once, through `take_secret`, on Enter.
    #[test]
    fn api_key_never_appears_in_any_rendered_frame() {
        const KEY: &str = "sk-ant-api03-Q7zXv9Lm2Wq8Rt5Yp3Kj";
        let view = stats_view_with_account();
        let mut app = remote_app();
        app.overlay = Overlay::Accounts;
        app.on_key(press(KeyCode::Char('a')), Some(&view));
        assert_eq!(app.mode, Mode::AddKey);
        let mut frames: Vec<String> = Vec::new();
        // Both sizes; the index of the 80×24 frame.
        fn snap(app: &mut App, view: &DashboardView, frames: &mut Vec<String>) -> usize {
            for (w, h) in [(80, 24), (179, 43)] {
                frames.push(render_tracked(app, view, w, h));
            }
            frames.len() - 2
        }

        type_text(&mut app, &view, &KEY[..21]);
        snap(&mut app, &view, &mut frames);
        // A paste lands whole; its line break neither submits nor types.
        assert!(app.on_paste(&format!("{}\n", &KEY[21..]), Some(&view)));
        assert_eq!(app.mode, Mode::AddKey);
        let at = snap(&mut app, &view, &mut frames);
        assert!(
            frames[at].contains('‹'),
            "a long key scrolls: {}",
            frames[at]
        );
        // Home: the window goes to the start, more of the key to the right.
        app.on_key(press(KeyCode::Home), Some(&view));
        let at = snap(&mut app, &view, &mut frames);
        assert!(frames[at].contains('›'), "{}", frames[at]);
        // A mid-value edit, then undone.
        app.on_key(press(KeyCode::End), Some(&view));
        for _ in 0..4 {
            app.on_key(press(KeyCode::Left), Some(&view));
        }
        type_text(&mut app, &view, "ZZ");
        snap(&mut app, &view, &mut frames);
        app.on_key(press(KeyCode::Backspace), Some(&view));
        app.on_key(press(KeyCode::Backspace), Some(&view));
        // Ctrl+U cuts to the start (it used to type a `u`), Ctrl+Y puts it back.
        app.on_key(ctrl('u'), Some(&view));
        snap(&mut app, &view, &mut frames);
        app.on_key(ctrl('y'), Some(&view));
        // Ctrl+K cuts to the end; Ctrl+W the word before the caret.
        app.on_key(ctrl('k'), Some(&view));
        app.on_key(ctrl('y'), Some(&view));
        app.on_key(ctrl('w'), Some(&view));
        assert!(app.field.is_empty(), "the key is one word");
        snap(&mut app, &view, &mut frames);
        app.on_key(ctrl('y'), Some(&view));
        snap(&mut app, &view, &mut frames);

        let chrome = app.chrome();
        assert_eq!(chrome.field.text(), "", "the frame's copy reads empty");
        assert!(!format!("{:?}", chrome.field).contains(&KEY[..8]));
        for frame in &frames {
            // The field is drawn: glyphs, or the placeholder once emptied.
            assert!(
                frame.contains('∙') || frame.contains("paste an API key"),
                "{frame}"
            );
            for piece in KEY.as_bytes().windows(5) {
                let piece = std::str::from_utf8(piece).expect("ascii");
                assert!(!frame.contains(piece), "{piece:?} rendered:\n{frame}");
            }
        }

        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.field.is_empty());
        match &app.backend {
            Backend::Remote(remote) => assert_eq!(remote.pending_add.as_deref(), Some(KEY)),
            Backend::Local(_) => unreachable!("remote app"),
        }
    }

    /// The limits field edits at the caret, takes only what a ceiling can
    /// be (typed or pasted), keeps the text on a parse error, and applies
    /// through the same `parse_limits_input` as before.
    #[test]
    fn limits_field_edits_at_the_caret_and_keeps_its_validation() {
        let view = stats_view_with_account();
        let mut app = remote_app();
        app.overlay = Overlay::Accounts;
        app.mode = Mode::Select { idx: 0 };
        app.on_key(press(KeyCode::Char('L')), Some(&view));
        assert_eq!(app.mode, Mode::EditLimits { idx: 0 });
        type_text(&mut app, &view, "8090x");
        assert_eq!(app.field.text(), "8090", "a letter is refused");
        app.on_key(press(KeyCode::Left), Some(&view));
        app.on_key(press(KeyCode::Left), Some(&view));
        type_text(&mut app, &view, ",");
        assert_eq!(app.field.text(), "80,90");
        app.on_key(press(KeyCode::End), Some(&view));
        assert!(app.on_paste(",9x5\n", Some(&view)));
        assert_eq!(app.field.text(), "80,90,95", "the paste keeps what fits");
        let frame = render_tracked(&mut app, &view, 80, 24);
        assert!(frame.contains("limits (5h,7d,fbl %) ❯ 80,90,95"), "{frame}");
        // Home + Ctrl+K cuts it all, Ctrl+Y puts it back.
        app.on_key(press(KeyCode::Home), Some(&view));
        app.on_key(ctrl('k'), Some(&view));
        assert_eq!(app.field.text(), "");
        app.on_key(ctrl('y'), Some(&view));
        assert_eq!(app.field.text(), "80,90,95");
        // An out-of-range value: an error, and the text stays to be fixed.
        app.on_key(press(KeyCode::Home), Some(&view));
        type_text(&mut app, &view, "1");
        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(app.mode, Mode::EditLimits { idx: 0 });
        assert_eq!(app.field.text(), "180,90,95");
        app.on_key(press(KeyCode::Home), Some(&view));
        app.on_key(press(KeyCode::Delete), Some(&view));
        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(app.mode, Mode::Select { idx: 0 });
        assert!(app.field.is_empty());
        match &app.backend {
            Backend::Remote(remote) => {
                let (name, limits) = remote.pending_limits.clone().expect("queued");
                assert_eq!(name, "claude:me@example.com");
                assert_eq!(limits.five_hour_max, Some(0.80));
                assert_eq!(limits.seven_day_max, Some(0.90));
                assert_eq!(limits.fable_weekly_max, Some(0.95));
            }
            Backend::Local(_) => unreachable!("remote app"),
        }
        // Outside a field a paste is typed key by key, as before.
        app.mode = Mode::Normal;
        app.overlay = Overlay::None;
        assert!(app.on_paste("a", Some(&view)));
        assert_eq!(app.overlay, Overlay::Accounts);
    }

    /// A config value edits mid-value: the field opens on the current value
    /// with the caret at its end, ←/Backspace edit inside it, and Enter
    /// applies what the field holds through the unchanged parse/confirm flow.
    #[test]
    fn config_value_edits_mid_value() {
        let mut view = stats_view_with_account();
        view.select_params.five_hour_max = 0.9;
        let mut app = remote_app();
        app.overlay = Overlay::Config;
        app.activate_config_action(ui::ConfigAction::Ceiling5h, Some(&view));
        assert_eq!(
            app.mode,
            Mode::ConfigEdit {
                action: ui::ConfigAction::Ceiling5h
            }
        );
        assert_eq!(app.field.text(), "90");
        app.on_key(press(KeyCode::Left), Some(&view));
        app.on_key(press(KeyCode::Backspace), Some(&view));
        type_text(&mut app, &view, "7");
        assert_eq!(app.field.text(), "70");
        let frame = render_tracked(&mut app, &view, 80, 24);
        assert!(frame.contains("edit ❯ 70"), "{frame}");
        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(app.mode, Mode::Normal, "a normal percent applies directly");
        match &app.backend {
            Backend::Remote(remote) => {
                let (req, _) = remote.pending_settings.as_ref().expect("queued");
                assert_eq!(req.five_hour_max, Some(0.7));
            }
            Backend::Local(_) => unreachable!("remote app"),
        }
    }

    #[test]
    fn can_open_browser_gates_only_linux_display_not_ssh() {
        // Regression for the `n` new-login false-"headless": a Mac user inside a
        // tmux session that leaked SSH_* was wrongly blocked. GUI platforms must
        // allow regardless of SSH/display; only Linux gates on a display server.
        assert!(
            can_open_browser_decide(true, false),
            "macOS/Windows must allow the browser even with no DISPLAY / under SSH"
        );
        assert!(
            can_open_browser_decide(true, true),
            "GUI platform with a display still allows"
        );
        assert!(
            can_open_browser_decide(false, true),
            "Linux with a display server allows"
        );
        assert!(
            !can_open_browser_decide(false, false),
            "Linux with no display server is genuinely headless"
        );
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // --- Feature B: mouse click-to-expand ----------------------------------

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Seed `app.hits` with one clickable request row on screen row 6 of an
    /// activity panel at rows 5..15, returning its key.
    fn seed_one_hit(app: &mut App) -> activity::ActivityKey {
        seed_hit_as(app, ui::ActivityHitKind::Entry)
    }

    /// [`seed_one_hit`] as a target of `kind` (the frame's map holds only it).
    fn seed_hit_as(app: &mut App, kind: ui::ActivityHitKind) -> activity::ActivityKey {
        let key = activity::ActivityKey {
            at_ms: 42,
            method: "POST".into(),
            path: "/v1/messages".into(),
            status: 200,
        };
        let panel = ratatui::layout::Rect {
            x: 0,
            y: 5,
            width: 80,
            height: 10,
        };
        app.hits.clear();
        app.hits.push_clipped(
            ratatui::layout::Rect::new(panel.x, 6, panel.width, 1),
            panel,
            ui::ClickTarget::Activity {
                key: key.clone(),
                kind,
            },
        );
        key
    }

    #[test]
    fn left_click_toggles_activity_expand() {
        let mut app = remote_app();
        let key = seed_one_hit(&mut app);
        // Click the row → expands.
        let changed = app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5, 6), None);
        assert!(changed, "a click on a hit row warrants a redraw");
        assert_eq!(app.expanded_activity.as_ref(), Some(&key));
        // Click it again → collapses (re-click toggles).
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5, 6), None);
        assert_eq!(app.expanded_activity, None);
    }

    #[test]
    fn run_header_marker_toggles_and_body_expands_only(/* UI-5, Z 2026-07-15 */) {
        let mut app = remote_app();
        let key = seed_hit_as(&mut app, ui::ActivityHitKind::RunHeader { expanded: false });
        // Marker-zone click (col < RUN_MARKER_ZONE) → opens the run.
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1, 6), None);
        assert_eq!(app.expanded_run.as_ref(), Some(&key));
        // Marker click again → closes.
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1, 6), None);
        assert_eq!(app.expanded_run, None);
        // Body click → opens.
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 40, 6), None);
        assert_eq!(app.expanded_run.as_ref(), Some(&key));
        // Body click while open → does NOT collapse (only the marker closes).
        let changed = app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 40, 6), None);
        assert!(!changed, "body click on an open run is a no-op");
        assert_eq!(app.expanded_run.as_ref(), Some(&key));
        // Entry detail state is untouched throughout.
        assert_eq!(app.expanded_activity, None);
    }

    #[test]
    fn click_input_line_opens_modal_and_swallows_mouse_beneath(/* UI-6 item 3 */) {
        let mut app = remote_app();
        // Make the seeded hit the 🔍 input detail line.
        let key = seed_hit_as(&mut app, ui::ActivityHitKind::InputLine);
        // Click it → opens the modal on that key WITHOUT expanding the row.
        let changed = app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 10, 6), None);
        assert!(changed, "opening the modal warrants a redraw");
        assert_eq!(app.input_modal.as_ref().map(|m| &m.key), Some(&key));
        assert_eq!(
            app.expanded_activity, None,
            "the input click does not expand"
        );
        // A click beneath is now swallowed: nothing toggles, the modal stays.
        seed_hit_as(&mut app, ui::ActivityHitKind::Entry);
        let changed = app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 10, 6), None);
        assert!(changed, "the modal consumes the click (redraw)");
        assert_eq!(
            app.expanded_activity, None,
            "no row toggles beneath the modal"
        );
        assert!(app.input_modal.is_some(), "the modal stays open");
    }

    #[test]
    fn input_modal_keys_scroll_and_close(/* UI-6 item 3 */) {
        let mut app = remote_app();
        let key = seed_one_hit(&mut app);
        app.open_input_modal(key);
        // What a frame drew: 40 body rows through a 12-row view. The keys
        // move within it.
        app.input_modal.as_mut().unwrap().state.layout.pos = ScrollPos::of(40, 12, 0);
        app.on_key(press(KeyCode::Down), None);
        assert_eq!(app.input_modal.as_ref().unwrap().state.scroll, 1);
        app.on_key(press(KeyCode::Up), None);
        assert_eq!(app.input_modal.as_ref().unwrap().state.scroll, 0);
        app.on_key(press(KeyCode::PageDown), None);
        assert_eq!(
            app.input_modal.as_ref().unwrap().state.scroll,
            12,
            "a page is the view"
        );
        app.on_key(press(KeyCode::End), None);
        assert_eq!(
            app.input_modal.as_ref().unwrap().state.scroll,
            28,
            "End stops at the last row"
        );
        // Any other key is swallowed — MAIN's activity scroll must not move.
        let before = app.activity_scroll;
        app.on_key(press(KeyCode::Char('x')), None);
        assert_eq!(
            app.activity_scroll, before,
            "keys don't leak beneath the modal"
        );
        assert!(
            app.input_modal.is_some(),
            "an unbound key keeps the modal open"
        );
        // Esc closes.
        app.on_key(press(KeyCode::Esc), None);
        assert!(app.input_modal.is_none(), "esc closes the modal");
    }

    #[test]
    fn raw_modal_keys_tabs_scroll_and_close(/* UI-7 */) {
        let mut app = remote_app();
        let key = seed_one_hit(&mut app);
        // Ready content: a plain (no-upstream) record renders the classic
        // 2 tabs; the upstream pair appears only on translated exchanges.
        let record = crate::proxy::raw_io::RawIoRecord::new(
            7,
            0,
            None,
            None,
            None,
            Some(200),
            b"{}",
            b"{}",
            1024,
            None,
            None,
            None,
        );
        let content = ui::raw_content_from_record(
            ui::RawGeneral {
                lines: Vec::new(),
                method: "POST".into(),
                path: "/v1/messages".into(),
                base_url: "http://localhost:3456".into(),
            },
            &record,
        );
        assert_eq!(content.tabs.len(), 2, "no upstream half → 2 payload tabs");
        app.raw_modal = Some(RawModal {
            key: key.clone(),
            id: 7,
            generation: 1,
            title: "raw".into(),
            tab: 0,
            scroll: 3,
            view: Default::default(),
            spin: 0,
            since: Duration::ZERO,
            flash: None,
            state: RawModalState::Ready(std::sync::Arc::new(content)),
        });
        // Tab advances and resets the scroll (tabs have independent sizes).
        app.on_key(press(KeyCode::Tab), None);
        let modal = app.raw_modal.as_ref().unwrap();
        assert_eq!(modal.tab, 1);
        assert_eq!(modal.scroll, 0);
        // ← wraps back around the 2-tab ring. Long lines wrap at the width,
        // so H/L no longer pan: they are swallowed like any unbound key.
        app.on_key(press(KeyCode::Left), None);
        assert_eq!(app.raw_modal.as_ref().unwrap().tab, 0);
        app.on_key(press(KeyCode::Char('L')), None);
        app.on_key(press(KeyCode::Char('H')), None);
        let modal = app.raw_modal.as_ref().unwrap();
        assert_eq!((modal.tab, modal.scroll), (0, 0), "H/L moved nothing");
        // Scroll keys move; unbound keys are swallowed beneath the modal.
        app.on_key(press(KeyCode::Down), None);
        assert_eq!(app.raw_modal.as_ref().unwrap().scroll, 1);
        let before = app.activity_scroll;
        app.on_key(press(KeyCode::Char('x')), None);
        assert_eq!(app.activity_scroll, before, "keys don't leak beneath");
        assert!(app.raw_modal.is_some());
        // The wheel scrolls it too (and the click is swallowed).
        app.on_mouse(mouse(MouseEventKind::ScrollDown, 10, 6), None);
        assert!(app.raw_modal.as_ref().unwrap().scroll > 1);
        // Esc closes.
        app.on_key(press(KeyCode::Esc), None);
        assert!(app.raw_modal.is_none(), "esc closes the raw viewer");
    }

    #[test]
    fn raw_load_applies_only_to_the_matching_loading_modal(/* UI-7 */) {
        let mut app = remote_app();
        let key = seed_one_hit(&mut app);
        let _ = &key;
        app.raw_modal = Some(RawModal {
            key: key.clone(),
            id: 7,
            generation: 5,
            title: String::new(),
            tab: 0,
            scroll: 0,
            view: Default::default(),
            spin: 0,
            since: Duration::ZERO,
            flash: None,
            state: RawModalState::Loading,
        });
        // A stale delivery for a PRIOR generation is ignored — this is the
        // close→reopen race guard (a late fetch must not clobber the reopen).
        app.apply_raw_load(RawLoad {
            generation: 4,
            result: Err("nope".into()),
        });
        assert!(matches!(
            app.raw_modal.as_ref().unwrap().state,
            RawModalState::Loading
        ));
        // The matching generation resolves it (a miss becomes Failed).
        app.apply_raw_load(RawLoad {
            generation: 5,
            result: Err("miss".into()),
        });
        assert!(matches!(
            app.raw_modal.as_ref().unwrap().state,
            RawModalState::Failed(_)
        ));
    }

    #[test]
    fn clip_result_flashes_only_on_the_requesting_generation(/* UI-8 */) {
        let mut app = remote_app();
        let key = seed_one_hit(&mut app);
        app.raw_modal = Some(RawModal {
            key,
            id: 7,
            generation: 9,
            title: String::new(),
            tab: 0,
            scroll: 0,
            view: Default::default(),
            spin: 0,
            since: Duration::ZERO,
            flash: None,
            state: RawModalState::Loading,
        });
        // A stale export result (older generation) never flashes.
        app.apply_clip_result(ClipResult {
            origin: ClipOrigin::Raw,
            generation: 8,
            message: StatusMsg::ok("stale"),
        });
        assert!(app.raw_modal.as_ref().unwrap().flash.is_none());
        // The requesting generation's result flashes.
        app.apply_clip_result(ClipResult {
            origin: ClipOrigin::Raw,
            generation: 9,
            message: StatusMsg::ok("copied 4 bytes → pbcopy"),
        });
        assert_eq!(
            app.raw_modal.as_ref().unwrap().flash.as_ref().unwrap().0,
            StatusMsg::ok("copied 4 bytes → pbcopy")
        );
    }

    #[test]
    fn open_raw_modal_selects_by_id_under_same_ms_key_collision(/* MUST-FIX */) {
        // Two requests completing in the SAME millisecond with identical
        // method/path/status share an ActivityKey (which omits the id). The
        // opener must fetch the CLICKED row's record, not the first match.
        let req = |id: u64| activity::Completed {
            at: SystemTime::UNIX_EPOCH + Duration::from_millis(42),
            body: activity::CompletedBody::Request {
                id,
                tenant: None,
                client_name: None,
                method: "POST".into(),
                path: "/v1/messages".into(),
                endpoint: crate::tui::Endpoint::Anthropic,
                account: Some("claude:a@x".into()),
                status: 200,
                duration: Duration::from_millis(10),
                tokens: None,
                group: Some("claude".into()),
                model: Some("claude-opus-4-8".into()),
                effort: None,
                fast: Some(false),
                ttfb_ms: None,
                ttft_ms: None,
                gen_ms: None,
                aborted: false,
                user_id: None,
                kind: Some("user".into()),
                excerpt: None,
            },
        };
        let first = req(7);
        let second = req(8);
        let key = first.activity_key().unwrap();
        assert_eq!(key, second.activity_key().unwrap(), "keys collide");
        let mut view = empty_view();
        view.completed = vec![first, second];

        // Click resolved to the SECOND row's id → the modal must open on id 8,
        // not the first-match id 7.
        let mut app = remote_app();
        app.open_raw_modal(key, 8, Some(&view));
        assert_eq!(
            app.raw_modal.as_ref().unwrap().id,
            8,
            "opener pinned the clicked row via id, not the first key match"
        );
    }

    #[test]
    fn burst_of_exports_all_queue_none_dropped(/* MUST-FIX */) {
        // Two export actions before the event loop drains the queue must both
        // run — the async path used to be a single slot and silently dropped
        // the first (a regression from synchronous execution).
        let mut app = remote_app();
        let key = seed_one_hit(&mut app);
        let content = ui::RawContent {
            tabs: vec![ui::RawTabContent {
                label: "Request",
                lines: Vec::new(),
                long_lines: false,
                body_text: "body".into(),
                curl: "curl".into(),
            }],
            record_json: "{}".into(),
            all_text: "all".into(),
        };
        app.raw_modal = Some(RawModal {
            key,
            id: 7,
            generation: 1,
            title: String::new(),
            tab: 0,
            scroll: 0,
            view: Default::default(),
            spin: 0,
            since: Duration::ZERO,
            flash: None,
            state: RawModalState::Ready(std::sync::Arc::new(content)),
        });
        app.raw_modal_action(ui::RawButton::Copy);
        app.raw_modal_action(ui::RawButton::Save);
        assert_eq!(
            app.pending_clip.len(),
            2,
            "both queued — the second must not overwrite the first"
        );
        // Single-flight FIFO: the first dispatch pops Copy and marks the slot
        // busy; a second dispatch attempt while busy yields nothing (no
        // concurrent clipboard write / no last-writer race).
        assert!(matches!(
            app.next_clip_if_idle().map(|r| r.button),
            Some(ui::RawButton::Copy)
        ));
        assert!(app.clip_inflight, "slot busy after dispatch");
        assert!(
            app.next_clip_if_idle().is_none(),
            "single-flight: no second dispatch while one is in flight"
        );
        // Its result frees the slot → the next (Save) dispatches, in order.
        app.clip_finished();
        assert!(matches!(
            app.next_clip_if_idle().map(|r| r.button),
            Some(ui::RawButton::Save)
        ));
        app.clip_finished();
        assert!(app.next_clip_if_idle().is_none());
    }

    #[test]
    fn export_queue_is_bounded_and_rejects_overflow_with_feedback(/* MUST-FIX */) {
        // A spamming user against a wedged exporter must not grow the queue
        // without limit: past the cap the newest is rejected with feedback
        // (already-queued actions are preserved, growth is bounded).
        let mut app = remote_app();
        let key = seed_one_hit(&mut app);
        let content = ui::RawContent {
            tabs: vec![ui::RawTabContent {
                label: "Request",
                lines: Vec::new(),
                long_lines: false,
                body_text: "body".into(),
                curl: "curl".into(),
            }],
            record_json: "{}".into(),
            all_text: "all".into(),
        };
        app.raw_modal = Some(RawModal {
            key,
            id: 7,
            generation: 1,
            title: String::new(),
            tab: 0,
            scroll: 0,
            view: Default::default(),
            spin: 0,
            since: Duration::ZERO,
            flash: None,
            state: RawModalState::Ready(std::sync::Arc::new(content)),
        });
        for _ in 0..(CLIP_QUEUE_MAX + 5) {
            app.raw_modal_action(ui::RawButton::Copy);
        }
        assert_eq!(
            app.pending_clip.len(),
            CLIP_QUEUE_MAX,
            "queue capped, never grows past CLIP_QUEUE_MAX"
        );
        assert_eq!(
            app.raw_modal.as_ref().unwrap().flash.as_ref().unwrap().0,
            StatusMsg::warn("export busy — try again in a moment"),
            "overflow rejected with feedback, not silently dropped"
        );
    }

    #[test]
    fn history_hydration_refused_for_cross_host_attach(/* review M1 */) {
        // Loopback attach (the standard `llmux` → localhost:3456 topology)
        // shares this machine's state file → allowed.
        for url in [
            "http://localhost:3456",
            "http://127.0.0.1:3456",
            "http://[::1]:3456",
            "http://127.9.9.9:3456/path",
        ] {
            assert!(base_url_is_loopback(url), "{url} is this machine");
        }
        // A daemon on another host has a DIFFERENT activity.jsonl — splicing
        // the local file under its live rows would show wrong data.
        for url in [
            "http://devbox-b:3456",
            "http://192.0.2.10:3456",
            "http://[2001:db8::1]:3456",
            "not a url",
        ] {
            assert!(!base_url_is_loopback(url), "{url} is another host");
        }
        // And the App-level gate wires it up: a cross-host remote never arms.
        let mut app = remote_app();
        if let Backend::Remote(remote) = &mut app.backend {
            remote.base_url = "http://devbox-b:3456".into();
        }
        app.request_history();
        assert!(!app.history_loading, "cross-host attach must not hydrate");
    }

    #[test]
    fn extend_completed_with_history_pages_older_rows(/* UI-5 infinite scroll */) {
        let note = |secs: u64| activity::Completed {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            body: activity::CompletedBody::Note {
                text: format!("n{secs}"),
                error: false,
            },
        };
        // Live window: newest-first 100..91; history overlaps (100..91) then
        // continues older (90..1).
        let live: Vec<_> = (91..=100).rev().map(note).collect();
        let history: Vec<_> = (1..=100).rev().map(note).collect();

        // Nothing materialized (`take == 0`) → nothing appended.
        let mut completed = live.clone();
        extend_completed_with_history(&mut completed, &history, 0);
        assert_eq!(completed.len(), live.len());

        // take = 5 appends exactly the 5 NEWEST strictly-older entries (the
        // overlap dedupes by timestamp cut).
        let mut completed = live.clone();
        extend_completed_with_history(&mut completed, &history, 5);
        assert_eq!(completed.len(), live.len() + 5);
        assert_eq!(
            completed[live.len()].at,
            SystemTime::UNIX_EPOCH + Duration::from_secs(90),
            "first appended row is the newest strictly-older history entry"
        );
        // No duplicates across the seam.
        let mut seen = std::collections::HashSet::new();
        assert!(
            completed.iter().all(|c| seen.insert(c.at)),
            "live+history merge must not duplicate the overlap"
        );
    }

    #[test]
    fn folded_wall_deadlock_is_broken_on_history_arrival(/* review R3-1 */) {
        // 350 raw `count` entries sharing one fold key collapse to ONE render
        // row, so the scroll ceiling is 0 and `activity_scroll` can never
        // leave 0 on its own. The arrival-time `grow_history_take` must
        // materialize the first history page in that exact state — otherwise
        // loaded history stays permanently unreachable (the R2 regression
        // test injected an unreachable scroll=5 and proved nothing).
        let count_req = |secs: u64| activity::Completed {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            body: activity::CompletedBody::Request {
                id: 1,
                method: "POST".into(),
                path: "/v1/messages/count_tokens".into(),
                endpoint: crate::tui::Endpoint::Anthropic,
                account: Some("claude:a@x".into()),
                status: 200,
                duration: Duration::from_millis(10),
                tokens: None,
                group: Some("claude".into()),
                model: Some("claude-fable-5".into()),
                effort: None,
                fast: Some(false),
                ttfb_ms: None,
                ttft_ms: None,
                gen_ms: None,
                aborted: false,
                user_id: None,
                kind: Some("count".into()),
                excerpt: None,
                tenant: None,
                client_name: None,
            },
        };
        let note = |secs: u64| activity::Completed {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            body: activity::CompletedBody::Note {
                text: format!("n{secs}"),
                error: false,
            },
        };
        let live: Vec<_> = (1_000..1_350).rev().map(count_req).collect();
        assert_eq!(
            triage::collapse_completed(&live).len(),
            1,
            "precondition: the live wall folds to one render row"
        );
        let history: Vec<_> = (1..=50).rev().map(note).collect();

        let mut app = remote_app();
        app.history_completed = Some(history.clone());
        let mut view = empty_view();
        view.completed = live.clone();
        // Arrival-time materialization at scroll == 0 (the deadlock state).
        assert_eq!(app.activity_scroll, 0);
        app.grow_history_take(&view);
        assert!(app.history_take > 0, "first page materialized at scroll 0");

        // The frame path now extends the view and the scroll ceiling rises.
        extend_completed_with_history(&mut view.completed, &history, app.history_take);
        assert!(
            triage::collapse_completed(&view.completed).len() > 1,
            "history rows are reachable — the folded wall no longer pins the ceiling"
        );
        // And a wheel-down actually moves now.
        app.scroll_activity(1, Some(&view));
        assert_eq!(app.activity_scroll, 1, "scroll escapes 0 after hydration");
    }

    #[test]
    fn click_off_a_row_does_nothing() {
        let mut app = remote_app();
        seed_one_hit(&mut app);
        // Row 9 has no hit target; expansion is untouched and no redraw.
        let changed = app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5, 9), None);
        assert!(!changed);
        assert_eq!(app.expanded_activity, None);
    }

    #[test]
    fn mouse_is_ignored_while_an_overlay_owns_the_screen() {
        let mut app = remote_app();
        seed_one_hit(&mut app);
        app.overlay = Overlay::Stats;
        let changed = app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5, 6), None);
        assert!(!changed, "overlay swallows the mouse");
        assert_eq!(app.expanded_activity, None, "no hidden row toggled");
    }

    #[test]
    fn wheel_scrolls_the_activity_history() {
        let mut app = remote_app();
        seed_one_hit(&mut app);
        let mut view = empty_view();
        // Give the view a few completed entries so the scroll offset can move.
        view.completed = (0..5)
            .map(|i| activity::Completed {
                at: SystemTime::UNIX_EPOCH + Duration::from_secs(i),
                body: activity::CompletedBody::Note {
                    text: format!("n{i}"),
                    error: false,
                },
            })
            .collect();
        assert_eq!(app.activity_scroll, 0);
        app.on_mouse(mouse(MouseEventKind::ScrollUp, 5, 6), Some(&view));
        assert_eq!(app.activity_scroll, 1, "wheel up scrolls into history");
        app.on_mouse(mouse(MouseEventKind::ScrollDown, 5, 6), Some(&view));
        assert_eq!(app.activity_scroll, 0, "wheel down returns to the tail");
    }

    /// UI-6 item 4: while scrolled into history (`scroll > 0`), a freshly
    /// arrived completed entry prepends (newest-first) and would slide the page
    /// being read down. The offset must auto-bump by the prepended render-row
    /// count so the row under the cursor stays put — including AT RING CAPACITY,
    /// where each append evicts the oldest and the folded-row COUNT never
    /// changes (a length delta would be dead there). At `scroll == 0` it must
    /// NOT bump (live-tail is preserved).
    #[test]
    fn new_activity_does_not_shift_a_scrolled_viewport() {
        let req = |secs: u64| activity::Completed {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            body: activity::CompletedBody::Request {
                id: 1,
                method: "POST".into(),
                path: "/v1/messages".into(),
                endpoint: crate::tui::Endpoint::Anthropic,
                account: Some("claude:a@x".into()),
                status: 200,
                duration: Duration::from_millis(10),
                tokens: None,
                group: Some("claude".into()),
                model: Some("claude-opus-4-8".into()),
                effort: None,
                fast: Some(false),
                ttfb_ms: None,
                ttft_ms: None,
                gen_ms: None,
                aborted: false,
                user_id: None,
                // Non-`count` kind → every entry renders 1:1 (no folding), so
                // render row k maps to `completed[k]`.
                kind: Some("user".into()),
                excerpt: None,
                tenant: None,
                client_name: None,
            },
        };
        let mut app = remote_app();
        let mut view = empty_view();
        // Newest-first: secs 10 down to 1 → 10 distinct render rows.
        view.completed = (1..=10).rev().map(req).collect();
        assert_eq!(
            triage::collapse_completed(&view.completed).len(),
            10,
            "precondition: 10 distinct render rows"
        );

        // --- Below capacity: append GROWS the list. ------------------------
        // Operator scrolls 3 rows in; this frame records the newest-key anchor.
        app.activity_scroll = 3;
        app.preserve_scroll_on_new_activity(&view);
        // The row currently at the top of the visible page.
        let anchored_key = view.completed[3].activity_key();
        // A new request lands (prepends as the newest row) and the next frame
        // observes it: the offset bumps by the one prepended render row.
        view.completed.insert(0, req(11));
        app.preserve_scroll_on_new_activity(&view);
        assert_eq!(
            app.activity_scroll, 4,
            "below capacity: offset bumped by one row"
        );
        assert_eq!(
            view.completed[app.activity_scroll].activity_key(),
            anchored_key,
            "below capacity: the row under the cursor stayed in place"
        );

        // --- At ring capacity: append EVICTS the oldest, len stays FLAT. ----
        app.activity_scroll = 3;
        app.preserve_scroll_on_new_activity(&view);
        let anchored_key = view.completed[3].activity_key();
        let before_len = triage::collapse_completed(&view.completed).len();
        view.completed.insert(0, req(100)); // newest arrives
        view.completed.pop(); // oldest evicted (ring full)
        assert_eq!(
            triage::collapse_completed(&view.completed).len(),
            before_len,
            "precondition: at capacity the folded render-row count is flat"
        );
        app.preserve_scroll_on_new_activity(&view);
        assert_eq!(
            app.activity_scroll, 4,
            "at capacity: key anchor bumps offset even though len is flat"
        );
        assert_eq!(
            view.completed[app.activity_scroll].activity_key(),
            anchored_key,
            "at capacity: the row under the cursor stayed in place"
        );

        // --- Live tail (scroll == 0) never bumps. --------------------------
        app.activity_scroll = 0;
        app.preserve_scroll_on_new_activity(&view);
        view.completed.insert(0, req(200));
        view.completed.pop();
        app.preserve_scroll_on_new_activity(&view);
        assert_eq!(app.activity_scroll, 0, "live tail is not bumped");
    }

    /// UI-6 item 4 regression: a Note occupies its own render row but has no
    /// key, so the newest KEYED request sits at row ≥1. An ABSOLUTE-index bump
    /// would add that offset on every idle redraw tick (no new arrivals) →
    /// runaway to the ceiling in ~1-2s. The DELTA anchor must hold the offset
    /// steady with no arrivals, then bump by exactly the prepended-row count
    /// when a real request lands above the note.
    #[test]
    fn leading_note_does_not_runaway_a_scrolled_viewport() {
        let note = |secs: u64| activity::Completed {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            body: activity::CompletedBody::Note {
                text: format!("n{secs}"),
                error: false,
            },
        };
        let req = |secs: u64| activity::Completed {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            body: activity::CompletedBody::Request {
                id: 1,
                method: "POST".into(),
                path: "/v1/messages".into(),
                endpoint: crate::tui::Endpoint::Anthropic,
                account: Some("claude:a@x".into()),
                status: 200,
                duration: Duration::from_millis(10),
                tokens: None,
                group: Some("claude".into()),
                model: Some("claude-opus-4-8".into()),
                effort: None,
                fast: Some(false),
                ttfb_ms: None,
                ttft_ms: None,
                gen_ms: None,
                aborted: false,
                user_id: None,
                kind: Some("user".into()),
                excerpt: None,
                tenant: None,
                client_name: None,
            },
        };
        let top_key =
            |v: &DashboardView, row: usize| match triage::collapse_completed(&v.completed)[row] {
                triage::ActivityRow::Single(i) => v.completed[i].activity_key(),
                triage::ActivityRow::Run { start, .. } => v.completed[start].activity_key(),
            };

        let mut app = remote_app();
        let mut view = empty_view();
        // Newest-first: a Note on top (render row 0, no key), then 9 requests →
        // 10 render rows. The newest KEYED request is at render row 1.
        view.completed = std::iter::once(note(100))
            .chain((1..=9).rev().map(req))
            .collect();
        app.activity_scroll = 3;

        // Many idle redraw ticks, NO new arrivals: the offset must NOT drift
        // (this is exactly what the absolute-index bump got wrong).
        for _ in 0..5 {
            app.preserve_scroll_on_new_activity(&view);
            assert_eq!(
                app.activity_scroll, 3,
                "idle ticks with a leading note must not drift the offset"
            );
        }
        let anchored_key = top_key(&view, 3);

        // A real request now lands as the newest entry (the note is pushed down
        // to render row 1): exactly one new render row prepended → offset += 1.
        view.completed.insert(0, req(200));
        app.preserve_scroll_on_new_activity(&view);
        assert_eq!(
            app.activity_scroll, 4,
            "note-then-request arrival bumps by exactly the one new render row"
        );
        assert_eq!(
            top_key(&view, app.activity_scroll),
            anchored_key,
            "the row under the cursor stayed in place"
        );
    }

    fn stats_view() -> DashboardView {
        let mut v = empty_view();
        v.model_usage = vec![crate::dashboard::ModelUsageDoc {
            group: "codex".into(),
            model: "gpt-5.5".into(),
            requests: 1,
            ok: 1,
            errors: 0,
            tokens_in: 10,
            tokens_out: 5,
            cache_read: None,
            cache_creation: None,
            last_used_ms: 0,
            in_flight: 0,
            accounts: Vec::new(),
            efforts: Vec::new(),
            endpoints: Vec::new(),
            cost_usd: 0.0,
        }];
        v
    }

    fn stats_view_with_account() -> DashboardView {
        use crate::routing::BackendGroup;
        use crate::scheduler::{AccountId, AccountSnapshot};
        let mut v = stats_view();
        v.snapshot.accounts = vec![AccountSnapshot {
            id: AccountId("claude:me@example.com".into()),
            healthy: true,
            auth_failure_status: None,
            credential_kind: "oauth",
            group: BackendGroup::Claude,
            five_hour: None,
            seven_day: None,
            scoped_limits: Vec::new(),
            scoped_cooldowns: Vec::new(),
            cooldown_until: None,
            cooldown_source: None,
            in_flight: 0,
            token_expires_at_ms: None,
            last_refresh_ms: None,
            paused: false,
            limits: crate::config::AccountLimits::default(),
        }];
        v
    }

    fn empty_view() -> DashboardView {
        use crate::scheduler::PoolSnapshot;
        DashboardView {
            version: "llmux 0.0 (test)".into(),
            grok: Default::default(),
            daily_usage: Vec::new(),
            daily_perf: Vec::new(),
            config_facts: Default::default(),
            usage_stats: Vec::new(),
            usage_controls: Default::default(),
            plans: Default::default(),
            auth_history: Default::default(),
            health: Default::default(),
            session_labels: Default::default(),
            pid: 1,
            uptime: Duration::from_secs(1),
            port: 3456,
            upstream: None,
            config_path: None,
            select_params: crate::scheduler::select::SelectParams {
                five_hour_max: 0.9,
                seven_day_max: 0.99,
                fable_weekly_max: 0.98,
                mode: crate::config::SchedulerMode::Default,
                usage_max_age: Duration::from_secs(600),
            },
            refresh_ahead: Duration::from_secs(25_200),
            evaluate_tick: Duration::from_secs(60),
            snapshot: PoolSnapshot {
                accounts: Vec::new(),
                current: std::collections::BTreeMap::new(),
                fable_current: std::collections::BTreeMap::new(),
                manual_pin: Default::default(),
            },
            last_switch: None,
            poll_health: std::collections::HashMap::new(),
            session_totals: std::collections::HashMap::new(),
            global_totals: activity::Totals::default(),
            rpm_5m: 0.0,
            in_flight: Vec::new(),
            completed: Vec::new(),
            logs: Vec::new(),
            model_usage: Vec::new(),
            client_usage: Vec::new(),
            client_keys: Vec::new(),
            windowed: Vec::new(),
            codex: crate::dashboard::CodexSettingsDoc::default(),
            email_anonymous: false,
            tui_effects: true,
            gradient: ui::GradientCfg::default(),
            show_fable_weekly: true,
            domain_abbrev: crate::config::default_domain_abbrev(),
            quota_display: crate::config::QuotaDisplay::default(),
            data_quality: crate::dashboard::DataQualityDoc::default(),
            events: Vec::new(),
        }
    }

    // --- codex usage controls (.prd/16) ------------------------------------

    /// A roster with one codex account (which HAS resets) and one claude
    /// account (which does not), plus the codex account selected — so every
    /// assertion below has a negative control.
    fn control_view() -> DashboardView {
        use crate::routing::BackendGroup;
        use crate::scheduler::{AccountId, AccountSnapshot};
        let account = |name: &str, kind: &'static str, group| AccountSnapshot {
            id: AccountId(name.into()),
            healthy: true,
            auth_failure_status: None,
            credential_kind: kind,
            group,
            five_hour: None,
            seven_day: None,
            scoped_limits: Vec::new(),
            scoped_cooldowns: Vec::new(),
            cooldown_until: None,
            cooldown_source: None,
            in_flight: 0,
            token_expires_at_ms: None,
            last_refresh_ms: None,
            paused: false,
            limits: crate::config::AccountLimits::default(),
        };
        let mut view = empty_view();
        view.snapshot.accounts = vec![
            account("claude:a@x.com", "oauth", BackendGroup::Claude),
            account("codex:c@x.com", "codex", BackendGroup::Codex),
        ];
        view
    }

    fn control_doc(
        available: Option<u64>,
        last_refresh_ms: Option<u64>,
    ) -> crate::proxy::usage_controls::UsageControlDoc {
        crate::proxy::usage_controls::UsageControlDoc {
            available_resets: available,
            applicable_resets: Some(0),
            last_refresh_ms,
            ..Default::default()
        }
    }

    /// `f` queues exactly ONE refresh, and a second press while it runs is
    /// refused — the anti-duplicate gate that also protects the redemption.
    #[test]
    fn accounts_f_queues_one_refresh_and_refuses_a_second_while_it_runs() {
        let view = control_view();
        let mut app = remote_app();
        app.overlay = Overlay::Accounts;
        app.on_key_accounts(KeyCode::Char('f'), Some(&view));
        assert_eq!(
            app.pending_control,
            Some(ControlOp::Refresh { account: None })
        );
        // The loop drains it and marks it in flight.
        let op = app.take_pending_control().expect("queued");
        app.control_inflight = true;
        assert_eq!(op, ControlOp::Refresh { account: None });

        app.on_key_accounts(KeyCode::Char('f'), Some(&view));
        assert_eq!(app.pending_control, None, "no second refresh while running");
        assert!(
            app.status_line().is_some_and(|s| s.contains("already")),
            "{:?}",
            app.status_line()
        );
    }

    /// `f` in the switcher refreshes the HIGHLIGHTED account (display order),
    /// not the whole roster.
    #[test]
    fn switcher_f_refreshes_the_highlighted_account() {
        let view = control_view();
        let mut app = remote_app();
        app.overlay = Overlay::Accounts;
        let codex_pos = view
            .display_order(Default::default(), SystemTime::now())
            .iter()
            .position(|&i| view.snapshot.accounts[i].credential_kind == "codex")
            .expect("codex row");
        app.mode = Mode::Select { idx: codex_pos };
        app.on_key_select(KeyCode::Char('f'), codex_pos, Some(&view));
        assert_eq!(
            app.pending_control,
            Some(ControlOp::Refresh {
                account: Some("codex:c@x.com".into())
            })
        );
        assert_eq!(
            app.mode,
            Mode::Select { idx: codex_pos },
            "stays in the switcher"
        );
    }

    /// Entering the accounts surface refreshes ONCE when the reset inventory
    /// is absent or stale — and NOT when it was read seconds ago (the floor),
    /// nor at all when no account has resets to count.
    #[test]
    fn accounts_entry_refresh_respects_the_staleness_floor() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ms = |secs: u64| secs * 1_000;

        // No account with reset entitlements (codex OR claude oauth) →
        // nothing to count → never refreshes on entry.
        let mut none_with_resets = control_view();
        none_with_resets
            .snapshot
            .accounts
            .retain(|a| !view::has_usage_controls(a.credential_kind));
        assert!(!accounts_entry_refresh_due(&none_with_resets, now));
        // A claude-only roster DOES count now: its grants are read on entry.
        let mut claude_only = control_view();
        claude_only
            .snapshot
            .accounts
            .retain(|a| a.credential_kind != "codex");
        assert!(accounts_entry_refresh_due(&claude_only, now));

        // Codex account with no observation at all → due.
        let mut view = control_view();
        assert!(accounts_entry_refresh_due(&view, now));

        // Observed 10s ago (both reset-bearing accounts) → NOT due (entry
        // must not hammer upstream).
        view.usage_controls.insert(
            "claude:a@x.com".into(),
            control_doc(Some(1), Some(ms(999_990))),
        );
        view.usage_controls.insert(
            "codex:c@x.com".into(),
            control_doc(Some(3), Some(ms(999_990))),
        );
        assert!(!accounts_entry_refresh_due(&view, now));

        // Observed 61s ago → due again.
        view.usage_controls.insert(
            "codex:c@x.com".into(),
            control_doc(Some(3), Some(ms(999_939))),
        );
        assert!(accounts_entry_refresh_due(&view, now));

        // A count with no timestamp is an unanchored observation → due.
        view.usage_controls
            .insert("codex:c@x.com".into(), control_doc(Some(3), None));
        assert!(accounts_entry_refresh_due(&view, now));
    }

    /// Both accounts entry points (the `a` key and a tab/mouse `open_tab`)
    /// queue the entry refresh — and exactly one, not one per press while it
    /// is still running.
    #[test]
    fn both_accounts_entry_points_queue_the_entry_refresh() {
        let view = control_view();
        let mut app = remote_app();
        app.on_key_main(KeyCode::Char('a'), Some(&view));
        assert_eq!(app.overlay, Overlay::Accounts);
        assert_eq!(
            app.pending_control,
            Some(ControlOp::Refresh { account: None }),
            "`a` entry requests the counts"
        );

        let mut app = remote_app();
        app.open_tab(Overlay::Accounts, Some(&view));
        assert_eq!(app.overlay, Overlay::Accounts);
        assert_eq!(
            app.pending_control,
            Some(ControlOp::Refresh { account: None }),
            "tab/mouse entry requests the same"
        );
    }

    /// The manual switch no longer refuses on CACHED state: the account whose
    /// quota reads 100% (exactly the stale-window case an upstream reset
    /// fixes) still queues, and so does the ALREADY-ACTIVE account — which is
    /// how the operator forces a fresh read of it.
    #[test]
    fn manual_switch_queues_for_exhausted_and_already_active_targets() {
        use crate::scheduler::window::{QuotaWindow, WindowSource};
        let mut view = control_view();
        let now = SystemTime::now();
        // Cached 100% on the codex account — the pre-.prd/16 client refused
        // this switch outright.
        view.snapshot.accounts[1].five_hour = Some(QuotaWindow {
            utilization: 1.0,
            resets_at: now + Duration::from_secs(3_600),
            fetched_at: now,
            source: WindowSource::UsagePoll,
        });
        let order = view.display_order(Default::default(), now);
        let codex_pos = order
            .iter()
            .position(|&i| view.snapshot.accounts[i].credential_kind == "codex")
            .expect("codex row");

        let mut app = remote_app();
        app.try_manual_switch(codex_pos, Some(&view));
        assert_eq!(
            app.pending_control,
            Some(ControlOp::Switch {
                account: "codex:c@x.com".into()
            }),
            "an exhausted cached window must not block the switch"
        );

        // Already active: still queued (refresh-then-reselect), with a status
        // that says so rather than "already active".
        view.snapshot.current.insert(
            crate::routing::BackendGroup::Codex,
            crate::scheduler::AccountId("codex:c@x.com".into()),
        );
        let order = view.display_order(Default::default(), now);
        let codex_pos = order
            .iter()
            .position(|&i| view.snapshot.accounts[i].credential_kind == "codex")
            .expect("codex row");
        let mut app = remote_app();
        app.try_manual_switch(codex_pos, Some(&view));
        assert_eq!(
            app.pending_control,
            Some(ControlOp::Switch {
                account: "codex:c@x.com".into()
            }),
            "re-selecting the active account refreshes it"
        );
        assert!(
            app.status_line().is_some_and(|s| s.contains("refreshing")),
            "{:?}",
            app.status_line()
        );
    }

    /// `R` opens the gate on the first row whose provider HAS resets (never
    /// an api-key / grok one), arrows move it onto the codex row, the prompt
    /// names that account and one reset, and cancelling queues nothing.
    #[test]
    fn reset_gate_targets_a_codex_account_and_cancel_queues_nothing() {
        let mut view = control_view();
        view.usage_controls
            .insert("codex:c@x.com".into(), control_doc(Some(3), None));
        let mut app = remote_app();
        app.overlay = Overlay::Accounts;
        app.on_key_accounts(KeyCode::Char('R'), Some(&view));
        let Mode::ConfirmReset { idx } = app.mode else {
            panic!("expected the reset gate, got {:?}", app.mode);
        };
        let order = view.display_order(Default::default(), SystemTime::now());
        assert!(
            view::has_usage_controls(view.snapshot.accounts[order[idx]].credential_kind),
            "the gate opens on an account that HAS resets"
        );
        // Walk the cursor onto the codex row.
        let codex_pos = order
            .iter()
            .position(|&i| view.snapshot.accounts[i].credential_kind == "codex")
            .expect("codex row");
        let mut idx = idx;
        while idx != codex_pos {
            let key = if idx < codex_pos {
                KeyCode::Down
            } else {
                KeyCode::Up
            };
            app.on_key_confirm_reset(key, idx, Some(&view));
            let Mode::ConfirmReset { idx: moved } = app.mode else {
                panic!("gate closed while moving: {:?}", app.mode);
            };
            idx = moved;
        }
        let status = app.status_line().expect("prompt").to_string();
        assert!(status.contains("codex:c@x.com"), "{status}");
        assert!(status.contains("ONE"), "{status}");
        assert!(status.contains("3 owned"), "{status}");

        // Esc cancels: nothing queued, nothing held.
        app.on_key_confirm_reset(KeyCode::Esc, idx, Some(&view));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.pending_control, None, "a cancelled gate spends nothing");
        assert!(app.pending_redemption.is_none());
    }

    /// Only `1` / `y` spend a reset: Enter (the marked "Cancel"), `2`, Tab
    /// and any other key close the gate with nothing queued (review of #233
    /// — the remove gate had this pinned by key, the reset gate only by text).
    #[test]
    fn reset_gate_spends_only_on_1_or_y() {
        let mut view = control_view();
        view.usage_controls
            .insert("codex:c@x.com".into(), control_doc(Some(3), None));
        let open = |app: &mut App| {
            app.mode = Mode::Normal;
            app.overlay = Overlay::Accounts;
            app.on_key_accounts(KeyCode::Char('R'), Some(&view));
            let Mode::ConfirmReset { idx } = app.mode else {
                panic!("expected the reset gate, got {:?}", app.mode);
            };
            idx
        };
        for cancel in [
            KeyCode::Enter,
            KeyCode::Char('2'),
            KeyCode::Char('n'),
            KeyCode::Tab,
            KeyCode::Char('q'),
            KeyCode::Esc,
        ] {
            let mut app = remote_app();
            let idx = open(&mut app);
            app.on_key_confirm_reset(cancel, idx, Some(&view));
            assert_eq!(app.mode, Mode::Normal, "{cancel:?} closes the gate");
            assert_eq!(app.pending_control, None, "{cancel:?} spends nothing");
            assert!(app.pending_redemption.is_none(), "{cancel:?} holds nothing");
        }
        for confirm in [KeyCode::Char('1'), KeyCode::Char('y')] {
            let mut app = remote_app();
            let idx = open(&mut app);
            app.on_key_confirm_reset(confirm, idx, Some(&view));
            assert!(
                app.pending_control.is_some() || app.pending_redemption.is_some(),
                "{confirm:?} spends one reset"
            );
        }
    }

    /// A roster with no codex OR claude account cannot open the gate at all;
    /// a claude-only roster can (its grants are a reset entitlement too).
    #[test]
    fn reset_gate_refuses_without_a_codex_account() {
        let mut view = control_view();
        view.snapshot
            .accounts
            .retain(|a| !view::has_usage_controls(a.credential_kind));
        let mut app = remote_app();
        app.overlay = Overlay::Accounts;
        app.on_key_accounts(KeyCode::Char('R'), Some(&view));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.pending_control, None);
        assert!(app
            .status_line()
            .is_some_and(|s| s.contains("no codex or claude account")));

        let mut claude_only = control_view();
        claude_only
            .snapshot
            .accounts
            .retain(|a| a.credential_kind == "oauth");
        let mut app = remote_app();
        app.overlay = Overlay::Accounts;
        app.on_key_accounts(KeyCode::Char('R'), Some(&claude_only));
        assert!(matches!(app.mode, Mode::ConfirmReset { .. }));
        assert!(app
            .status_line()
            .is_some_and(|s| s.contains("claude:a@x.com") && s.contains("ONE")));
    }

    /// The confirm resolves the cursor to an account at CONFIRM time through
    /// the display order, so a roster that reordered between opening the gate
    /// and pressing `y` cannot redirect the redemption to another account.
    #[test]
    fn reset_confirm_follows_the_row_under_the_cursor_after_a_reorder() {
        let mut view = control_view();
        // A SECOND codex account, so the codex block can actually reorder
        // (the backend-group blocks themselves never move).
        let mut spare = view.snapshot.accounts[1].clone();
        spare.id = crate::scheduler::AccountId("codex:z@x.com".into());
        view.snapshot.accounts.push(spare);
        let mut app = remote_app();
        // `next` is the mode where a state change reorders rows.
        app.account_sort = triage::AccountSort::Next;
        app.overlay = Overlay::Accounts;
        app.on_key_accounts(KeyCode::Char('R'), Some(&view));
        let Mode::ConfirmReset { idx } = app.mode else {
            panic!("expected the reset gate");
        };
        // The roster reorders: the account the gate opened on goes
        // auth-broken and sinks below its sibling, so row `idx` now holds a
        // DIFFERENT account.
        view.snapshot.accounts[1].healthy = false;
        let order = view.display_order(app.account_sort, SystemTime::now());
        assert_ne!(
            view.snapshot.accounts[order[idx]].id.0, "codex:c@x.com",
            "the state change really reordered the codex block"
        );
        // Move the cursor onto the codex row and confirm.
        let codex_pos = order
            .iter()
            .position(|&i| view.snapshot.accounts[i].id.0 == "codex:c@x.com")
            .expect("codex row");
        app.on_key_confirm_reset(KeyCode::Char('y'), codex_pos, Some(&view));
        match app.pending_control.as_ref().expect("queued redemption") {
            ControlOp::Reset { account, .. } => assert_eq!(account, "codex:c@x.com"),
            other => panic!("expected a redemption, got {other:?}"),
        }
        // Confirming on the CLAUDE row redeems THAT account's grant (Claude
        // oauth accounts carry reset grants since 2026-10-06) — and nothing
        // else: the queued redemption names the claude account.
        let mut app = remote_app();
        app.account_sort = triage::AccountSort::Next;
        let claude_pos = order
            .iter()
            .position(|&i| view.snapshot.accounts[i].credential_kind == "oauth")
            .expect("claude row");
        app.on_key_confirm_reset(KeyCode::Char('y'), claude_pos, Some(&view));
        match app
            .pending_control
            .as_ref()
            .expect("queued claude redemption")
        {
            ControlOp::Reset { account, .. } => assert_eq!(account, "claude:a@x.com"),
            other => panic!("expected a redemption, got {other:?}"),
        }
    }

    /// Render the app's current frame as one string (the runtime's own draw
    /// call, on a test backend).
    fn render_app(app: &App, view: &DashboardView, w: u16, h: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).expect("terminal");
        let chrome = app.chrome();
        let mut hits = None;
        terminal
            .draw(|frame| ui::draw(frame, Some(view), &chrome, &mut hits))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Age the status line past its TTL, as five seconds of waiting would.
    fn expire_status(app: &mut App) {
        if let Some((_, at)) = app.status.as_mut() {
            *at = Instant::now()
                .checked_sub(STATUS_TTL + Duration::from_secs(1))
                .expect("monotonic clock far enough from its origin");
        }
        assert_eq!(app.status_line(), None, "the status line expired");
    }

    /// The reset gate names its account, the inventory and the numbered
    /// choices for as long as it is open — after the five-second status line
    /// is gone too (the bug: the footer then read a bare `redeem ONE reset?`).
    #[test]
    fn reset_gate_names_its_account_after_the_status_line_expires() {
        let mut view = control_view();
        view.usage_controls
            .insert("codex:c@x.com".into(), control_doc(Some(3), None));
        let mut app = remote_app();
        app.overlay = Overlay::Accounts;
        app.on_key_accounts(KeyCode::Char('R'), Some(&view));
        let order = view.display_order(Default::default(), SystemTime::now());
        let codex_pos = order
            .iter()
            .position(|&i| view.snapshot.accounts[i].id.0 == "codex:c@x.com")
            .expect("codex row");
        app.mode = Mode::ConfirmReset { idx: codex_pos };
        expire_status(&mut app);
        for (w, h) in [(80, 24), (179, 43)] {
            let frame = render_app(&app, &view, w, h);
            assert!(frame.contains("codex:c@x.com"), "{frame}");
            assert!(frame.contains("Redeem ONE rate-limit reset"), "{frame}");
            assert!(frame.contains("3 owned"), "{frame}");
            assert!(frame.contains("1) Redeem one reset"), "{frame}");
            assert!(
                frame.contains("❯ 2) Cancel"),
                "the mark rests on Cancel: {frame}"
            );
            assert!(frame.contains("1/y redeem"), "{frame}");
        }
        // A held redemption is named with its request id instead.
        app.pending_redemption = Some(PendingRedemption {
            account: "codex:c@x.com".into(),
            request_id: "rid-7".into(),
            credit_id: None,
        });
        let frame = render_app(&app, &view, 80, 24);
        assert!(frame.contains("UNRESOLVED redemption"), "{frame}");
        assert!(frame.contains("rid-7"), "{frame}");
    }

    /// The remove gate says WHICH account it deletes (it used to read
    /// `remove selected?`), follows the cursor, and keeps "any other key
    /// cancels": Enter and `2` cancel, only `1` / `y` remove.
    #[test]
    fn remove_gate_names_the_account_and_only_1_or_y_remove() {
        let view = control_view();
        let order = view.display_order(Default::default(), SystemTime::now());
        let name = |idx: usize| view.snapshot.accounts[order[idx]].id.0.clone();
        let mut app = remote_app();
        app.overlay = Overlay::Accounts;
        app.on_key_accounts(KeyCode::Char('r'), Some(&view));
        assert_eq!(app.mode, Mode::ConfirmRemove { idx: 0 });
        let frame = render_app(&app, &view, 80, 24);
        assert!(frame.contains(&format!("Remove {}", name(0))), "{frame}");
        app.on_key_confirm_remove(KeyCode::Down, 0, Some(&view));
        assert_eq!(app.mode, Mode::ConfirmRemove { idx: 1 });
        let frame = render_app(&app, &view, 80, 24);
        assert!(frame.contains(&format!("Remove {}", name(1))), "{frame}");

        for cancel in [
            KeyCode::Enter,
            KeyCode::Char('2'),
            KeyCode::Tab,
            KeyCode::Char('q'),
        ] {
            app.mode = Mode::ConfirmRemove { idx: 1 };
            app.on_key_confirm_remove(cancel, 1, Some(&view));
            assert_eq!(app.mode, Mode::Normal, "{cancel:?} closes the gate");
            assert_eq!(
                app.take_pending_remove(),
                None,
                "{cancel:?} removes nothing"
            );
        }
        app.mode = Mode::ConfirmRemove { idx: 1 };
        app.on_key_confirm_remove(KeyCode::Char('1'), 1, Some(&view));
        assert_eq!(
            app.take_pending_remove(),
            Some(name(1)),
            "1 removes the named account"
        );
    }

    /// The config gate states the setting and its old → new value on the
    /// card; ↑↓ move the mark, Enter answers it (Apply until moved), `2` and
    /// `n` cancel.
    #[test]
    fn config_gate_states_old_and_new_and_enter_answers_the_mark() {
        let view = control_view();
        let mut app = remote_app();
        app.overlay = Overlay::Config;
        app.activate_config_action(ui::ConfigAction::RoutingEnabled, Some(&view));
        assert!(
            matches!(app.mode, Mode::ConfigConfirm { .. }),
            "{:?}",
            app.mode
        );
        expire_status(&mut app);
        let frame = render_app(&app, &view, 80, 24);
        assert!(frame.contains("Set routing.enabled: off → on?"), "{frame}");
        assert!(frame.contains("reroutes LIVE traffic"), "{frame}");
        assert!(frame.contains("❯ 1) Apply"), "{frame}");

        // Moving the mark to Cancel makes Enter cancel.
        app.on_key_config_confirm(KeyCode::Down, ui::ConfigAction::RoutingEnabled, Some(&view));
        assert_eq!(app.confirm_choice, 1);
        assert!(render_app(&app, &view, 80, 24).contains("❯ 2) Cancel"));
        app.on_key_config_confirm(
            KeyCode::Enter,
            ui::ConfigAction::RoutingEnabled,
            Some(&view),
        );
        assert_eq!(app.mode, Mode::Normal);
        assert!(
            app.config_pending.is_none(),
            "Enter on Cancel applies nothing"
        );

        // Reopening starts on Apply again; a ceiling driven to 0 names both values.
        app.mode = Mode::ConfigEdit {
            action: ui::ConfigAction::Ceiling5h,
        };
        app.field = ComposerState::with_text("0").single_line();
        app.on_key_config_edit(
            press(KeyCode::Enter),
            ui::ConfigAction::Ceiling5h,
            Some(&view),
        );
        assert_eq!(app.confirm_choice, 0);
        let p = (view.select_params.five_hour_max * 100.0).round();
        let frame = render_app(&app, &view, 80, 24);
        assert!(
            frame.contains(&format!("Set 5h ceiling: {p}% → 0%?")),
            "{frame}"
        );
        app.on_key_config_confirm(KeyCode::Char('2'), ui::ConfigAction::Ceiling5h, Some(&view));
        assert_eq!(app.mode, Mode::Normal, "2 cancels");
    }

    /// The redemption id is minted ONCE by the client and held before the
    /// send; a retry reuses THAT id (never a fresh one), and an id the daemon
    /// reports as pending is adopted rather than replaced.
    #[test]
    fn redemption_reuses_the_held_request_id_on_retry() {
        let view = control_view();
        let order = view.display_order(Default::default(), SystemTime::now());
        let codex_pos = order
            .iter()
            .position(|&i| view.snapshot.accounts[i].credential_kind == "codex")
            .expect("codex row");

        let mut app = remote_app();
        app.submit_reset(codex_pos, &view);
        let ControlOp::Reset { request_id, .. } = app.pending_control.clone().expect("queued")
        else {
            panic!("expected a redemption");
        };
        assert!(!request_id.is_empty(), "the client mints the key");
        assert_eq!(
            app.pending_redemption
                .as_ref()
                .map(|p| p.request_id.clone()),
            Some(request_id.clone()),
            "held BEFORE the send"
        );

        // The outcome never arrived (uncertain): retry reuses the same key.
        app.pending_control = None;
        app.control_inflight = false;
        app.submit_reset(codex_pos, &view);
        let ControlOp::Reset {
            request_id: retry_id,
            ..
        } = app.pending_control.clone().expect("queued retry")
        else {
            panic!("expected a redemption");
        };
        assert_eq!(retry_id, request_id, "a retry never mints a new key");

        // A daemon-reported pending id (crash recovery / another client) is
        // adopted by a client that holds nothing.
        let mut view = control_view();
        view.usage_controls.insert(
            "codex:c@x.com".into(),
            crate::proxy::usage_controls::UsageControlDoc {
                pending_request_id: Some("01DAEMON".into()),
                ..Default::default()
            },
        );
        let mut app = remote_app();
        app.submit_reset(codex_pos, &view);
        match app.pending_control.as_ref().expect("queued") {
            ControlOp::Reset { request_id, .. } => assert_eq!(request_id, "01DAEMON"),
            other => panic!("expected a redemption, got {other:?}"),
        }
    }

    /// A refused queue must not leave a phantom hold: with another control
    /// operation in flight, `y` on the gate queues NOTHING, and the client
    /// must not start holding a redemption id it never sent. An id it was
    /// already holding stays exactly as it was.
    #[test]
    fn a_busy_queue_creates_no_phantom_pending_redemption() {
        let view = control_view();
        let order = view.display_order(Default::default(), SystemTime::now());
        let codex_pos = order
            .iter()
            .position(|&i| view.snapshot.accounts[i].credential_kind == "codex")
            .expect("codex row");

        // Busy flag 1: an operation is already running.
        let mut app = remote_app();
        app.control_inflight = true;
        app.submit_reset(codex_pos, &view);
        assert_eq!(app.pending_control, None, "nothing queued while in flight");
        assert!(
            app.pending_redemption.is_none(),
            "no hold for a redemption that was never sent"
        );

        // Busy flag 2: an operation is queued but not yet drained.
        let mut app = remote_app();
        app.pending_control = Some(ControlOp::Refresh { account: None });
        app.submit_reset(codex_pos, &view);
        assert_eq!(
            app.pending_control,
            Some(ControlOp::Refresh { account: None }),
            "the queued refresh is not displaced"
        );
        assert!(app.pending_redemption.is_none());

        // An EXISTING hold survives a refused attempt untouched.
        let mut app = remote_app();
        app.control_inflight = true;
        app.pending_redemption = Some(PendingRedemption {
            account: "codex:c@x.com".into(),
            request_id: "01HELD".into(),
            credit_id: Some("c1".into()),
        });
        app.submit_reset(codex_pos, &view);
        assert_eq!(
            app.pending_redemption,
            Some(PendingRedemption {
                account: "codex:c@x.com".into(),
                request_id: "01HELD".into(),
                credit_id: Some("c1".into()),
            }),
            "a refused attempt rewrites nothing"
        );
    }

    /// Receipt-close failure (R1): upstream reached a TERMINAL outcome but the
    /// daemon could not release its durable pending receipt, so it reports the
    /// redemption as still pending. That is a SUCCESS with a warning — the
    /// client keeps the safe retry identity and never re-offers the spend as a
    /// new one.
    #[test]
    fn terminal_outcome_that_is_still_pending_keeps_the_retry_identity() {
        let response: crate::proxy::usage_controls::ConsumeResponse =
            serde_json::from_value(serde_json::json!({
                "outcome": "reset",
                "request_id": "01RID",
                "windows_reset": 1,
                "usage_control": { "pending_request_id": "01RID" },
            }))
            .expect("consume response");
        let result = consume_result("codex:c@x.com", response);
        assert!(
            result.resolved.is_none(),
            "a receipt the daemon still reports as pending is not resolved"
        );
        assert_eq!(
            result.hold.as_ref().map(|p| p.request_id.clone()),
            Some("01RID".into()),
            "the safe retry identity is kept"
        );
        assert!(
            result.message.text.contains("redeemed"),
            "{:?}",
            result.message
        );
        assert!(
            result.message.text.contains("01RID"),
            "{:?}",
            result.message
        );
        // A success whose receipt is still open is a warning, not a plain ok.
        assert_eq!(result.message.outcome, twolab_tui::banner::Outcome::Warn);

        // The normal terminal case still resolves.
        let clean: crate::proxy::usage_controls::ConsumeResponse =
            serde_json::from_value(serde_json::json!({
                "outcome": "reset", "request_id": "01RID", "windows_reset": 1,
            }))
            .expect("consume response");
        let result = consume_result("codex:c@x.com", clean);
        assert_eq!(result.resolved.as_deref(), Some("codex:c@x.com"));
        assert!(result.hold.is_none());
    }

    /// Closing the dialog is NOT abandoning: the held key survives, and
    /// reopening the gate offers that same id.
    #[test]
    fn closing_the_dialog_keeps_the_pending_redemption() {
        let view = control_view();
        let order = view.display_order(Default::default(), SystemTime::now());
        let codex_pos = order
            .iter()
            .position(|&i| view.snapshot.accounts[i].credential_kind == "codex")
            .expect("codex row");
        let mut app = remote_app();
        app.pending_redemption = Some(PendingRedemption {
            account: "codex:c@x.com".into(),
            request_id: "01HELD".into(),
            credit_id: None,
        });
        app.mode = Mode::ConfirmReset { idx: codex_pos };
        app.on_key_confirm_reset(KeyCode::Esc, codex_pos, Some(&view));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(
            app.pending_redemption
                .as_ref()
                .map(|p| p.request_id.clone()),
            Some("01HELD".into()),
            "closing the dialog cannot clear an unresolved redemption"
        );
        // Reopening names that id instead of a fresh prompt.
        app.overlay = Overlay::Accounts;
        app.on_key_accounts(KeyCode::Char('R'), Some(&view));
        assert!(app
            .status_line()
            .is_some_and(|s| s.contains("01HELD") && s.contains("UNRESOLVED")));
    }

    /// Only a TERMINAL outcome releases the held key; an uncertain one
    /// replaces it (and the single-flight slot is freed either way).
    #[test]
    fn control_results_hold_uncertain_ids_and_release_terminal_ones() {
        let mut app = remote_app();
        app.control_inflight = true;
        app.apply_control_result(ControlResult {
            message: StatusMsg::fail("uncertain"),
            hold: Some(PendingRedemption {
                account: "codex:c@x.com".into(),
                request_id: "01RID".into(),
                credit_id: None,
            }),
            resolved: None,
        });
        assert!(!app.control_inflight, "the slot is freed");
        assert_eq!(
            app.pending_redemption
                .as_ref()
                .map(|p| p.request_id.clone()),
            Some("01RID".into())
        );

        // A terminal outcome for ANOTHER account leaves it held.
        app.apply_control_result(ControlResult {
            message: StatusMsg::ok("done"),
            hold: None,
            resolved: Some("claude:a@x.com".into()),
        });
        assert!(app.pending_redemption.is_some());

        // A terminal outcome for THIS account releases it.
        app.apply_control_result(ControlResult {
            message: StatusMsg::ok("done"),
            hold: None,
            resolved: Some("codex:c@x.com".into()),
        });
        assert!(app.pending_redemption.is_none());
    }

    /// Status lines: a partial refresh failure is never summarized into a
    /// green — its outcome is `fail` and every failed account keeps its own
    /// line —, the redemption outcomes stay distinct in text and outcome, a
    /// post-success stale read is a warning ON a success, and a switch shows
    /// its refresh warning separately from the switch result.
    #[test]
    fn control_status_lines_never_flatten_failures_into_success() {
        use crate::auth::codex_usage::ResetOutcome;
        use crate::proxy::usage_controls::{ConsumeResponse, RefreshResponse, SwitchResponse};
        use twolab_tui::banner::Outcome;

        // Built through serde, not struct literals: these are the daemon's
        // wire types, and pinning their exact field SET here would break on
        // every additive field the backend adds.
        let response: RefreshResponse = serde_json::from_value(serde_json::json!({
            "ok": false,
            "results": [
                { "account": "codex:c@x.com", "ok": true, "provider": "codex",
                  "usage_control": { "available_resets": 3 } },
                { "account": "claude:a@x.com", "ok": false, "provider": "oauth",
                  "error": "upstream 502" },
            ],
        }))
        .expect("refresh response");
        let message = refresh_message(&response);
        assert_eq!(message.outcome, Outcome::Fail, "{message:?}");
        assert!(message.text.contains("1 ok, 1 FAILED"), "{message:?}");
        assert_eq!(
            message.lines,
            vec!["claude:a@x.com: upstream 502".to_string()],
            "the failure is its own line, account and error: {message:?}"
        );
        // A single-account failure stays one line, the error in it.
        let lone: RefreshResponse = serde_json::from_value(serde_json::json!({
            "ok": false,
            "results": [
                { "account": "claude:a@x.com", "ok": false, "provider": "oauth",
                  "error": "upstream 502" },
            ],
        }))
        .expect("refresh response");
        let lone = refresh_message(&lone);
        assert_eq!(lone.outcome, Outcome::Fail);
        assert!(lone.lines.is_empty(), "{lone:?}");
        assert!(
            lone.text.contains("claude:a@x.com") && lone.text.contains("upstream 502"),
            "{lone:?}"
        );

        // Single-account success names the inventory it observed.
        let one: RefreshResponse = serde_json::from_value(serde_json::json!({
            "ok": true,
            "results": [
                { "account": "codex:c@x.com", "ok": true, "provider": "codex",
                  "usage_control": { "available_resets": 3, "applicable_resets": 0 } },
            ],
        }))
        .expect("refresh response");
        let message = refresh_message(&one);
        assert_eq!(message.outcome, Outcome::Ok);
        assert!(message.text.contains("3 owned"), "{message:?}");
        assert!(message.text.contains("0 applicable now"), "{message:?}");

        let consume = |outcome: ResetOutcome, refresh_warning: Option<&str>| {
            serde_json::from_value::<ConsumeResponse>(serde_json::json!({
                "outcome": outcome, "request_id": "01RID", "windows_reset": 1,
                "refresh_warning": refresh_warning,
            }))
            .expect("consume response")
        };
        for (outcome, text, expected) in [
            (ResetOutcome::Reset, "redeemed", Outcome::Ok),
            (
                ResetOutcome::AlreadyRedeemed,
                "no second reset",
                Outcome::Warn,
            ),
            (
                ResetOutcome::NothingToReset,
                "nothing to reset",
                Outcome::Warn,
            ),
            (ResetOutcome::NoCredit, "no reset credit", Outcome::Fail),
            (ResetOutcome::Cooldown, "cooldown", Outcome::Fail),
        ] {
            let message = consume_message("c", &consume(outcome, None));
            assert!(message.text.contains(text), "{message:?}");
            assert_eq!(message.outcome, expected, "{message:?}");
        }
        // A failed follow-up read rides on the SUCCESS, not instead of it.
        let stale = consume_message("c", &consume(ResetOutcome::Reset, Some("read failed")));
        assert!(
            stale.text.contains("redeemed") && stale.text.contains("WARNING"),
            "{stale:?}"
        );
        assert_eq!(stale.outcome, Outcome::Warn, "{stale:?}");

        let switched = switch_message(
            &serde_json::from_value::<SwitchResponse>(serde_json::json!({
                "ok": true, "current": "codex:c@x.com",
                "refresh_warning": "usage read timed out",
            }))
            .expect("switch response"),
        );
        assert!(switched.text.contains("switched to"), "{switched:?}");
        assert!(switched.text.contains("WARNING"), "{switched:?}");
        assert!(
            switched.text.contains("usage read timed out"),
            "{switched:?}"
        );
        // The switch happened: a warning, not a failure.
        assert_eq!(switched.outcome, Outcome::Warn, "{switched:?}");
    }

    /// A refresh of several accounts with failures is a toast card with
    /// every failed account on its own line (it was one status row joined
    /// with ` · `, cut at 80 columns and gone after 5 s): the outcome's word
    /// in the border, `+N more` past four lines, the footer row left free —
    /// and the card goes after its 8 s on the dashboard's own clock.
    #[test]
    fn refresh_failures_are_a_toast_with_a_line_per_account() {
        use crate::proxy::usage_controls::RefreshResponse;
        let view = control_view();
        let mut app = remote_app();
        let mut results = vec![serde_json::json!({
            "account": "codex:c@x.com", "ok": true, "provider": "codex",
        })];
        results.extend((2..=6).map(|n| {
            serde_json::json!({
                "account": format!("claude:a{n}@x.com"), "ok": false,
                "provider": "oauth", "error": format!("upstream 50{n}"),
            })
        }));
        let response: RefreshResponse =
            serde_json::from_value(serde_json::json!({ "ok": false, "results": results }))
                .expect("refresh response");
        app.control_inflight = true;
        app.apply_control_result(ControlResult {
            message: refresh_message(&response),
            hold: None,
            resolved: None,
        });
        assert_eq!(
            app.status_line(),
            None,
            "the footer row gives way to the card"
        );
        for (w, h) in [(80, 24), (179, 43)] {
            let frame = render_app(&app, &view, w, h);
            let rows: Vec<&str> = frame.lines().collect();
            assert!(
                rows.iter().any(|r| r.contains("┌ fail ")),
                "the outcome's word in the border:\n{frame}"
            );
            assert!(frame.contains("✗ refresh: 1 ok, 5 FAILED"), "{frame}");
            for n in 2..=4 {
                let line = format!("claude:a{n}@x.com: upstream 50{n}");
                assert!(
                    rows.iter().any(|r| r.contains(&line)),
                    "{line} on its own line:\n{frame}"
                );
            }
            assert!(frame.contains("+2 more"), "{frame}");
            assert!(!frame.contains("claude:a6@x.com"), "{frame}");
        }
        // Past the failure's 8 s the card is gone; the draw only sees the
        // clock the chrome carries.
        let later = app.chrome_at(app.started + Duration::from_secs(9));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).expect("terminal");
        let mut hits = None;
        terminal
            .draw(|frame| ui::draw(frame, Some(&view), &later, &mut hits))
            .expect("draw");
        let text = format!("{:?}", terminal.backend().buffer());
        assert!(!text.contains("FAILED"), "{text}");
    }

    /// The connecting screen counts how long the attach has been retrying
    /// from the moment the link was lost — every further failed poll keeps
    /// the count going rather than restarting it.
    #[test]
    fn attach_wait_counts_from_the_loss_not_from_each_failed_poll() {
        let mut app = remote_app();
        if let Backend::Remote(remote) = &mut app.backend {
            remote.connected = true;
        }
        let ago = |secs| {
            Instant::now()
                .checked_sub(Duration::from_secs(secs))
                .expect("monotonic clock far enough from its origin")
        };
        app.link_since = ago(300);
        app.apply_fetch(FetchMsg::Lost);
        assert!(
            app.chrome().attach_wait < Duration::from_secs(5),
            "lost now"
        );
        app.link_since = ago(30);
        app.apply_fetch(FetchMsg::Lost);
        let wait = app.chrome().attach_wait;
        assert!(wait >= Duration::from_secs(30), "still retrying: {wait:?}");
    }

    /// The raw viewer's action flash says how it went and is aged by the
    /// app's clock when the frame's chrome is built: the draw no longer reads
    /// `Instant::now()`, and the flash lives as long as the status row's.
    #[test]
    fn raw_flash_carries_its_outcome_and_ages_on_the_app_clock() {
        let view = control_view();
        let mut app = remote_app();
        let key = seed_one_hit(&mut app);
        let now = Instant::now();
        app.raw_modal = Some(RawModal {
            key,
            id: 7,
            generation: 1,
            title: "raw".into(),
            tab: 0,
            scroll: 0,
            view: Default::default(),
            spin: 0,
            since: Duration::ZERO,
            flash: Some((StatusMsg::fail("clipboard tool not found"), now)),
            state: RawModalState::Failed("gone".into()),
        });
        let frame = render_app(&app, &view, 80, 24);
        assert!(frame.contains("✗ fail clipboard tool not found"), "{frame}");
        // Still there just before the status TTL, gone at it.
        let before = app.chrome_at(now + STATUS_TTL - Duration::from_millis(1));
        assert!(before
            .raw_modal
            .as_ref()
            .and_then(|m| m.flash.as_ref())
            .is_some());
        let after = app.chrome_at(now + STATUS_TTL);
        assert!(after
            .raw_modal
            .as_ref()
            .and_then(|m| m.flash.as_ref())
            .is_none());
        app.raw_modal.as_mut().expect("modal").flash =
            Some((StatusMsg::ok("copied 4 bytes → pbcopy"), now));
        let frame = render_app(&app, &view, 80, 24);
        assert!(frame.contains("✓ ok   copied 4 bytes → pbcopy"), "{frame}");
    }

    /// A daemon error that carries an id (uncertain / pending) is held for an
    /// explicit same-key retry; every other refusal spent nothing and holds
    /// nothing.
    #[test]
    fn control_errors_hold_only_the_ids_that_must_be_retried() {
        use crate::proxy::usage_controls::UsageControlError as E;
        let uncertain = control_error_result(
            "codex:c@x.com",
            &E::Uncertain {
                message: "timeout".into(),
                request_id: "01RID".into(),
            },
        );
        assert_eq!(
            uncertain.hold.as_ref().map(|p| p.request_id.clone()),
            Some("01RID".into())
        );
        assert!(
            uncertain.message.text.contains("UNCERTAIN"),
            "{:?}",
            uncertain.message
        );
        assert_eq!(uncertain.message.outcome, twolab_tui::banner::Outcome::Fail);

        let pending = control_error_result(
            "codex:c@x.com",
            &E::Pending {
                account: "codex:c@x.com".into(),
                request_id: "01OLD".into(),
                credit_id: Some("cred-1".into()),
            },
        );
        assert_eq!(
            pending.hold.as_ref().map(|p| p.request_id.clone()),
            Some("01OLD".into())
        );

        // A plain refusal (busy / unsupported / no credit) holds nothing.
        let busy = control_error_result("codex:c@x.com", &E::Busy("codex:c@x.com".into()));
        assert!(busy.hold.is_none());
        assert!(busy.resolved.is_none());
    }
}
