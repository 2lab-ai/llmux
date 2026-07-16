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
mod anim;
mod clip;
mod event;
// pub(crate): `cli::status` reuses the token/age formatters so the plain
// `llmux status` output and the dashboard agree on the display.
pub(crate) mod format;
pub(crate) mod logs;
mod triage;
mod ui;
mod view;

pub use event::{ActivityEvent, TokenCounts};

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

use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use crate::config::AccountConfig;
use crate::dashboard::{CodexSettingsDoc, DashboardDoc};
use crate::scheduler::select;
use view::DashboardView;

/// Codex models the dashboard cycles through with `m` (req8.1). Any model can
/// still be set via config / the control endpoint; this is the quick-pick set.
const CODEX_MODELS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.5",
    "gpt-5.5-codex",
    "gpt-5-codex",
];
/// Reasoning-effort levels cycled with `e` (and the group-settings bar);
/// "" = BYPASS — the client's `output_config.effort` rides through (UI-3
/// U12). A concrete value OVERRIDES every request. `max` is native on the
/// gpt-5.6 family and clamps to `xhigh` on older models.
const CODEX_EFFORTS: &[&str] = &["", "minimal", "low", "medium", "high", "xhigh", "max"];
/// Grok effort rotation for the group-settings bar (UI-3 U12); "" = bypass.
/// Values are the config superset (`none|low|medium|high`) — per-model
/// clamping happens at request time in the provider.
const GROK_EFFORTS: &[&str] = &["", "none", "low", "medium", "high"];

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
/// How long a transient status-line message stays on screen.
const STATUS_TTL: Duration = Duration::from_secs(5);

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
/// `login_codex_interactive` for ChatGPT/Codex — then the minted credential is
/// injected into the daemon (in-process locally, `POST /llmux/inject-account`
/// when attached). One code path for local and attach (issue #4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginKind {
    /// Anthropic Claude PKCE OAuth (`login_interactive`).
    Anthropic,
    /// ChatGPT / Codex OAuth (`login_codex_interactive`).
    Codex,
}

impl LoginKind {
    /// Quick-pick rows for `Mode::NewLogin`, in display order.
    pub(crate) const ALL: [LoginKind; 2] = [LoginKind::Anthropic, LoginKind::Codex];

    /// One-line label for the picker + status line.
    pub(crate) fn label(self) -> &'static str {
        match self {
            LoginKind::Anthropic => "Claude (Anthropic OAuth)",
            LoginKind::Codex => "Codex (ChatGPT OAuth)",
        }
    }
}

/// Input mode: normal keybar vs. account-selection (the `s` key) vs. the
/// add-account key entry (`a`) vs. the remove confirmation (`r`) vs. the
/// new-login provider picker (`n`).
///
/// Deliberately `Copy` (no owned buffer inside): the add-account input text
/// lives in [`App::add_input`] so the masked render never has to clone a
/// secret through this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    /// Cursor row for the pending switch.
    Select {
        idx: usize,
    },
    /// Entering an API key for a new account (the `a` key). The typed text is
    /// held in [`App::add_input`] and rendered masked.
    AddKey,
    /// Confirming a destructive account removal (the `r` key). `idx` is the
    /// display row being removed; the name is resolved at confirm time.
    ConfirmRemove {
        idx: usize,
    },
    /// Picking the provider for a new browser login (the `n` key). `idx` is the
    /// cursor row into [`LoginKind::ALL`]. Enter starts the OAuth flow in this
    /// client; the minted credential is injected into the daemon.
    NewLogin {
        idx: usize,
    },
    /// Editing per-account ceiling overrides for the switcher's highlighted
    /// row (`L`). The typed text lives in [`App::add_input`]; format
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
}

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
    pub status_line: Option<String>,
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
    /// Cursor row in the Sessions overlay's session list.
    pub session_cursor: usize,
    /// Sessions overlay sort order (`o` cycles).
    pub session_sort: SessionSort,
    /// `Some` in attach mode.
    pub attach: Option<Attach>,
    /// Number of characters typed so far in `Mode::AddKey` — the footer shows
    /// a masked prompt (`••••`) of this width, never the raw key.
    pub add_input_len: usize,
    /// Session-local `u`-key override of the quota-gauge fill direction;
    /// `None` = the config default carried on the view applies.
    pub quota_display_override: Option<crate::config::QuotaDisplay>,
    /// `t`-key session toggle: absolute UTC reset stamps in the quota bars.
    pub reset_absolute: bool,
    /// Live text of the limits editor (`Mode::EditLimits`); empty otherwise.
    /// Rendered raw in the footer (percent ceilings are not secrets).
    pub limits_input: String,
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
    /// Usage-tab granularity (`g` cycles hour/day/month, usage-stats).
    pub usage_gran: activity::UsageGran,
    /// Usage-tab scroll offset: number of newest BUCKETS skipped (0 = most
    /// recent at the top).
    pub usage_scroll: usize,
    /// The click-opened input-text modal (UI-6 item 3), or `None` when closed.
    /// Drawn last over MAIN; its content is looked up from `view.completed` by
    /// the stored key every frame, so it works identically in local and attach
    /// mode and closes gracefully when the entry ages out of the ring.
    pub input_modal: Option<InputModal>,
    /// The click-opened raw request/response viewer (UI-7), or `None` when
    /// closed. Content-owning (cheap to clone — the body lines sit behind an
    /// `Arc`), drawn last like the input modal.
    pub raw_modal: Option<RawModal>,
}

/// The click-opened full-input modal (UI-6 item 3). Holds only the clicked
/// entry's STABLE identity (never a list index — rows prepend) plus the vertical
/// scroll offset in wrapped lines; the excerpt text itself is re-read from
/// `view.completed` each frame, so nothing goes stale and no wire field is added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputModal {
    pub key: activity::ActivityKey,
    pub scroll: u16,
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
    pub scroll: u16,
    /// Horizontal pan in display cells (UI-8; clamped like `scroll`).
    pub hscroll: u16,
    /// Animation frame for the Loading spinner (advances with the shared tick).
    pub spin: usize,
    /// Action feedback ("copied … → pbcopy" / "saved → …"), shown in place of
    /// the key legend until it expires (drawn ~3 s).
    pub flash: Option<(String, std::time::Instant)>,
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

/// Outcome of a background export, delivered on the clip channel.
struct ClipResult {
    generation: u64,
    message: String,
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
    /// Switch target chosen in select mode, performed by the event loop
    /// (key handling is sync; the POST is not).
    pending_switch: Option<String>,
    /// Codex settings change (fast/model/effort) queued by a key, performed by
    /// the event loop via `POST /llmux/codex` (req8.1).
    pending_codex: Option<crate::dashboard::CodexSettingsDoc>,
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
    status: Option<(String, Instant)>,
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
    /// The activity panel's hit-test layout from the LAST rendered frame: the
    /// panel rect + the clickable request rows. Recorded by the event loop after
    /// each `draw`, read by the mouse handler to map a click to an entry.
    activity_chrome: ui::ActivityChrome,
    /// The tab bar's hit-test layout from the LAST rendered frame (UI-3 U6):
    /// one rect per tab label. Same record/read cycle as `activity_chrome`.
    tab_chrome: Vec<ui::TabHit>,
    /// Sessions table hit layout from the last frame (mouse row select).
    sessions_chrome: Option<ui::SessionsChrome>,
    /// The raw viewer's hit-test layout from the LAST rendered frame (UI-8):
    /// payload-tab rects + top-right action buttons. Same record/read cycle
    /// as `activity_chrome`; empty while the modal is closed.
    raw_chrome: ui::RawModalChrome,
    /// Separator rows from the LAST rendered frame (UI-3 U7/U8): each is the
    /// top-border row of a pane, dragging it resizes the pane ABOVE it.
    separator_chrome: Vec<ui::SeparatorHit>,
    /// Session-local pane-height overrides set by separator drags.
    pane_heights: PaneHeights,
    /// Tokens-per-day chart span (UI-3 U14), cycled with `d` in Stats.
    chart_days: u64,
    /// Perf overlay span (`d` cycles) + series-table cursor.
    perf_days: u64,
    perf_cursor: usize,
    /// Accounts-table row rects from the LAST rendered frame (UI-3 U11) —
    /// right-click target map.
    account_row_chrome: Vec<ui::AccountRowHit>,
    /// The rendered context menu's hit layout (UI-3 U11), when open.
    menu_chrome: Option<ui::MenuChrome>,
    /// Group-settings bar segments from the LAST rendered frame (UI-3 U9/U10).
    setting_chrome: Vec<ui::SettingHit>,
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
    /// The separator currently being dragged, if any (mouse button held).
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
    /// Sender handed to the `spawn_blocking` load task by `open_sessions`; the
    /// event loop owns the receiver and applies each progressive partial. `None`
    /// only in unit tests that never run `event_loop` (they drive overlay state
    /// directly).
    sessions_tx: Option<mpsc::Sender<SessionsLoad>>,
    /// Cursor row in the Sessions overlay's session list.
    session_cursor: usize,
    /// Sessions overlay sort order (`o` cycles; re-applied on load delivery).
    session_sort: SessionSort,
    /// API-key buffer for `Mode::AddKey`. Held outside `Mode` so the enum
    /// stays `Copy` and the secret is owned in exactly one place; cleared on
    /// submit/cancel. Never rendered raw — the footer shows a masked width.
    add_input: String,
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
    /// Usage-tab granularity (`g` cycles hour/day/month, usage-stats).
    usage_gran: activity::UsageGran,
    /// Usage-tab scroll offset in BUCKETS (0 = newest at the top); reset when
    /// the granularity changes so a deep hourly scroll can't strand the
    /// monthly table past its last row.
    usage_scroll: usize,
    /// The click-opened input-text modal (UI-6 item 3); `None` when closed. The
    /// scroll offset is clamped after each draw against the wrapped line count
    /// the render pass reports (`MainChrome::input_modal_max_scroll`).
    input_modal: Option<InputModal>,
    /// The click-opened raw request/response viewer (UI-7); `None` when closed.
    raw_modal: Option<RawModal>,
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
}

impl App {
    fn new(backend: Backend) -> Self {
        Self {
            backend,
            mode: Mode::Normal,
            frame: 0,
            should_quit: false,
            status: None,
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
            activity_chrome: ui::ActivityChrome::default(),
            tab_chrome: Vec::new(),
            sessions_chrome: None,
            raw_chrome: ui::RawModalChrome::default(),
            separator_chrome: Vec::new(),
            pane_heights: PaneHeights::default(),
            chart_days: 14,
            perf_days: 14,
            perf_cursor: 0,
            account_row_chrome: Vec::new(),
            menu_chrome: None,
            menu_anchor: None,
            menu_account: None,
            setting_chrome: Vec::new(),
            limits_from_menu: false,
            drag: None,
            model_cursor: 0,
            stats_window: activity::StatsWindow::default(),
            sessions: Vec::new(),
            sessions_loading: false,
            sessions_pct: 100,
            sessions_tx: None,
            session_cursor: 0,
            session_sort: SessionSort::default(),
            add_input: String::new(),
            pending_login: None,
            quota_display_override: None,
            usage_gran: activity::UsageGran::default(),
            usage_scroll: 0,
            input_modal: None,
            raw_modal: None,
            pending_raw: None,
            raw_tx: None,
            raw_generation: 0,
            pending_clip: std::collections::VecDeque::new(),
            clip_inflight: false,
            clip_tx: None,
            reset_absolute: false,
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
        Chrome {
            frame: self.frame,
            mode: self.mode,
            overlay: self.overlay,
            activity_scroll: self.activity_scroll,
            expanded_activity: self.expanded_activity.clone(),
            expanded_run: self.expanded_run.clone(),
            model_cursor: self.model_cursor,
            stats_window: self.stats_window,
            sessions: self.sessions.clone(),
            sessions_loading: self.sessions_loading,
            sessions_pct: self.sessions_pct,
            session_cursor: self.session_cursor,
            session_sort: self.session_sort,
            add_input_len: self.add_input.chars().count(),
            quota_display_override: self.quota_display_override,
            reset_absolute: self.reset_absolute,
            pane_heights: self.pane_heights,
            menu_anchor: self.menu_anchor,
            menu_account: self.menu_account.clone(),
            chart_days: self.chart_days,
            perf_days: self.perf_days,
            perf_cursor: self.perf_cursor,
            usage_gran: self.usage_gran,
            usage_scroll: self.usage_scroll,
            input_modal: self.input_modal.clone(),
            // The Loading spinner rides the shared frame counter (set here so
            // the stored modal itself never needs a per-tick mutation).
            raw_modal: self.raw_modal.clone().map(|mut m| {
                m.spin = self.frame;
                m
            }),
            limits_input: if matches!(self.mode, Mode::EditLimits { .. }) {
                self.add_input.clone()
            } else {
                String::new()
            },
            status_line: self.status_line().map(str::to_string),
            attach: match &self.backend {
                Backend::Local(_) => None,
                Backend::Remote(remote) => Some(Attach {
                    pid: remote.doc.as_ref().map(|d| d.pid).or(remote.pid),
                    connected: remote.connected,
                }),
            },
        }
    }

    /// Active status-line message, if it hasn't expired.
    fn status_line(&self) -> Option<&str> {
        self.status
            .as_ref()
            .filter(|(_, since)| since.elapsed() < STATUS_TTL)
            .map(|(text, _)| text.as_str())
    }

    fn set_status(&mut self, text: String) {
        self.status = Some((text, Instant::now()));
    }

    fn apply_fetch(&mut self, msg: FetchMsg) {
        if let Backend::Remote(remote) = &mut self.backend {
            match msg {
                FetchMsg::Doc(doc) => {
                    remote.doc = Some(*doc);
                    remote.connected = true;
                }
                FetchMsg::Lost => remote.connected = false,
            }
        }
    }

    fn take_pending_switch(&mut self) -> Option<String> {
        match &mut self.backend {
            Backend::Remote(remote) => remote.pending_switch.take(),
            Backend::Local(_) => None,
        }
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
        // A pending `Mode` interaction (account switch / key entry / remove
        // confirm / login picker) always takes the key first — these run WITHIN
        // the Accounts overlay (issues #3/#4) and must keep working unchanged.
        match self.mode {
            Mode::Select { idx } => return self.on_key_select(key.code, idx, view),
            Mode::EditLimits { idx } => return self.on_key_edit_limits(key.code, idx, view),
            Mode::AddKey => return self.on_key_add(key.code),
            Mode::ConfirmRemove { idx } => return self.on_key_confirm_remove(key.code, idx, view),
            Mode::NewLogin { idx } => return self.on_key_new_login(key.code, idx),
            Mode::ContextMenu { idx, item } => {
                return self.on_key_context_menu(key.code, idx, item, view)
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
            Overlay::Config => self.on_key_config(key.code),
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
        // The input modal (UI-6 item 3), when open, owns the mouse: the wheel
        // scrolls it (clamped after draw) and every click is swallowed so it
        // can't reach a row beneath the modal.
        if let Some(modal) = self.input_modal.as_mut() {
            match mouse.kind {
                MouseEventKind::ScrollUp => modal.scroll = modal.scroll.saturating_sub(1),
                MouseEventKind::ScrollDown => modal.scroll = modal.scroll.saturating_add(1),
                _ => {}
            }
            return true;
        }
        // The raw viewer (UI-7) owns the mouse the same way when open; UI-8
        // adds clickable payload tabs + the top-right action buttons and a
        // horizontal wheel pan.
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
                    if let Some(btn) = button {
                        self.raw_modal_action(btn);
                    } else if let (Some(idx), Some(modal)) = (tab, self.raw_modal.as_mut()) {
                        modal.tab = idx;
                        modal.scroll = 0;
                        modal.hscroll = 0;
                    }
                }
                kind => {
                    if let Some(modal) = self.raw_modal.as_mut() {
                        match kind {
                            MouseEventKind::ScrollUp => {
                                modal.scroll = modal.scroll.saturating_sub(3)
                            }
                            MouseEventKind::ScrollDown => {
                                modal.scroll = modal.scroll.saturating_add(3)
                            }
                            MouseEventKind::ScrollLeft => {
                                modal.hscroll = modal.hscroll.saturating_sub(RAW_PAN)
                            }
                            MouseEventKind::ScrollRight => {
                                modal.hscroll = modal.hscroll.saturating_add(RAW_PAN)
                            }
                            _ => {}
                        }
                    }
                }
            }
            return true;
        }
        // Tab-bar clicks (UI-3 U6) work from ANY overlay while no text-entry
        // interaction is pending — the tab bar is how the mouse navigates.
        if self.mode == Mode::Normal
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            if let Some(tab) = ui::hit_test_tabs(&self.tab_chrome, mouse.column, mouse.row) {
                self.open_tab(tab, view);
                return true;
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
                match self
                    .menu_chrome
                    .as_ref()
                    .and_then(|m| m.hit_item(mouse.column, mouse.row))
                {
                    Some(item) => self.run_menu_item(idx, item, view),
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
            if let Some(idx) = self
                .account_row_chrome
                .iter()
                .find(|r| {
                    mouse.row == r.area.y
                        && mouse.column >= r.area.x
                        && mouse.column < r.area.right()
                })
                .map(|r| r.display_idx)
            {
                // Pin the REAL account id now; display indexes reorder.
                self.menu_account = view.and_then(|v| {
                    let order = v.display_order(SystemTime::now());
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
        // Otherwise only MAIN (no overlay, no pending mode interaction) gets
        // the mouse.
        if self.overlay != Overlay::None || self.mode != Mode::Normal {
            return false;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Separator rows first (UI-3 U7/U8): press starts a drag.
                if let Some(sep) = self
                    .separator_chrome
                    .iter()
                    .find(|s| s.y == mouse.row)
                    .copied()
                {
                    self.drag = Some(sep);
                    return true;
                }
                // Group-settings bar (UI-3 U9/U10): click a segment to
                // rotate that setting.
                if let Some(action) = self
                    .setting_chrome
                    .iter()
                    .find(|h| {
                        mouse.row == h.area.y
                            && mouse.column >= h.area.x
                            && mouse.column < h.area.right()
                    })
                    .map(|h| h.action)
                {
                    match action {
                        ui::SettingAction::SchedMode => self.toggle_scheduler_mode(view),
                        ui::SettingAction::CodexModel => self.cycle_codex_model(view),
                        ui::SettingAction::CodexEffort => self.cycle_codex_effort(view),
                        ui::SettingAction::CodexFast => self.toggle_codex_fast(view),
                        ui::SettingAction::GrokEffort => self.cycle_grok_effort(view),
                    }
                    return true;
                }
                match ui::hit_test_activity(&self.activity_chrome, mouse.column, mouse.row) {
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
        self.input_modal = Some(InputModal { key, scroll: 0 });
    }

    /// Key handling while the input modal is open (UI-6 item 3). Esc/q/Enter
    /// close it; the arrows/PgUp/PgDn adjust the scroll offset (over-scroll is
    /// clamped after the next draw against the wrapped line count). Every other
    /// key is swallowed so nothing leaks to MAIN beneath the modal.
    fn on_key_input_modal(&mut self, code: KeyCode) {
        let Some(modal) = self.input_modal.as_mut() else {
            return;
        };
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.input_modal = None,
            KeyCode::Up | KeyCode::Char('k') => modal.scroll = modal.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => modal.scroll = modal.scroll.saturating_add(1),
            KeyCode::PageUp => modal.scroll = modal.scroll.saturating_sub(MODAL_PAGE),
            KeyCode::PageDown => modal.scroll = modal.scroll.saturating_add(MODAL_PAGE),
            KeyCode::Home => modal.scroll = 0,
            KeyCode::End => modal.scroll = u16::MAX,
            _ => {}
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
            hscroll: 0,
            spin: 0,
            flash: None,
            state,
        });
    }

    /// Key handling while the raw viewer is open (UI-7/UI-8): Esc/q/Enter
    /// close, ←/→/Tab/h/l walk the payload tabs (both offsets reset — tabs
    /// have independent sizes), arrows/PgUp/PgDn/Home/End scroll, H/L pan
    /// horizontally, and c/C/a/s/S fire the copy/curl/copy-all/save/save-all
    /// actions (same as the top-right buttons). Everything else is swallowed
    /// so nothing leaks beneath the modal.
    fn on_key_raw_modal(&mut self, code: KeyCode) {
        let Some(modal) = self.raw_modal.as_mut() else {
            return;
        };
        let tab_count = match &modal.state {
            RawModalState::Ready(content) => content.tabs.len().max(1),
            _ => 1,
        };
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.raw_modal = None,
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
                modal.tab = (modal.tab + 1) % tab_count;
                modal.scroll = 0;
                modal.hscroll = 0;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                modal.tab = (modal.tab + tab_count - 1) % tab_count;
                modal.scroll = 0;
                modal.hscroll = 0;
            }
            KeyCode::Char('H') => modal.hscroll = modal.hscroll.saturating_sub(RAW_PAN),
            KeyCode::Char('L') => modal.hscroll = modal.hscroll.saturating_add(RAW_PAN),
            KeyCode::Up | KeyCode::Char('k') => modal.scroll = modal.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => modal.scroll = modal.scroll.saturating_add(1),
            KeyCode::PageUp => modal.scroll = modal.scroll.saturating_sub(MODAL_PAGE),
            KeyCode::PageDown => modal.scroll = modal.scroll.saturating_add(MODAL_PAGE),
            KeyCode::Home => modal.scroll = 0,
            KeyCode::End => modal.scroll = u16::MAX,
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
            modal.flash = Some((
                "raw record not loaded yet".to_string(),
                std::time::Instant::now(),
            ));
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
                "export busy — try again in a moment".to_string(),
                std::time::Instant::now(),
            ));
            return;
        }
        modal.flash = Some(("working…".to_string(), std::time::Instant::now()));
        self.pending_clip.push_back(ClipReq {
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
        if let Some(modal) = self.raw_modal.as_mut() {
            if modal.generation == result.generation {
                modal.flash = Some((result.message, std::time::Instant::now()));
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
                    Ok(path) => match button {
                        ui::RawButton::SaveAll => format!("saved record → {path}"),
                        _ => format!("saved → {path}"),
                    },
                    Err(err) => err,
                }
            } else {
                let n = payload.len();
                match clip::copy(&payload) {
                    Ok(dest) => match button {
                        ui::RawButton::CopyCurl => format!("copied curl ({n} bytes) → {dest}"),
                        ui::RawButton::CopyAll => format!("copied all {n} bytes → {dest}"),
                        _ => format!("copied {n} bytes → {dest}"),
                    },
                    Err(err) => err,
                }
            };
            let _ = tx.blocking_send(ClipResult {
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
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('l') | KeyCode::Esc => self.overlay = Overlay::None,
            _ => {}
        }
    }

    /// Number of context-menu entries (UI-3 U11): switch now / pause·resume /
    /// set limit / delete.
    const MENU_ITEMS: usize = 4;

    /// Key handling for the context menu (UI-3 U11): ↑↓ move, Enter runs the
    /// highlighted entry, Esc (or any other key) dismisses.
    fn on_key_context_menu(
        &mut self,
        code: KeyCode,
        idx: usize,
        item: usize,
        view: Option<&DashboardView>,
    ) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::ContextMenu {
                    idx,
                    item: item.saturating_sub(1),
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.mode = Mode::ContextMenu {
                    idx,
                    item: (item + 1).min(Self::MENU_ITEMS - 1),
                };
            }
            KeyCode::Enter => self.run_menu_item(idx, item, view),
            _ => self.close_menu(),
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
                let order = v.display_order(SystemTime::now());
                match order
                    .iter()
                    .position(|&i| v.snapshot.accounts.get(i).is_some_and(|a| a.id.0 == *name))
                {
                    Some(pos) => pos,
                    None => {
                        self.set_status(format!("{name} is gone — menu action cancelled"));
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
            .map(|v| ui::perf_series_count(v, self.perf_days))
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

    /// Key handling for the Config overlay (`c`, UI-3 U6). `c`/`Esc` closes.
    fn on_key_config(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') | KeyCode::Esc => self.overlay = Overlay::None,
            _ => {}
        }
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
            Overlay::Usage => {
                self.usage_scroll = 0;
                self.overlay = Overlay::Usage;
            }
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
            // Summon overlays (issue #5).
            KeyCode::Char('a') => self.overlay = Overlay::Accounts,
            KeyCode::Char('g') => self.open_stats(view),
            KeyCode::Char('l') => self.overlay = Overlay::Logs,
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
                Ok(mode) => self.set_status(format!("scheduler mode: {}", mode.label())),
                Err(err) => self.set_status(format!("scheduler mode change failed: {err}")),
            },
            Backend::Remote(remote) => {
                remote.pending_mode = Some(next);
                self.set_status(format!("scheduler mode → {}…", next.label()));
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
                format!("scheduler mode: {}", mode.label())
            }
            Ok(response) => format!("scheduler mode change failed: {}", response.status()),
            Err(err) => format!("scheduler mode change failed: {err}"),
        };
        self.set_status(message);
    }

    /// Flip the quota bars between reset countdown and absolute UTC stamp.
    fn toggle_reset_display(&mut self) {
        self.reset_absolute = !self.reset_absolute;
        self.set_status(
            if self.reset_absolute {
                "reset shown as absolute time (UTC)"
            } else {
                "reset shown as countdown"
            }
            .into(),
        );
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
        self.set_status(
            match next {
                QuotaDisplay::Used => "quota gauges fill: used%",
                QuotaDisplay::Remaining => "quota gauges fill: remaining%",
            }
            .into(),
        );
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
            // Switch the active account (the `s` switcher, now scoped to this
            // overlay). Rows render in selection order; the current account
            // (when one exists) is always row 0 — start the cursor there.
            KeyCode::Char('s') => {
                let accounts = view.map_or(0, |v| v.snapshot.accounts.len());
                if accounts == 0 {
                    self.set_status("no accounts to switch between".into());
                } else {
                    self.mode = Mode::Select { idx: 0 };
                }
            }
            // Add an API-key account (issue #3): works in BOTH local and attach
            // mode (local: in-process config update + pool reload; remote:
            // POST /llmux/add-account).
            KeyCode::Char('a') => {
                self.add_input.clear();
                self.mode = Mode::AddKey;
                self.set_status(
                    "add account: paste an Anthropic API key, Enter to add, Esc to cancel".into(),
                );
            }
            // Remove the selected account (issue #3): a destructive delete, so
            // it opens a confirm step (y/N) — never a silent delete.
            KeyCode::Char('r') => {
                let len = view.map_or(0, |v| v.snapshot.accounts.len());
                if len == 0 {
                    self.set_status("no accounts to remove".into());
                } else {
                    self.mode = Mode::ConfirmRemove { idx: 0 };
                }
            }
            // Start a NEW browser login (issue #4): opens a provider picker
            // (Claude / Codex). The OAuth flow runs in THIS client; the minted
            // credential is injected into the daemon, so it works in both local
            // and attach mode with no restart.
            KeyCode::Char('n') => self.open_new_login(),
            _ => {}
        }
    }

    /// Open the Stats overlay (`g`). No-op (with a hint) until at least one
    /// model row exists, matching the old detailed-view guard (req13).
    fn open_stats(&mut self, view: Option<&DashboardView>) {
        if view.is_some_and(|v| !v.model_usage.is_empty()) {
            self.overlay = Overlay::Stats;
            self.model_cursor = 0;
        } else {
            self.set_status("models: no model usage yet".into());
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
            None => self.set_status("codex: no codex account (run `llmux login --codex`)".into()),
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
            self.set_status("codex: no codex account".into());
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
            self.set_status("codex: no codex account".into());
        }
    }

    /// Cycle the grok effort override (UI-3 U12): bypass → none → low →
    /// medium → high → bypass. Local mode applies + persists in-process;
    /// attach mode queues `POST /llmux/grok`.
    fn cycle_grok_effort(&mut self, view: Option<&DashboardView>) {
        let Some(view) = view else { return };
        if !view.grok.available {
            self.set_status("grok: no grok account".into());
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
                let mut shape = state.grok.shape();
                shape.effort = effort.clone();
                state.grok.set_shape(shape);
                if let Some(path) = &state.config_path {
                    let _ = crate::config::update_path(path, |c| {
                        c.grok.reasoning_effort = effort.clone();
                    });
                }
                self.set_status(format!("grok effort: {label}"));
            }
            Backend::Remote(remote) => {
                remote.pending_grok = Some(effort);
                self.set_status(format!("grok effort → {label}…"));
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
            Ok(response) if response.status().is_success() => format!("grok effort: {label}"),
            Ok(response) => format!("grok effort change failed: {}", response.status()),
            Err(err) => format!("grok effort change failed: {err}"),
        };
        self.set_status(message);
    }

    /// Apply a codex settings change: locally in-process, or queued for the
    /// event loop to POST in attach mode.
    fn set_codex(&mut self, new: CodexSettingsDoc) {
        match &mut self.backend {
            Backend::Local(state) => {
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
                if let Some(path) = &state.config_path {
                    let _ = crate::config::update_path(path, |c| {
                        c.codex.default_model = new.model.clone();
                        c.codex.fast = new.fast;
                        c.codex.reasoning_effort = new.effort.clone();
                    });
                }
                self.set_status(codex_status_line(&new));
            }
            Backend::Remote(remote) => {
                remote.pending_codex = Some(new.clone());
                self.set_status(format!("applying {}…", codex_status_line(&new)));
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
            Ok(response) if response.status().is_success() => format!("{verb} {account}"),
            Ok(response) => format!("pause change failed: {}", response.status()),
            Err(err) => format!("pause change failed: {err}"),
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
                format!("limits updated for {account}")
            }
            Ok(response) => format!("limits change failed: {}", response.status()),
            Err(err) => format!("limits change failed: {err}"),
        };
        self.set_status(message);
    }

    /// Open the limits editor for the switcher's highlighted row (`L` in
    /// `Mode::Select`). Input format: `5h,7d,fbl` as percents (`90,98,98`),
    /// missing/empty positions keep no override, empty input = all-global.
    fn open_limits_editor(&mut self, idx: usize, view: Option<&DashboardView>) {
        let Some(view) = view else { return };
        let now = SystemTime::now();
        let order = view.display_order(now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        let fmt = |v: Option<f64>| v.map_or("global".to_string(), |v| format!("{:.0}%", v * 100.0));
        self.add_input.clear();
        self.mode = Mode::EditLimits { idx };
        self.set_status(format!(
            "limits for {} — enter `5h,7d,fbl` percents (now {}, {}, {}); empty = global; Enter apply, Esc cancel",
            target.id,
            fmt(target.limits.five_hour_max),
            fmt(target.limits.seven_day_max),
            fmt(target.limits.fable_weekly_max),
        ));
    }

    /// Key handling for `Mode::EditLimits`: plain text entry (digits, `,`,
    /// `.`, space), Enter parses + applies, Esc cancels back to the switcher.
    fn on_key_edit_limits(&mut self, code: KeyCode, idx: usize, view: Option<&DashboardView>) {
        match code {
            KeyCode::Char(c) if c.is_ascii_digit() || c == ',' || c == '.' || c == ' ' => {
                self.add_input.push(c);
            }
            KeyCode::Backspace => {
                self.add_input.pop();
            }
            KeyCode::Esc => {
                self.add_input.clear();
                self.mode = if std::mem::take(&mut self.limits_from_menu) {
                    Mode::Normal
                } else {
                    Mode::Select { idx }
                };
            }
            KeyCode::Enter => {
                let raw = std::mem::take(&mut self.add_input);
                match parse_limits_input(&raw) {
                    Ok(limits) => {
                        self.apply_limits_selected(idx, view, limits);
                        self.mode = if std::mem::take(&mut self.limits_from_menu) {
                            Mode::Normal
                        } else {
                            Mode::Select { idx }
                        };
                    }
                    Err(err) => {
                        // Keep editing — restore the text so it can be fixed.
                        self.add_input = raw;
                        self.set_status(err);
                    }
                }
            }
            _ => {}
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
        let order = view.display_order(now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        let name = target.id.0.clone();
        match &mut self.backend {
            Backend::Local(state) => match state.set_account_limits(&name, limits) {
                Ok(true) => self.set_status(format!("limits updated for {name}")),
                Ok(false) => self.set_status(format!("account {name} not found")),
                Err(err) => self.set_status(format!("limits change failed: {err}")),
            },
            Backend::Remote(remote) => {
                remote.pending_limits = Some((name.clone(), limits));
                self.set_status(format!("updating limits for {name}…"));
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
        let order = view.display_order(now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        let name = target.id.0.clone();
        let next = !target.paused;
        match &mut self.backend {
            Backend::Local(state) => {
                let verb = if next { "paused" } else { "resumed" };
                match state.set_account_paused(&name, next) {
                    Ok(true) => self.set_status(format!("{verb} {name}")),
                    Ok(false) => self.set_status(format!("account {name} not found")),
                    Err(err) => self.set_status(format!("pause change failed: {err}")),
                }
            }
            Backend::Remote(remote) => {
                remote.pending_pause = Some((name.clone(), next));
                self.set_status(format!(
                    "{} {name}…",
                    if next { "pausing" } else { "resuming" }
                ));
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
            Ok(response) if response.status().is_success() => codex_status_line(&new),
            Ok(response) => format!("codex change failed: {}", response.status()),
            Err(err) => format!("codex change failed: {err}"),
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
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('s') => self.mode = Mode::Normal,
            _ => self.mode = Mode::Select { idx },
        }
    }

    /// Open the new-login provider picker, OR — on a headless client that
    /// cannot open a browser — refuse with the `llmux login` fallback instead
    /// of starting a flow that would hang on the callback. "Headless" is
    /// decided by [`Self::can_open_browser`].
    fn open_new_login(&mut self) {
        if can_open_browser() {
            self.mode = Mode::NewLogin { idx: 0 };
            self.set_status(
                "new login: ↑↓ pick provider, Enter to open the browser, Esc to cancel".into(),
            );
        } else {
            self.mode = Mode::Normal;
            self.set_status(headless_login_hint(self.is_remote()));
        }
    }

    /// Key handling for `Mode::NewLogin` — the provider picker. Up/down move
    /// the cursor; Enter queues the chosen login for the event loop (which
    /// suspends the TUI, runs the browser flow, then re-inits); Esc cancels.
    fn on_key_new_login(&mut self, code: KeyCode, idx: usize) {
        let len = LoginKind::ALL.len();
        let idx = idx.min(len - 1);
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::NewLogin {
                    idx: idx.saturating_sub(1),
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.mode = Mode::NewLogin {
                    idx: (idx + 1).min(len - 1),
                };
            }
            KeyCode::Enter => {
                let kind = LoginKind::ALL[idx];
                self.pending_login = Some(kind);
                self.mode = Mode::Normal;
                self.set_status(format!("opening browser for {}…", kind.label()));
            }
            // Any other key cancels.
            _ => {
                self.mode = Mode::Normal;
                self.set_status("new login cancelled".into());
            }
        }
    }

    fn take_pending_login(&mut self) -> Option<LoginKind> {
        self.pending_login.take()
    }

    /// Key handling for `Mode::AddKey` — typing the new account's API key.
    /// Printable chars append to the buffer; Backspace deletes; Enter submits;
    /// Esc cancels. The buffer is never rendered raw (the footer shows a masked
    /// width via [`Chrome::add_input_len`]).
    fn on_key_add(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.add_input.clear();
                self.mode = Mode::Normal;
                self.set_status("add account cancelled".into());
            }
            KeyCode::Enter => self.submit_add(),
            KeyCode::Backspace => {
                self.add_input.pop();
            }
            // Guard against accidental control chars; only printable input.
            KeyCode::Char(c) if !c.is_control() => {
                self.add_input.push(c);
            }
            _ => {}
        }
    }

    /// Submit the typed API key: add the account in-process (local) or queue a
    /// `POST /llmux/add-account` (remote). The buffer is cleared either way so
    /// the secret does not linger in memory longer than necessary.
    fn submit_add(&mut self) {
        let api_key = self.add_input.trim().to_string();
        self.add_input.clear();
        self.mode = Mode::Normal;
        if api_key.is_empty() {
            self.set_status("add account cancelled: empty key".into());
            return;
        }
        match &mut self.backend {
            Backend::Local(state) => match state.add_apikey_account(None, &api_key) {
                // Status echoes the assigned NAME only — never the key.
                Ok((name, _outcome)) => self.set_status(format!("added account {name}")),
                Err(err) => self.set_status(format!("add account failed: {err}")),
            },
            Backend::Remote(remote) => {
                remote.pending_add = Some(api_key);
                self.set_status("adding account…".into());
            }
        }
    }

    /// Key handling for `Mode::ConfirmRemove` — a destructive delete gate.
    /// `y` confirms the removal; any other key cancels. Arrow/j/k move the
    /// target row so the operator can pick which account to delete.
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
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.submit_remove(idx, view);
                self.mode = Mode::Normal;
            }
            // Any other key (Esc/n/q/…) cancels — delete is never silent.
            _ => {
                self.mode = Mode::Normal;
                self.set_status("remove cancelled".into());
            }
        }
    }

    /// Resolve the display row to an account name, then remove it in-process
    /// (local) or queue a `POST /llmux/remove-account` (remote).
    fn submit_remove(&mut self, idx: usize, view: Option<&DashboardView>) {
        let Some(view) = view else { return };
        let now = SystemTime::now();
        // The cursor indexes DISPLAY rows (selection order), not config order.
        let order = view.display_order(now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        let name = target.id.0.clone();
        match &mut self.backend {
            Backend::Local(state) => match state.remove_account(&name) {
                Ok(true) => self.set_status(format!("removed account {name}")),
                Ok(false) => self.set_status(format!("account {name} not found")),
                Err(err) => self.set_status(format!("remove failed: {err}")),
            },
            Backend::Remote(remote) => {
                remote.pending_remove = Some(name.clone());
                self.set_status(format!("removing {name}…"));
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
                let name = response
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| v["name"].as_str().map(str::to_string))
                    .unwrap_or_else(|| "account".into());
                format!("added account {name}")
            }
            Ok(response) => format!("add account failed: {}", response.status()),
            Err(err) => format!("add account failed: {err}"),
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
            Ok(response) if response.status().is_success() => format!("removed account {name}"),
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let detail = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                    .unwrap_or_else(|| status.to_string());
                format!("remove {name} failed: {detail}")
            }
            Err(err) => format!("remove {name} failed: {err}"),
        };
        self.set_status(message);
    }

    /// Run a new browser login in THIS client and inject the resulting account
    /// into the daemon (issue #4). The OAuth flow (`login_interactive` /
    /// `login_codex_interactive`) opens the browser + binds a localhost
    /// callback HERE; the minted credential is then injected — in-process when
    /// local, via `POST /llmux/inject-account` when attached. ONE code path:
    /// the only fork is where the credential lands.
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
                        self.set_status(format!("login failed: {err}"));
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
                        self.set_status(format!("codex login failed: {err}"));
                        return;
                    }
                }
            }
        };

        // Inject: in-process locally, or relay to the daemon when attached.
        match &mut self.backend {
            Backend::Local(state) => match state.inject_account(account) {
                Ok((name, _outcome)) => self.set_status(format!("logged in: added {name}")),
                Err(err) => self.set_status(format!("login persist failed: {err}")),
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
                let name = response
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| v["name"].as_str().map(str::to_string))
                    .unwrap_or_else(|| account.name.clone());
                format!("logged in: added {name}")
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let detail = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                    .unwrap_or_else(|| status.to_string());
                format!("login inject failed: {detail}")
            }
            Err(err) => format!("login inject failed: {err}"),
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
                    self.set_status(msg);
                }
                Err(err) => self.set_status(format!("reload failed: {err}")),
            },
            Backend::Remote(_) => {
                self.set_status(
                    "reload: local mode only — restart applies on the server host".into(),
                );
            }
        }
    }

    /// Enter in select mode — switch the scheduler to the chosen account.
    ///
    /// The eligibility precheck runs here on the view's snapshot (same pure
    /// gate the scheduler uses), so the operator gets the real refusal
    /// reason immediately; the commit re-validates anyway (local:
    /// `AccountPool::switch_to` under the pool lock; remote: the server's
    /// switch endpoint runs the identical call).
    fn try_manual_switch(&mut self, idx: usize, view: Option<&DashboardView>) {
        let Some(view) = view else { return };
        let now = SystemTime::now();
        // The cursor indexes DISPLAY rows (selection order), not config order.
        let order = view.display_order(now);
        let Some(target) = order.get(idx).and_then(|&i| view.snapshot.accounts.get(i)) else {
            return;
        };
        if view.snapshot.is_current(&target.id) {
            self.set_status(format!("{} is already active", target.id));
            return;
        }
        let headers_only =
            select::headers_only_mode(&view.snapshot, &view.select_params, None, now);
        if let Some(reason) = select::eligibility(target, &view.select_params, now, headers_only) {
            if reason == select::IneligibleReason::Paused {
                self.set_status(format!(
                    "cannot switch to {}: paused — press p to resume",
                    target.id
                ));
            } else {
                self.set_status(format!("cannot switch to {}: {reason:?}", target.id));
            }
            return;
        }
        let target_id = target.id.clone();
        let from = view.snapshot.representative_current().cloned();
        match &mut self.backend {
            Backend::Local(state) => {
                match state.pool.switch_to(&target_id, &view.select_params, now) {
                    Ok(()) => {
                        state.emit(ActivityEvent::AccountSwitched {
                            from: from.map(|id| id.0),
                            to: target_id.0.clone(),
                            reason: Some("manual".into()),
                        });
                        self.set_status(format!("switched to {target_id} (manual)"));
                    }
                    Err(err) => self.set_status(format!("switch to {target_id} failed: {err}")),
                }
            }
            Backend::Remote(remote) => {
                remote.pending_switch = Some(target_id.0.clone());
                self.set_status(format!("switching to {target_id}…"));
            }
        }
    }

    /// Perform the queued remote switch (`POST /llmux/switch`).
    async fn perform_remote_switch(&mut self, target: String) {
        let Backend::Remote(remote) = &mut self.backend else {
            return;
        };
        let url = format!("{}/llmux/switch", remote.base_url);
        let mut request = remote
            .client
            .post(&url)
            .json(&serde_json::json!({ "account": target }));
        if let Some(key) = &remote.api_key {
            request = request.header("x-api-key", key);
        }
        let message = match request.send().await {
            Ok(response) if response.status().is_success() => {
                format!("switched to {target} (manual)")
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let detail = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                    .unwrap_or_else(|| status.to_string());
                format!("switch to {target} failed: {detail}")
            }
            Err(err) => format!("switch to {target} failed: {err}"),
        };
        self.set_status(message);
    }
}

/// Initialize the terminal for the dashboard: `ratatui::try_init` (raw mode +
/// alternate screen + a panic hook that restores them) PLUS native mouse
/// capture (Feature B), which `try_init` does NOT enable. Because the panic
/// hook installed by `try_init` only undoes raw-mode/alt-screen, we chain our
/// own hook BEFORE it that also disables mouse capture, so a panic on any path
/// leaves the terminal fully restored. Every call site uses this helper (and
/// [`restore_terminal`]) so the enable/disable always pair up — including the
/// login suspend/resume path that re-inits the terminal mid-session.
fn init_terminal() -> std::io::Result<ratatui::DefaultTerminal> {
    // Chain a mouse-disable into the panic hook BEFORE `try_init` installs its
    // own restore hook. `try_init`'s hook runs `restore()` then calls the
    // previous hook (this one), so on panic the order is: leave alt-screen +
    // raw mode, then disable mouse capture. Idempotent if it runs twice.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
        prev(info);
    }));
    let terminal = ratatui::try_init()?;
    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
    Ok(terminal)
}

/// Tear down the terminal: disable mouse capture FIRST, then `ratatui::restore`
/// (leave alternate screen + disable raw mode). The inverse of
/// [`init_terminal`]; runs on every normal exit path.
fn restore_terminal() {
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
}

/// Run the in-process dashboard over live server state until quit.
///
/// Terminal lifecycle via [`init_terminal`] / [`restore_terminal`]: raw mode +
/// alternate screen + mouse capture, all undone on every exit path (and the
/// panic hook).
pub async fn run_local(state: crate::proxy::server::AppState) -> std::io::Result<()> {
    let mut terminal = init_terminal()?;
    let mut app = App::new(Backend::Local(Box::new(state)));
    let result = event_loop(&mut terminal, &mut app, None).await;
    restore_terminal();
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
    let mut app = App::new(Backend::Remote(Box::new(Remote {
        client,
        base_url: opts.base_url,
        api_key: opts.api_key,
        pid: opts.pid,
        doc: None,
        connected: false,
        pending_switch: None,
        pending_codex: None,
        pending_pause: None,
        pending_limits: None,
        pending_mode: None,
        pending_add: None,
        pending_remove: None,
        pending_grok: None,
    })));
    let result = event_loop(&mut terminal, &mut app, Some(rx)).await;
    restore_terminal();
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
    // Input is event-driven, not polled: `EventStream` parks on the terminal fd
    // (mio) and only wakes the task when a real key/mouse/resize/paste arrives.
    // At idle (no input) this contributes zero wakeups, unlike a fixed-interval
    // poll which fired ~30×/s reading nothing. See issue #14 (idle quiescence).
    let mut events = EventStream::new();

    loop {
        let mut redraw = tokio::select! {
            _ = render.tick() => {
                app.frame = app.frame.wrapping_add(1);
                true
            }
            // Wakes only when the terminal actually has an event. Handle the one
            // the stream delivered, then drain any *already-ready* siblings (a
            // multi-byte paste) without blocking, so a burst is one redraw.
            Some(event) = events.next() => drain_input(app, event?)?,
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
        };
        if let Some(req) = app.take_pending_raw() {
            app.spawn_raw_fetch(req);
            redraw = true;
        }
        if let Some(req) = app.next_clip_if_idle() {
            app.spawn_clip(req);
            redraw = true;
        }
        if let Some(target) = app.take_pending_switch() {
            app.perform_remote_switch(target).await;
            redraw = true;
        }
        if let Some(codex) = app.take_pending_codex() {
            app.perform_remote_codex(codex).await;
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
        // run the flow, then re-init and force a full redraw. The fetch poller
        // (remote mode) keeps running in the background meanwhile.
        if let Some(kind) = app.take_pending_login() {
            restore_terminal();
            app.perform_login(kind).await;
            *terminal = init_terminal()?;
            let _ = terminal.clear();
            redraw = true;
        }
        if app.should_quit {
            return Ok(());
        }
        if redraw {
            let view = app.view(SystemTime::now());
            // Anchor a scrolled-into-history viewport against newly arrived rows
            // (UI-6 item 4) before `chrome()` snapshots the scroll offset.
            if let Some(view) = view.as_ref() {
                app.preserve_scroll_on_new_activity(view);
            }
            let chrome = app.chrome();
            // Capture the activity panel's hit-test layout from this frame so a
            // left-click in the next input drain maps to the right entry.
            let mut hits = None;
            terminal.draw(|frame| ui::draw(frame, view.as_ref(), &chrome, &mut hits))?;
            let main = hits.unwrap_or_default();
            app.activity_chrome = main.activity;
            app.tab_chrome = main.tabs;
            app.sessions_chrome = main.sessions_table;
            app.raw_chrome = main.raw_modal.clone().unwrap_or_default();
            app.separator_chrome = main.separators;
            app.account_row_chrome = main.account_rows;
            app.menu_chrome = main.menu;
            app.setting_chrome = main.settings;
            // Reconcile the input modal (UI-6 item 3) against what the frame
            // could actually render: `Some(max)` clamps the scroll to the
            // wrapped line count, `None` means the entry aged out of the ring
            // (lookup failed) so the modal closes gracefully.
            if app.input_modal.is_some() {
                match main.input_modal_max_scroll {
                    Some(max) => {
                        if let Some(modal) = app.input_modal.as_mut() {
                            modal.scroll = modal.scroll.min(max);
                        }
                    }
                    None => app.input_modal = None,
                }
            }
            // Clamp the raw viewer's scroll offsets against what this frame
            // rendered (UI-7/UI-8). Unlike the input modal, no draw ⇒ no
            // clamp — the modal owns its content and never closes on aging.
            if let (Some(modal), Some(raw)) = (app.raw_modal.as_mut(), main.raw_modal.as_ref()) {
                modal.scroll = modal.scroll.min(raw.max_scroll.0);
                modal.hscroll = modal.hscroll.min(raw.max_scroll.1);
            }
        }
    }
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
            app.on_key(key, view.as_ref());
            *dirty = true;
        }
        Event::Mouse(mouse) => {
            let view = view.get_or_insert_with(|| app.view(SystemTime::now()));
            if app.on_mouse(mouse, view.as_ref()) {
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

/// Lines the input modal (UI-6 item 3) scrolls per PgUp/PgDn keystroke.
const MODAL_PAGE: u16 = 10;

/// Horizontal pan step for the raw viewer (UI-8), in display cells.
const RAW_PAN: u16 = 8;

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

/// Streaming, progressive variant of the session load: reads the persisted
/// raw-io log line by line, accumulating parsed records, and every
/// `SESSIONS_CHUNK_RECORDS` (and always at EOF) folds the ACCUMULATED records
/// and delivers a partial over `tx`. The final partial carries `done = true`.
/// A missing/unreadable file delivers a single empty, done partial so the
/// overlay's loading state always clears. Runs on the blocking pool.
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
    let Ok(file) = std::fs::File::open(&path) else {
        send_empty_done();
        return;
    };
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = std::io::BufReader::new(file);
    let mut records: Vec<crate::proxy::raw_io::RawIoRecord> = Vec::new();
    let mut line = String::new();
    let mut bytes_read: u64 = 0;
    let mut since_fold: usize = 0;
    loop {
        line.clear();
        // `read_line` keeps the newline so `bytes_read` tracks real file offset
        // for an accurate `pct`.
        match std::io::BufRead::read_line(&mut reader, &mut line) {
            Ok(0) => break, // EOF
            Ok(n) => bytes_read = bytes_read.saturating_add(n as u64),
            Err(_) => break, // truncated/unreadable tail → fold what we have
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<crate::proxy::raw_io::RawIoRecord>(trimmed) {
            records.push(rec);
            since_fold += 1;
        }
        if since_fold >= SESSIONS_CHUNK_RECORDS {
            since_fold = 0;
            let partial = SessionsLoad {
                sessions: crate::session::fold_sessions(&records),
                done: false,
                pct: sessions_load_pct(bytes_read, file_len),
            };
            // Receiver gone (overlay closed / app exiting) → stop early.
            if tx.blocking_send(partial).is_err() {
                return;
            }
        }
    }
    // Final fold at EOF — always delivered (even for an empty file) so the
    // loading state clears.
    let _ = tx.blocking_send(SessionsLoad {
        sessions: crate::session::fold_sessions(&records),
        done: true,
        pct: 100,
    });
}

/// Parse the limits-editor input: comma/space-separated percents in order
/// `5h, 7d, fbl` — `"90,98,98"`, `"90"` (5h only), `"90,,98"` (skip 7d),
/// `""` (all global). Values are percents; `>1` divides by 100, `<=1` is
/// taken as a fraction, so `0.9` and `90` mean the same ceiling.
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
            pending_switch: None,
            pending_codex: None,
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

    #[test]
    fn login_kind_labels_distinguish_providers() {
        assert!(LoginKind::Anthropic.label().contains("Anthropic"));
        assert!(LoginKind::Codex.label().contains("Codex"));
        assert_eq!(LoginKind::ALL.len(), 2);
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
        app.tab_chrome = vec![
            ui::TabHit {
                area: ratatui::layout::Rect {
                    x: 1,
                    y: 2,
                    width: 9,
                    height: 1,
                },
                overlay: Overlay::None,
            },
            ui::TabHit {
                area: ratatui::layout::Rect {
                    x: 13,
                    y: 2,
                    width: 8,
                    height: 1,
                },
                overlay: Overlay::Config,
            },
        ];
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
        app.separator_chrome = vec![ui::SeparatorHit {
            y: 10,
            pane: ui::PaneId::Accounts,
            pane_top: 3,
        }];
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
        app.account_row_chrome = vec![ui::AccountRowHit {
            area: row_rect,
            display_idx: 0,
        }];
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
        app.account_row_chrome = vec![ui::AccountRowHit {
            area: row_rect,
            display_idx: 0,
        }];
        assert!(app.on_mouse(rclick, Some(&view)));
        app.on_key(press(KeyCode::Down), Some(&view));
        app.on_key(press(KeyCode::Down), Some(&view));
        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(app.mode, Mode::EditLimits { idx: 0 });
        app.on_key(press(KeyCode::Esc), Some(&view));
        assert_eq!(app.mode, Mode::Normal);

        // delete (item 3): opens the destructive confirm, never silent.
        let mut app = remote_app();
        app.account_row_chrome = vec![ui::AccountRowHit {
            area: row_rect,
            display_idx: 0,
        }];
        assert!(app.on_mouse(rclick, Some(&view)));
        app.on_key(press(KeyCode::End), Some(&view)); // unknown key → closes
        assert_eq!(app.mode, Mode::Normal, "unknown key dismisses");
        assert!(app.on_mouse(rclick, Some(&view)));
        for _ in 0..3 {
            app.on_key(press(KeyCode::Down), Some(&view));
        }
        app.on_key(press(KeyCode::Enter), Some(&view));
        assert_eq!(app.mode, Mode::ConfirmRemove { idx: 0 });

        // Click outside the open menu dismisses it.
        let mut app = remote_app();
        app.account_row_chrome = vec![ui::AccountRowHit {
            area: row_rect,
            display_idx: 0,
        }];
        assert!(app.on_mouse(rclick, Some(&view)));
        app.menu_chrome = Some(ui::MenuChrome {
            area: ratatui::layout::Rect {
                x: 10,
                y: 6,
                width: 14,
                height: 5,
            },
            items: vec![],
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
        app.setting_chrome = vec![
            ui::SettingHit {
                area: ratatui::layout::Rect {
                    x: 0,
                    y: 30,
                    width: 10,
                    height: 1,
                },
                action: ui::SettingAction::CodexEffort,
            },
            ui::SettingHit {
                area: ratatui::layout::Rect {
                    x: 20,
                    y: 30,
                    width: 6,
                    height: 1,
                },
                action: ui::SettingAction::GrokEffort,
            },
        ];
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
        // Cycling from the top of the grok list wraps back to bypass.
        view.grok.effort = Some("high".into());
        assert!(app.on_mouse(click(22), Some(&view)));
        assert_eq!(app.take_pending_grok(), Some(None), "high wraps to bypass");
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
        app.account_row_chrome = vec![ui::AccountRowHit {
            area: ratatui::layout::Rect {
                x: 0,
                y: 5,
                width: 80,
                height: 1,
            },
            display_idx: 0,
        }];
        let rclick = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.on_mouse(rclick, Some(&view)));
        assert_eq!(app.menu_account.as_deref(), Some("claude:a@x.com"));

        // Reorder: `a` moves to display row 1. Running pause (item 1) must
        // still act on `a`, not on whoever now sits at row 0.
        view.snapshot.accounts.swap(0, 1);
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

    #[test]
    fn misc_and_config_keys_round_trip() {
        let mut app = remote_app();
        app.on_key_main(KeyCode::Char('?'), None);
        assert_eq!(app.overlay, Overlay::Misc);
        app.on_key_misc(KeyCode::Esc);
        assert_eq!(app.overlay, Overlay::None);
        app.on_key_main(KeyCode::Char('c'), None);
        assert_eq!(app.overlay, Overlay::Config);
        app.on_key_config(KeyCode::Char('c'));
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
        app.on_key_add(KeyCode::Esc); // cancel back to Normal
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
        assert_eq!(app.add_input, "x");
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

    /// Seed `app.activity_chrome` with one clickable request row spanning the
    /// given screen rows, returning its key.
    fn seed_one_hit(app: &mut App) -> activity::ActivityKey {
        let key = activity::ActivityKey {
            at_ms: 42,
            method: "POST".into(),
            path: "/v1/messages".into(),
            status: 200,
        };
        app.activity_chrome = ui::ActivityChrome {
            area: ratatui::layout::Rect {
                x: 0,
                y: 5,
                width: 80,
                height: 10,
            },
            hits: vec![ui::ActivityHit {
                key: key.clone(),
                y_start: 6,
                height: 1,
                kind: ui::ActivityHitKind::Entry,
            }],
        };
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
        let key = seed_one_hit(&mut app);
        app.activity_chrome.hits[0].kind = ui::ActivityHitKind::RunHeader { expanded: false };
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
        let key = seed_one_hit(&mut app);
        // Make the seeded hit the 🔍 input detail line.
        app.activity_chrome.hits[0].kind = ui::ActivityHitKind::InputLine;
        // Click it → opens the modal on that key WITHOUT expanding the row.
        let changed = app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 10, 6), None);
        assert!(changed, "opening the modal warrants a redraw");
        assert_eq!(app.input_modal.as_ref().map(|m| &m.key), Some(&key));
        assert_eq!(
            app.expanded_activity, None,
            "the input click does not expand"
        );
        // A click beneath is now swallowed: nothing toggles, the modal stays.
        app.activity_chrome.hits[0].kind = ui::ActivityHitKind::Entry;
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
        // Arrows adjust the offset (clamped post-draw against the render pass).
        app.on_key(press(KeyCode::Down), None);
        assert_eq!(app.input_modal.as_ref().unwrap().scroll, 1);
        app.on_key(press(KeyCode::Up), None);
        assert_eq!(app.input_modal.as_ref().unwrap().scroll, 0);
        app.on_key(press(KeyCode::PageDown), None);
        assert_eq!(app.input_modal.as_ref().unwrap().scroll, MODAL_PAGE);
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
            hscroll: 5,
            spin: 0,
            flash: None,
            state: RawModalState::Ready(std::sync::Arc::new(content)),
        });
        // Tab advances and resets BOTH offsets (tabs have independent sizes).
        app.on_key(press(KeyCode::Tab), None);
        let modal = app.raw_modal.as_ref().unwrap();
        assert_eq!(modal.tab, 1);
        assert_eq!(modal.scroll, 0);
        assert_eq!(modal.hscroll, 0);
        // ← wraps back around the 2-tab ring; H/L pan horizontally.
        app.on_key(press(KeyCode::Left), None);
        assert_eq!(app.raw_modal.as_ref().unwrap().tab, 0);
        app.on_key(press(KeyCode::Char('L')), None);
        assert_eq!(app.raw_modal.as_ref().unwrap().hscroll, RAW_PAN);
        app.on_key(press(KeyCode::Char('H')), None);
        assert_eq!(app.raw_modal.as_ref().unwrap().hscroll, 0);
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
            hscroll: 0,
            spin: 0,
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
            hscroll: 0,
            spin: 0,
            flash: None,
            state: RawModalState::Loading,
        });
        // A stale export result (older generation) never flashes.
        app.apply_clip_result(ClipResult {
            generation: 8,
            message: "stale".into(),
        });
        assert!(app.raw_modal.as_ref().unwrap().flash.is_none());
        // The requesting generation's result flashes.
        app.apply_clip_result(ClipResult {
            generation: 9,
            message: "copied 4 bytes → pbcopy".into(),
        });
        assert_eq!(
            app.raw_modal.as_ref().unwrap().flash.as_ref().unwrap().0,
            "copied 4 bytes → pbcopy"
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
                method: "POST".into(),
                path: "/v1/messages".into(),
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
                width: 0,
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
            hscroll: 0,
            spin: 0,
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
                width: 0,
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
            hscroll: 0,
            spin: 0,
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
            "export busy — try again in a moment",
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
            "http://oudwood-512:3456",
            "http://100.98.240.111:3456",
            "http://[2001:db8::1]:3456",
            "not a url",
        ] {
            assert!(!base_url_is_loopback(url), "{url} is another host");
        }
        // And the App-level gate wires it up: a cross-host remote never arms.
        let mut app = remote_app();
        if let Backend::Remote(remote) = &mut app.backend {
            remote.base_url = "http://oudwood-512:3456".into();
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
            usage_stats: Vec::new(),
            health: Default::default(),
            session_labels: Default::default(),
            pid: 1,
            uptime: Duration::from_secs(1),
            port: 3456,
            upstream: None,
            config_path: None,
            select_params: select::SelectParams {
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
}
