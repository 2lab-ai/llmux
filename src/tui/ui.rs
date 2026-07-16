//! Frame rendering: header (identity/port/uptime/attach banner), account
//! table in selection order, scheduler/poller/totals pane + selected-account
//! detail, activity log, log console, footer keybar. Pure projection of a
//! [`DashboardView`] (data) + [`Chrome`] (UI-local cursor/panes/status) — no
//! state mutation here, and no knowledge of where the view came from (local
//! `AppState` or a fetched document), so the renderer is never forked.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Borders, Cell, Chart, Clear, Dataset, GraphType, Paragraph, Row, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Table, Wrap,
};
use ratatui::Frame;
use unicode_segmentation::UnicodeSegmentation;

use crate::dashboard::ModelUsageDoc;
use crate::logging::LogLine;
use crate::routing::BackendGroup;
use crate::scheduler::select::IneligibleReason;
use crate::scheduler::window::{
    classify_window_display, QuotaWindow, ScopedQuotaWindow, WindowDisplayState,
};
use crate::scheduler::{select, AccountSnapshot};

use super::activity::{ActivityKey, Completed, CompletedBody, InFlight};
use super::event::TokenCounts;
use super::format::{self, GaugeLevel};
use super::triage::{self, ActivityRow, VerdictLevel};
use super::view::DashboardView;
use super::{anim, Chrome, InputModal, Mode, Overlay, RawModal, RawModalState};

/// Total width of one quota gauge cell in the accounts table: a reverse-video
/// bar (fill = utilization, reset countdown / absolute stamp overlaid inside),
/// one separator space, and a right-aligned percent label — both facts,
/// numeric and spatial, at once. 17 columns (bar 11, space, label 5).
const QUOTA_CELL_WIDTH: usize = QUOTA_BAR_WIDTH + 1 + QUOTA_LABEL_WIDTH;
/// Right-aligned percent label slot inside the quota cell: worst case
/// `100%!` (the parked/over `!` rides the label, as it always did).
const QUOTA_LABEL_WIDTH: usize = 5;
/// The bar portion of the quota cell: 11 columns — sized to the WIDEST text
/// the bar hosts, the absolute reset stamp `MM/DD HH:MM` (`t` toggle, exactly
/// 11 chars); the top-2-unit countdown ("10h 15m" + state glyph) also fits.
const QUOTA_BAR_WIDTH: usize = 11;
/// Cap on the quota gauge bar width when leftover terminal width is poured into
/// it (Z 2026-07-13): cap so ultra-wide terminals don't render comedy bars;
/// ~3× the base keeps the absolute reset stamp readable. The floor stays
/// `QUOTA_BAR_WIDTH` (no leftover → today's exact layout).
const GAUGE_BAR_MAX: usize = 32;
/// Width at/above which the MODELS table shows the wide column set. The
/// accounts table no longer uses this: it computes whether the wide column
/// set actually fits (Z 2026-07-13 — the fixed 150 threshold predated the
/// NAME_COL_MAX cap and caused a 1-col layout flap at 149/150).
const WIDE_TABLE_AT: u16 = 150;
/// Max width of the accounts-table name column (Z 2026-07-13: cap at 20 — the
/// leftover-space allocation made it too wide). Longer names are clipped by the
/// cell.
const NAME_COL_MAX: u16 = 20;
/// Width at/above which the middle row fits summary + detail side by side.
const SIDE_BY_SIDE_AT: u16 = 110;
/// Default rows shown in the always-visible compact model strip (req12; Z
/// 2026-07-15 UI-4 V1: 3 → 5). This sets only the AUTO pane height — the
/// strip renders as many rows as its area holds, so dragging the pane
/// taller (U8) reveals more (UI-4 V2). Width of its token-share mini-bar.
const MODEL_STRIP_ROWS: usize = 5;
const MODEL_BAR_WIDTH: usize = 10;
/// Rows shown in the compact per-client attribution panel in the stats overlay
/// (issue #32) — the top N clients by request count.
const CLIENT_PANEL_ROWS: usize = 6;
/// A model used within this window counts as "recently active" (req15).
const MODEL_RECENT_WINDOW: Duration = Duration::from_secs(60);
/// Max heatmap cells shown at once (issue #23). The rows are sorted by tokens
/// desc, so the busiest (group, model, account) cells stay visible; the panel
/// title reports the total when more exist.
const HEATMAP_MAX_ROWS: usize = 8;
/// Width of the heatmap's token-intensity mini-bar.
const HEATMAP_BAR_WIDTH: usize = 12;

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn level_color(level: GaugeLevel) -> Color {
    match level {
        GaugeLevel::Green => Color::Green,
        GaugeLevel::Yellow => Color::Yellow,
        GaugeLevel::Red => Color::Red,
    }
}

/// Format an API-equivalent USD cost (Feature D) for display: `≥$1` keeps two
/// decimals (`$3.78`), a sub-dollar amount keeps four (`$0.0123`) so small
/// per-request costs are still legible, and exactly zero renders `$0.0000`.
fn format_cost(usd: f64) -> String {
    if usd == 0.0 {
        "$0.0000".to_string()
    } else if usd >= 1.0 {
        format!("${usd:.2}")
    } else {
        format!("${usd:.4}")
    }
}

/// Everything a row/pane needs that is derived once per frame.
struct FrameCtx {
    now: SystemTime,
    /// Local UTC offset for absolute time labels.
    tz_offset: i64,
    /// Indices into `view.snapshot.accounts` in scheduler preference order.
    order: Vec<usize>,
    headers_only: bool,
    /// Monotonic animation frame counter (drives `anim` glyphs).
    frame: usize,
    /// Live `email_anonymous` display setting (SSOT E4) — carried here so
    /// surfaces that render from `chrome` only (sessions overlay, footer
    /// status) can mask without a `view` handle.
    mask: bool,
    /// Effective quota-gauge fill direction this frame: the `u`-key session
    /// override when set, else the config default carried on the view.
    quota_display: crate::config::QuotaDisplay,
    /// `t`-key session toggle: quota bars show the reset as an absolute UTC
    /// stamp (`07/07 13:50`) instead of the countdown.
    reset_absolute: bool,
}

/// Display form of an account name under the `email_anonymous` setting:
/// masked through the deterministic demo alias when on, raw otherwise. Render
/// layer ONLY — the view snapshot keeps real ids so switch/remove still
/// address the pool correctly.
fn masked_name(name: &str, mask: bool) -> String {
    if mask {
        crate::demo::alias_always(name)
    } else {
        name.to_string()
    }
}

/// Accounts-table display form of an account id: mask first (the
/// `email_anonymous` aliasing), then strip the `{group}:` prefix — the table's
/// `group` column already says CLAUDE/CODEX, so repeating it per name is pure
/// width — then abbreviate the email domain via config `domain_abbrev`
/// (`ai3@insightquest.io` → `ai3@iq.io`). Render layer ONLY, exactly like
/// [`masked_name`]: the snapshot keeps real ids so switch/remove still
/// address the pool correctly. The detail pane keeps the FULL raw id (it is
/// the fidelity surface, issue #70).
fn row_account_name(
    raw: &str,
    mask: bool,
    abbrev: &std::collections::BTreeMap<String, String>,
) -> String {
    let masked = masked_name(raw, mask);
    let bare = masked
        .split_once(':')
        .map_or(masked.as_str(), |(_, rest)| rest);
    match bare.split_once('@') {
        Some((local, domain)) => match abbrev.get(domain) {
            Some(short) => format!("{local}@{short}"),
            None => bare.to_string(),
        },
        None => bare.to_string(),
    }
}

/// Display form of free text that may EMBED emails (activity notes, tracing
/// lines, footer status): every email-looking token is aliased when masking
/// is on, the rest passes through untouched.
fn masked_text(text: &str, mask: bool) -> String {
    if mask {
        crate::demo::mask_email_text(text)
    } else {
        text.to_string()
    }
}

/// Top-level draw entry. `view` is `None` only in attach mode before the
/// first document arrives — then we paint a connecting screen + the footer,
/// never a half-rendered table.
pub(crate) fn draw(
    frame: &mut Frame,
    view: Option<&DashboardView>,
    chrome: &Chrome,
    hits: &mut Option<MainChrome>,
) {
    // No activity panel hit-targets until MAIN draws one this frame (cleared so
    // a stale layout from a previous frame can never mis-map a click).
    *hits = None;
    let Some(view) = view else {
        draw_connecting(frame, chrome);
        return;
    };

    let now = SystemTime::now();
    let ctx = FrameCtx {
        now,
        tz_offset: format::local_offset_secs(now),
        order: view.display_order(now),
        headers_only: select::headers_only_mode(&view.snapshot, &view.select_params, None, now),
        frame: chrome.frame,
        mask: view.email_anonymous,
        quota_display: chrome.quota_display_override.unwrap_or(view.quota_display),
        reset_absolute: chrome.reset_absolute,
    };

    // MAIN is the wall-clock view: ALWAYS drawn first, every frame, so it keeps
    // updating underneath any overlay (issue #5). Local and attach render from
    // the same `DashboardView`, so this path is never forked.
    draw_main(frame, view, &ctx, chrome, now, hits);

    // A summoned overlay (if any) is then drawn OVER MAIN. Each overlay clears
    // its own rect with `Clear` so MAIN shows through only outside it; because
    // MAIN was already drawn this frame, "MAIN keeps updating underneath" is
    // automatic. The rect is computed HERE (not per-overlay) so every overlay
    // shares the same top edge: right under the tab bar, banner-aware.
    let overlay_area = overlay_rect(frame.area(), event_banner_line(&view.events, now).is_some());
    match chrome.overlay {
        Overlay::None => {}
        Overlay::Accounts => draw_accounts_overlay(frame, overlay_area, view, &ctx, chrome),
        Overlay::Stats => draw_stats_overlay(frame, overlay_area, view, &ctx, chrome),
        Overlay::Usage => draw_usage_overlay(frame, overlay_area, view, chrome),
        Overlay::Logs => draw_logs_overlay(frame, overlay_area, view),
        Overlay::Sessions => draw_sessions_overlay(frame, overlay_area, &ctx, chrome, hits),
        Overlay::Misc => draw_misc_overlay(frame, overlay_area, view),
        Overlay::Perf => draw_perf_overlay(frame, overlay_area, view, &ctx, chrome),
        Overlay::Config => draw_config_overlay(frame, overlay_area, view, chrome, hits),
    }

    // The input modal (UI-6 item 3) draws LAST over MAIN + any overlay: a
    // centered, scrollable full-text box. Its max-scroll (or a close signal
    // when the entry aged out) rides back on the hit record so the runtime can
    // clamp/close. Drawn before the footer so the `q`/`esc` hint stays visible.
    if let Some(modal) = &chrome.input_modal {
        let max_scroll = draw_input_modal(frame, view, modal);
        if let Some(hits) = hits.as_mut() {
            hits.input_modal_max_scroll = max_scroll;
        }
    }

    // The raw request/response viewer (UI-7) draws over everything but the
    // footer — same layering contract as the input modal (the two are never
    // open at once: each swallows the clicks that could open the other).
    if let Some(modal) = &chrome.raw_modal {
        let raw_chrome = draw_raw_modal(frame, modal);
        if let Some(hits) = hits.as_mut() {
            hits.raw_modal = Some(raw_chrome);
        }
    }

    // The footer keybar is part of the chrome and reflects the active overlay /
    // mode; drawn last so it sits above everything.
    let footer_area = Rect {
        x: frame.area().x,
        y: frame.area().bottom().saturating_sub(2),
        width: frame.area().width,
        height: 2,
    };
    frame.render_widget(Clear, footer_area);
    draw_footer(frame, footer_area, chrome, ctx.mask);
}

/// MAIN — the always-rendered wall-clock view (issue #5): header banner ·
/// account quota table · scheduler/totals summary · compact per-model strip ·
/// in-flight + activity. No navigation, no overlay surfaces. The selected-
/// account detail pane and the full log console moved to the Accounts and Logs
/// overlays respectively; the model strip stays here.
/// Build the one-line event banner for the very top of MAIN from the configured
/// `events`, or `None` when none is active (then no row is reserved). An event
/// is ACTIVE while `from <= now < to`; among the active ones the banner shows
/// the single one with the EARLIEST `to`. Pretty and compact, all times LOCAL:
/// `Fable 5 Available until 7/12 · until 7/12 23:59 · 3d 15h 15m 25s left` — the
/// deadline is rendered in local time and the remaining time ticks with the
/// existing per-frame redraw. Bold content, dim separators — distinct but
/// tasteful, consistent with the rest of the TUI.
fn event_banner_line(
    events: &[crate::config::EventBanner],
    now: SystemTime,
) -> Option<Line<'static>> {
    // Active = from <= now < to; pick the one with the earliest `to`.
    let (to, event) = events
        .iter()
        .filter_map(|e| {
            let from = crate::event::parse_event_time(&e.from)?;
            let to = crate::event::parse_event_time(&e.to)?;
            (from <= now && now < to).then_some((to, e))
        })
        .min_by_key(|(to, _)| *to)?;
    // `now < to` guarantees a positive remaining.
    let remaining = to.duration_since(now).ok()?;
    let offset = format::local_offset_secs(to);
    let sep = Span::styled(" · ", dim());
    Some(Line::from(vec![
        Span::styled(
            event.content.clone(),
            Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ),
        sep.clone(),
        Span::styled("until ", dim()),
        Span::styled(
            format::month_day_hm(to, offset),
            Style::new().fg(Color::Cyan),
        ),
        sep,
        Span::styled(
            format!("{} left", format::remaining_hms(remaining)),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
    ]))
}

fn draw_main(
    frame: &mut Frame,
    view: &DashboardView,
    ctx: &FrameCtx,
    chrome: &Chrome,
    now: SystemTime,
    hits: &mut Option<MainChrome>,
) {
    let snapshot = &view.snapshot;
    // Event banner (config `events`): ONE line pinned to the very top while an
    // event is active; zero height (no reserved row) when none is active
    // (unparseable, before `from`, or past `to`).
    let event_line = event_banner_line(&view.events, now);
    let banner_height = u16::from(event_line.is_some());
    // Drag-set overrides (UI-3 U7/U8) replace the automatic heights; the
    // clamp keeps a dragged pane from collapsing below border+header.
    let table_height = chrome
        .pane_heights
        .accounts
        .map(|h| h.clamp(PANE_MIN_HEIGHT, PANE_MAX_HEIGHT))
        .unwrap_or_else(|| (snapshot.accounts.len().max(1) as u16).saturating_add(2));
    let middle_height = chrome
        .pane_heights
        .middle
        .map(|h| h.clamp(PANE_MIN_HEIGHT, PANE_MAX_HEIGHT))
        .unwrap_or(8);
    // Compact model strip (req12): only when model data exists. 0 height (no
    // pane) otherwise, so the idle layout is unchanged.
    let strip_rows = view.model_usage.len().min(MODEL_STRIP_ROWS);
    // +2 for the table's top border (title) and header row.
    let auto_strip_height = if strip_rows > 0 {
        strip_rows as u16 + 2
    } else {
        0
    };
    let strip_height = match chrome.pane_heights.strip {
        // A drag override only applies while the strip exists at all.
        Some(h) if auto_strip_height > 0 => h.clamp(PANE_MIN_HEIGHT, PANE_MAX_HEIGHT),
        _ => auto_strip_height,
    };
    let [banner_area, header_area, tabs_area, table_area, middle_area, strip_area, activity_area, groups_area, footer_area] =
        Layout::vertical([
            Constraint::Length(banner_height),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(table_height),
            Constraint::Length(middle_height),
            Constraint::Length(strip_height),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .areas(frame.area());

    if let Some(line) = event_line {
        frame.render_widget(Paragraph::new(line), banner_area);
    }
    draw_header(frame, header_area, view, chrome);
    let tabs = draw_tabs(frame, tabs_area, chrome.overlay);
    let account_rows = draw_accounts(frame, table_area, view, ctx, chrome);
    draw_middle(frame, middle_area, view, ctx, chrome);
    if strip_height > 0 {
        draw_models_strip(frame, strip_area, view, ctx, now);
    }
    let activity = draw_activity(frame, activity_area, view, chrome, now);
    // Drag separators (UI-3 U7/U8): each pane's TOP border row resizes the
    // pane above it. With no model strip the activity border resizes the
    // middle pane instead.
    let mut separators = vec![SeparatorHit {
        y: middle_area.y,
        pane: PaneId::Accounts,
        pane_top: table_area.y,
    }];
    if strip_height > 0 {
        separators.push(SeparatorHit {
            y: strip_area.y,
            pane: PaneId::Middle,
            pane_top: middle_area.y,
        });
        separators.push(SeparatorHit {
            y: activity_area.y,
            pane: PaneId::Strip,
            pane_top: strip_area.y,
        });
    } else {
        separators.push(SeparatorHit {
            y: activity_area.y,
            pane: PaneId::Middle,
            pane_top: middle_area.y,
        });
    }
    let settings = draw_group_settings(frame, groups_area, view);
    // The context menu (UI-3 U11) draws last so it floats over every pane.
    let menu = chrome
        .menu_anchor
        .filter(|_| matches!(chrome.mode, Mode::ContextMenu { .. }))
        .map(|anchor| draw_context_menu(frame, view, ctx, chrome, anchor));
    *hits = Some(MainChrome {
        activity,
        tabs,
        separators,
        account_rows,
        menu,
        sessions_table: None,
        config_rows: Vec::new(),
        settings,
        // Filled in by `draw` after the modal (if any) renders over MAIN.
        input_modal_max_scroll: None,
        raw_modal: None,
    });
    // Footer slot reserved in the layout; the real footer is drawn by `draw`
    // last (over any overlay). Keep MAIN's bottom row clear here.
    let _ = footer_area;
}

/// The group-settings bar (UI-3 U9/U10): one bottom row showing each backend
/// group's live settings; clicking a highlighted segment ROTATES that
/// setting (scheduler mode, codex model / effort / fast, grok effort).
/// Effort `bypass` = the client's own value rides through (UI-3 U12).
fn draw_group_settings(frame: &mut Frame, area: Rect, view: &DashboardView) -> Vec<SettingHit> {
    if area.height == 0 {
        return Vec::new();
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut hits: Vec<SettingHit> = Vec::new();
    let mut x = area.x;
    let push_plain = |spans: &mut Vec<Span<'static>>, x: &mut u16, text: String, style| {
        let w = text.chars().count() as u16;
        spans.push(Span::styled(text, style));
        *x += w;
    };
    let clickable = Style::new()
        .fg(Color::Cyan)
        .add_modifier(Modifier::UNDERLINED);
    let push_click = |spans: &mut Vec<Span<'static>>,
                      hits: &mut Vec<SettingHit>,
                      x: &mut u16,
                      text: String,
                      action: SettingAction| {
        let w = text.chars().count() as u16;
        hits.push(SettingHit {
            area: Rect {
                x: *x,
                y: area.y,
                width: w,
                height: 1,
            },
            action,
        });
        spans.push(Span::styled(text, clickable));
        *x += w;
    };
    let sep = |spans: &mut Vec<Span<'static>>, x: &mut u16| {
        spans.push(Span::styled("  │  ", dim()));
        *x += 5;
    };

    push_plain(&mut spans, &mut x, " sched ".into(), dim());
    push_click(
        &mut spans,
        &mut hits,
        &mut x,
        view.select_params.mode.label().to_string(),
        SettingAction::SchedMode,
    );

    let count = |g: BackendGroup| {
        view.snapshot
            .accounts
            .iter()
            .filter(|a| a.group == g)
            .count()
    };
    let claude_n = count(BackendGroup::Claude);
    if claude_n > 0 {
        sep(&mut spans, &mut x);
        push_plain(
            &mut spans,
            &mut x,
            "claude ".into(),
            group_color(Some("claude")).add_modifier(Modifier::BOLD),
        );
        push_plain(&mut spans, &mut x, format!("{claude_n} acc"), dim());
    }
    if view.codex.available {
        sep(&mut spans, &mut x);
        push_plain(
            &mut spans,
            &mut x,
            "codex ".into(),
            group_color(Some("codex")).add_modifier(Modifier::BOLD),
        );
        push_click(
            &mut spans,
            &mut hits,
            &mut x,
            view.codex.model.clone(),
            SettingAction::CodexModel,
        );
        push_plain(&mut spans, &mut x, " effort:".into(), dim());
        push_click(
            &mut spans,
            &mut hits,
            &mut x,
            view.codex.effort.clone().unwrap_or_else(|| "bypass".into()),
            SettingAction::CodexEffort,
        );
        push_plain(&mut spans, &mut x, " fast:".into(), dim());
        push_click(
            &mut spans,
            &mut hits,
            &mut x,
            if view.codex.fast { "on" } else { "off" }.into(),
            SettingAction::CodexFast,
        );
    }
    if view.grok.available {
        sep(&mut spans, &mut x);
        push_plain(
            &mut spans,
            &mut x,
            "grok ".into(),
            group_color(Some("grok")).add_modifier(Modifier::BOLD),
        );
        push_plain(&mut spans, &mut x, "effort:".into(), dim());
        push_click(
            &mut spans,
            &mut hits,
            &mut x,
            view.grok.effort.clone().unwrap_or_else(|| "bypass".into()),
            SettingAction::GrokEffort,
        );
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    hits
}

/// The tab strip (UI-3 U6): one row of clickable surface names right under
/// the header. The active surface renders reversed; every label's rect is
/// returned for the mouse hit-test. Keyboard shortcuts keep working — the
/// tabs are the mouse-native mirror of `a`/`g`/`l`/`s`/`?`/`c`.
fn draw_tabs(frame: &mut Frame, area: Rect, active: Overlay) -> Vec<TabHit> {
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let mut tabs: Vec<TabHit> = Vec::new();
    let mut x = area.x + 1;
    for (i, (label, overlay)) in TABS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", dim()));
            x += 3;
        }
        let style = if *overlay == active {
            Style::new()
                .fg(Color::Cyan)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new().fg(Color::Cyan)
        };
        let width = label.chars().count() as u16;
        spans.push(Span::styled(*label, style));
        tabs.push(TabHit {
            area: Rect {
                x,
                y: area.y,
                width,
                height: 1,
            },
            overlay: *overlay,
        });
        x += width;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    tabs
}

/// Accounts overlay (`a`): a near-full-screen surface giving the account quota
/// table the priority slot plus the selected-account detail pane, over which
/// the add/remove/switch/login interactions (issues #3/#4) run. Cleared so MAIN
/// shows through only at the very edges. The quota table's own titled border is
/// the overlay's top separator — the old extra bold " accounts " header line
/// stacked a THIRD "accounts" row under the tab label (Z 2026-07-16).
fn draw_accounts_overlay(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    chrome: &Chrome,
) {
    frame.render_widget(Clear, area);
    let snapshot = &view.snapshot;
    let table_height = (snapshot.accounts.len().max(1) as u16).saturating_add(2);
    let [table_area, detail_area] =
        Layout::vertical([Constraint::Length(table_height), Constraint::Min(3)]).areas(area);
    let _ = draw_accounts(frame, table_area, view, ctx, chrome);
    if snapshot.accounts.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "no accounts — press a to add an API key, n to start a browser login",
            Style::new().fg(Color::Yellow),
        )))
        .block(Block::new().borders(Borders::TOP).title(" detail "));
        frame.render_widget(empty, detail_area);
    } else {
        draw_detail(frame, detail_area, view, ctx, chrome);
    }
}

/// Stats overlay (`g`): the detailed per-model usage table + drill-down (req13;
/// was the `show_models` full view). Keeps the account quota table above it for
/// context, matching the old layout.
fn draw_stats_overlay(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    chrome: &Chrome,
) {
    frame.render_widget(Clear, area);
    let snapshot = &view.snapshot;
    let table_height = (snapshot.accounts.len().max(1) as u16).saturating_add(2);
    // Reserve a bottom slice for the windowed (24h/72h) per-account/per-model
    // token heatmap (issue #23). The heatmap height tracks the visible cells,
    // capped so the model table/drill-down above always keep room.
    let heatmap_height = heatmap_panel_height(view, chrome.stats_window, area.height);
    // Tokens-per-Day chart slice (UI-3 U14): shown whenever daily data exists
    // and the overlay is tall enough to keep the model table readable.
    let chart_height = if view.daily_usage.is_empty() || area.height < 31 {
        0
    } else {
        DAILY_CHART_HEIGHT
    };
    let [table_area, chart_area, body_area, heatmap_area] = Layout::vertical([
        Constraint::Length(table_height),
        Constraint::Length(chart_height),
        Constraint::Min(3),
        Constraint::Length(heatmap_height),
    ])
    .areas(area);
    if chart_height > 0 {
        draw_daily_chart(frame, chart_area, view, ctx, chrome.chart_days);
    }
    let _ = draw_accounts(frame, table_area, view, ctx, chrome);
    // Reserve a compact per-client attribution panel (issue #32) at the bottom
    // of the stats body when there is client usage to show; otherwise the
    // models view keeps the whole body. The windowed heatmap (issue #23) always
    // renders in its own reserved slice below.
    if view.client_usage.is_empty() {
        draw_models_full(frame, body_area, view, ctx, chrome);
    } else {
        let clients_height = (view.client_usage.len().min(CLIENT_PANEL_ROWS) as u16)
            .saturating_add(2)
            .min(body_area.height.saturating_sub(3).max(2));
        let [models_area, clients_area] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(clients_height)])
                .areas(body_area);
        draw_models_full(frame, models_area, view, ctx, chrome);
        draw_clients_compact(frame, clients_area, view);
    }
    draw_heatmap(frame, heatmap_area, view, chrome.stats_window);
}

/// Height of the Tokens-per-Day chart slice in the Stats overlay (UI-3
/// U14): border/title + plot rows + x-axis labels + legend line.
const DAILY_CHART_HEIGHT: u16 = 13;
/// Day spans the Tokens-per-Day chart cycles through (`d` in the Stats
/// overlay — UI-3 U14 period selection).
pub(crate) const DAILY_CHART_SPANS: [u64; 4] = [7, 14, 30, 90];
/// Max model lines plotted at once (top by window tokens); more would turn
/// minimal into noise.
const DAILY_CHART_SERIES: usize = 4;
/// Line colors by rank — chosen to stay distinct on dark terminals and to
/// echo the group hues used elsewhere.
const DAILY_CHART_COLORS: [Color; DAILY_CHART_SERIES] =
    [Color::Magenta, Color::Cyan, Color::Yellow, Color::Green];

/// Tokens-per-Day line chart (UI-3 U14): one braille line per top model over
/// the selected trailing span (`d` cycles [`DAILY_CHART_SPANS`]),
/// modern-minimal — no grid, dim axes,
/// a single legend line with colored dots. Data = the daily fold carried on
/// the document (history filled by the persisted-request replay).
fn draw_daily_chart(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    chart_days: u64,
) {
    let chart_days = chart_days.clamp(2, 366);
    let today = ctx
        .now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0);
    let start = today.saturating_sub(chart_days.saturating_sub(1));
    // Rank models by window total, then build one (x=day-offset, y=tokens)
    // series per top model, zero-filled across the whole span so a quiet day
    // reads as a drop to zero, not a gap.
    // Bounded on BOTH ends: a future-dated replay row (clock skew) must not
    // index past the series (review R1 MUST-FIX 2).
    let in_span = |r: &&crate::dashboard::DailyUsageDoc| r.day >= start && r.day <= today;
    let mut totals: std::collections::HashMap<(&str, &str), u64> = Default::default();
    for r in view.daily_usage.iter().filter(&in_span) {
        *totals
            .entry((r.group.as_str(), r.model.as_str()))
            .or_default() += r.tokens_in + r.tokens_out + r.cache_read + r.cache_creation;
    }
    let mut ranked: Vec<((&str, &str), u64)> = totals.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(DAILY_CHART_SERIES);
    if ranked.is_empty() {
        return;
    }
    let grand: u64 = ranked.iter().map(|(_, t)| *t).sum::<u64>().max(1);
    let days = chart_days as usize;
    let mut series: Vec<Vec<(f64, f64)>> = vec![vec![(0.0, 0.0); days]; ranked.len()];
    for (s, serie) in series.iter_mut().enumerate() {
        for (i, point) in serie.iter_mut().enumerate() {
            point.0 = i as f64;
        }
        let _ = s;
    }
    let mut y_max = 0f64;
    for r in view.daily_usage.iter().filter(&in_span) {
        if let Some(idx) = ranked
            .iter()
            .position(|((g, m), _)| *g == r.group && *m == r.model)
        {
            let x = (r.day - start) as usize;
            let y = (r.tokens_in + r.tokens_out + r.cache_read + r.cache_creation) as f64;
            series[idx][x].1 += y;
            y_max = y_max.max(series[idx][x].1);
        }
    }
    let y_max = y_max.max(1.0);

    let datasets: Vec<Dataset> = series
        .iter()
        .enumerate()
        .map(|(i, data)| {
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(DAILY_CHART_COLORS[i]))
                .data(data)
        })
        .collect();
    let day_label = |d: u64| {
        let at = UNIX_EPOCH + Duration::from_secs(d * 86_400);
        let full = format::month_day_hm(at, 0);
        full.split(' ').next().unwrap_or("").to_string()
    };
    let x_axis = Axis::default()
        .bounds([0.0, (days - 1) as f64])
        .labels(vec![
            Span::styled(day_label(start), dim()),
            Span::styled(day_label(start + chart_days / 2), dim()),
            Span::styled(day_label(today), dim()),
        ])
        .style(dim());
    let y_axis = Axis::default()
        .bounds([0.0, y_max])
        .labels(vec![
            Span::styled("0", dim()),
            Span::styled(format::human_count((y_max / 2.0) as u64), dim()),
            Span::styled(format::human_count(y_max as u64), dim()),
        ])
        .style(dim());
    let [plot_area, legend_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let chart = Chart::new(datasets)
        .block(
            Block::new()
                .borders(Borders::TOP)
                .title(format!(" tokens per day — last {chart_days}d · d cycles ")),
        )
        .x_axis(x_axis)
        .y_axis(y_axis);
    frame.render_widget(chart, plot_area);
    // Legend: ● model (share%) — one line, dim separators.
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    for (i, ((group, model), total)) in ranked.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(Span::styled(
            "● ".to_string(),
            Style::new().fg(DAILY_CHART_COLORS[i]),
        ));
        spans.push(Span::raw(format!("{} ", abbrev_model(Some(group), model))));
        spans.push(Span::styled(
            format!("({:.1}%)", *total as f64 * 100.0 / grand as f64),
            dim(),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), legend_area);
}

/// Compact per-client request-attribution table (issue #32): top
/// [`CLIENT_PANEL_ROWS`] clients by request count, keyed by `metadata.user_id`
/// (the `unknown` bucket holds requests with no id). In-memory metering only —
/// not a credential, never gates a request.
fn draw_clients_compact(frame: &mut Frame, area: Rect, view: &DashboardView) {
    let total = view.client_usage.len();
    let header = ["client", "req", "ok/err", "in", "out"];
    let rows = view
        .client_usage
        .iter()
        .take(CLIENT_PANEL_ROWS)
        .map(|c| {
            let ok_err = Line::from(vec![
                Span::styled(format::human_count(c.ok), Style::new().fg(Color::Green)),
                Span::raw("/"),
                Span::styled(
                    format::human_count(c.errors),
                    if c.errors > 0 {
                        Style::new().fg(Color::Red)
                    } else {
                        dim()
                    },
                ),
            ]);
            Row::new(vec![
                Cell::from(c.client.clone()),
                Cell::from(format::human_count(c.requests)),
                Cell::from(ok_err),
                Cell::from(format::human_count(c.tokens_in)),
                Cell::from(format::human_count(c.tokens_out)),
            ])
        })
        .collect::<Vec<_>>();
    let constraints = [
        Constraint::Fill(1),
        Constraint::Length(7),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(9),
    ];
    let shown = total.min(CLIENT_PANEL_ROWS);
    let title = format!(" clients — top {shown} of {total} by requests (metadata.user_id) ");
    let table = Table::new(rows, constraints)
        .header(Row::new(header).style(dim().add_modifier(Modifier::BOLD)))
        .block(Block::new().borders(Borders::TOP).title(title));
    frame.render_widget(table, area);
}

/// Rows the windowed heatmap panel needs: a title/header pair + one row per
/// visible cell, capped at [`HEATMAP_MAX_ROWS`], plus the top border. Returns 0
/// when the panel would not fit (tiny terminal) so the model view keeps the
/// space.
fn heatmap_panel_height(
    view: &DashboardView,
    window: super::activity::StatsWindow,
    total: u16,
) -> u16 {
    let cells = heatmap_cells(view, window).len().min(HEATMAP_MAX_ROWS);
    // border(1) + best-effort line(1) + header(1) + cells (≥1 for the "no
    // activity" / first row), then never starve the model view above it.
    let want = 3 + cells.max(1) as u16;
    want.min(total.saturating_sub(8))
}

/// Usage overlay (`U`, usage-stats): the calendar usage table — hourly /
/// daily / monthly buckets (`g` cycles) × model, with the four token classes
/// and the API-equivalent cost. Rows arrive on the document pre-bucketed,
/// pre-labeled, and pre-priced (the daemon's civil calendar and pricing
/// overrides are the single source of truth — see `UsageStatDoc`); this
/// renderer only filters by the selected granularity, groups consecutive
/// rows by bucket, and draws a bold per-bucket total row above the per-model
/// rows (cost desc). `usage_scroll` skips whole buckets, newest first.
fn draw_usage_overlay(frame: &mut Frame, area: Rect, view: &DashboardView, chrome: &Chrome) {
    frame.render_widget(Clear, area);
    let gran = chrome.usage_gran;
    let rows: Vec<&crate::dashboard::UsageStatDoc> = view
        .usage_stats
        .iter()
        .filter(|r| r.gran == gran.tag())
        .collect();
    if rows.is_empty() {
        // Distinguish "this granularity's trailing window is empty" (idle
        // daemon >72h → hourly drained, but daily/monthly still have rows)
        // from "no usage rows at all" (fresh daemon, or an attach to an
        // older daemon that doesn't serve the field) — review CR: claiming
        // "no history yet" while the monthly tab is full reads as data loss.
        let hint = if view.usage_stats.is_empty() {
            "no usage history yet — send requests through the proxy (older daemons don't serve usage rows)"
        } else {
            "no buckets in this granularity's window — `g` switches granularity"
        };
        let empty = Paragraph::new(Line::from(Span::styled(
            hint,
            Style::new().fg(Color::Yellow),
        )))
        .block(
            Block::new()
                .borders(Borders::TOP)
                .title(format!(" usage — {} ", gran.label())),
        );
        frame.render_widget(empty, area);
        return;
    }

    // Group consecutive rows by bucket — the document orders each granularity
    // newest bucket first, models within a bucket adjacent.
    let mut buckets: Vec<(u64, &str, Vec<&crate::dashboard::UsageStatDoc>)> = Vec::new();
    for r in rows {
        match buckets.last_mut() {
            Some((bucket, _, group)) if *bucket == r.bucket => group.push(r),
            _ => buckets.push((r.bucket, r.label.as_str(), vec![r])),
        }
    }

    // Period totals for the title: every bucket of the selected granularity,
    // all four token classes (same reasoning as `model_total`). Rows without
    // a USABLE cost (unpriced — or an invalid amount from a pathological
    // pricing override, review R2) contribute nothing to the sum and QUALIFY
    // it, so a total can never read as "all traffic priced" while it
    // silently dropped (or was reduced by) a component.
    let total_cost: f64 = buckets
        .iter()
        .flat_map(|(_, _, g)| g.iter())
        .filter_map(|r| usage_cost_valid(r))
        .sum();
    let any_unpriced = buckets
        .iter()
        .flat_map(|(_, _, g)| g.iter())
        .any(|r| usage_cost_valid(r).is_none());
    let total_tokens: u64 = buckets
        .iter()
        .flat_map(|(_, _, g)| g.iter())
        .map(usage_row_tokens)
        .fold(0, u64::saturating_add);
    let scroll = chrome.usage_scroll.min(buckets.len().saturating_sub(1));
    let title = format!(
        " usage — {} · {} buckets · Σ {} tok · Σ {}{} ",
        gran.label(),
        buckets.len(),
        format::human_count(total_tokens),
        format_cost(total_cost),
        if any_unpriced { " (+unpriced)" } else { "" },
    );

    let mut trows: Vec<Row> = Vec::new();
    for (_, label, group) in buckets.iter().skip(scroll) {
        // The table widget truncates below the fold, so building rows past
        // the visible area is pure waste — bound the construction to the
        // screen (review CR: 180 daily buckets × models per frame otherwise).
        if trows.len() > area.height as usize {
            break;
        }
        // ONE accumulator pass per bucket (review CR): six parallel `.sum()`
        // sweeps were six lines nothing forced to stay in step with the
        // columns rendered below them.
        // A row without a USABLE cost (unpriced, or an invalid amount) adds
        // nothing to the bucket total and flips the `+?` qualifier — a
        // negative component must not silently reduce a total that then
        // reads as fully priced (review R2 MUST-FIX).
        let (requests, tokens_in, tokens_out, cache_read, cache_creation, cost, bucket_unpriced) =
            group.iter().fold(
                (0u64, 0u64, 0u64, 0u64, 0u64, 0f64, false),
                |(req, ti, to, cr, cc, cost, unpriced), r| {
                    let valid = usage_cost_valid(r);
                    (
                        req.saturating_add(r.requests),
                        ti.saturating_add(r.tokens_in),
                        to.saturating_add(r.tokens_out),
                        cr.saturating_add(r.cache_read),
                        cc.saturating_add(r.cache_creation),
                        cost + valid.unwrap_or(0.0),
                        unpriced || valid.is_none(),
                    )
                },
            );
        trows.push(
            Row::new(vec![
                Cell::from(*label),
                Cell::from(Span::styled(format!("{} models", group.len()), dim())),
                Cell::from(format::human_count(requests)),
                Cell::from(format::human_count(tokens_in)),
                Cell::from(format::human_count(tokens_out)),
                Cell::from(format::human_count(cache_read)),
                Cell::from(format::human_count(cache_creation)),
                usage_cost_cell(
                    Some(cost),
                    UsageCostTier::Total,
                    if bucket_unpriced { "+?" } else { "" },
                ),
            ])
            .style(Style::new().add_modifier(Modifier::BOLD)),
        );
        let mut models = group.clone();
        models.sort_by(|a, b| {
            b.cost_usd
                .partial_cmp(&a.cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| usage_row_tokens(b).cmp(&usage_row_tokens(a)))
        });
        for m in models {
            trows.push(Row::new(vec![
                Cell::from(""),
                Cell::from(Span::styled(
                    format!("{}/{}", m.group, m.model),
                    group_color(Some(&m.group)),
                )),
                Cell::from(format::human_count(m.requests)),
                Cell::from(format::human_count(m.tokens_in)),
                Cell::from(format::human_count(m.tokens_out)),
                Cell::from(format::human_count(m.cache_read)),
                Cell::from(format::human_count(m.cache_creation)),
                // "no rate known" (or an invalid amount) renders as an
                // explicit dash — a fabricated $0.0000 would read as "free"
                // (review R1 MUST-FIX 3 / R2 MUST-FIX).
                usage_cost_cell(usage_cost_valid(m), UsageCostTier::Detail, ""),
            ]));
        }
    }

    let header = vec![
        "bucket", "model", "req", "input", "output", "cache r", "cache w", "cost",
    ];
    let constraints = vec![
        Constraint::Length(12),
        Constraint::Fill(1),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(USAGE_COST_COL_WIDTH),
    ];
    let table = Table::new(trows, constraints)
        .header(Row::new(header).style(dim().add_modifier(Modifier::BOLD)))
        .block(Block::new().borders(Borders::TOP).title(title));
    frame.render_widget(table, area);
}

/// Total tokens of one usage row: all four classes, same reasoning as
/// [`model_total`] — `tokens_in` alone would hide cache-heavy traffic that
/// the `cost` column still prices.
fn usage_row_tokens(r: &&crate::dashboard::UsageStatDoc) -> u64 {
    r.tokens_in
        .saturating_add(r.tokens_out)
        .saturating_add(r.cache_read)
        .saturating_add(r.cache_creation)
}

/// A usage row's cost when it is USABLE for display arithmetic: priced AND
/// finite AND non-negative (a pathological config `pricing` override can
/// push a negative rate through the doc build). `None` = the row renders
/// the honesty dash AND every aggregate that would have included it carries
/// the `+?` / `(+unpriced)` qualifier — validated per COMPONENT, before
/// aggregation, so an invalid term can't hide inside a plausible-looking
/// sum (review R2 MUST-FIX).
fn usage_cost_valid(r: &crate::dashboard::UsageStatDoc) -> Option<f64> {
    (r.priced && r.cost_usd.is_finite() && r.cost_usd >= 0.0).then_some(r.cost_usd)
}

/// Which display tier a Usage-table cost amount belongs to (ledger polish):
/// bucket totals read at full strength, per-model detail rows a tier darker
/// so the eye lands on the totals first (weight-hierarchy).
#[derive(Clone, Copy, PartialEq, Eq)]
enum UsageCostTier {
    Total,
    Detail,
}

/// Width of the integer part of a Usage cost amount ("999,999" — six years
/// of this daemon's heaviest month fits). Fixing this width is what anchors
/// the DECIMAL POINT to one column for every row (number-tabular): amounts
/// align like a ledger instead of drifting with their magnitude.
const USAGE_COST_INT_WIDTH: usize = 7;
/// `$` + integer part + `.` + up to 4 fraction digits + `+?` marker.
const USAGE_COST_COL_WIDTH: u16 = (1 + USAGE_COST_INT_WIDTH as u16) + 1 + 4 + 2;

/// One Usage-table cost cell (usage-stats ledger polish). `None` = no rate
/// known → an explicit `—` at the ones column, never a fabricated $0.
///
/// Financial-display conventions, adapted to the terminal:
/// - the decimal point sits at a FIXED column (integer part right-aligned in
///   [`USAGE_COST_INT_WIDTH`], with thousands separators) so a column of
///   amounts scans like a ledger;
/// - digits ABOVE the point carry the emphasis, digits BELOW it render a
///   tier dimmer (the eye reads dollars first, cents on demand), and the
///   `$` sign is quieter than both;
/// - sub-dollar amounts keep 4 fraction digits (per-request costs stay
///   legible), dollar-plus amounts keep 2 — the point column never moves.
fn usage_cost_cell(cost: Option<f64>, tier: UsageCostTier, marker: &str) -> Cell<'static> {
    let (int_style, frac_style) = match tier {
        UsageCostTier::Total => (Style::new(), Style::new().fg(Color::DarkGray)),
        UsageCostTier::Detail => (
            Style::new().fg(Color::DarkGray),
            Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        ),
    };
    // The `$` sign sits at the QUIETEST tier — never louder than the
    // fraction (on Detail rows the fraction is already at the floor, so
    // they share it).
    let sign_style = Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM);
    // No rate known — or an INVALID amount (negative / non-finite, reachable
    // through a pathological config `pricing` override) — renders as the
    // explicit dash: invalid data must never dress up as a credible $0
    // (review R1 MUST-FIX, same honesty contract as `priced: false`). The
    // dash lands where the ones digit would be, keeping the ledger column.
    let cost = cost.filter(|c| c.is_finite() && *c >= 0.0);
    let Some(cost) = cost else {
        return Cell::from(Line::from(vec![
            Span::raw(" ".repeat(1 + USAGE_COST_INT_WIDTH - 1)),
            Span::styled("—", frac_style),
        ]));
    };
    let (int_part, frac) = usage_cost_parts(cost);
    Cell::from(Line::from(vec![
        Span::raw(" ".repeat(USAGE_COST_INT_WIDTH.saturating_sub(int_part.len()))),
        Span::styled("$", sign_style),
        Span::styled(int_part, int_style),
        Span::styled(format!("{frac}{marker}"), frac_style),
    ]))
}

/// Split a non-negative cost into its grouped integer part and its fraction
/// string. Integer arithmetic on the rounded sub-units so a carry can't
/// produce an 11-character fraction (0.99999 must render `("1", ".00")`,
/// not `("0", ".10000")`); sub-dollar amounts keep 4 fraction digits,
/// dollar-plus amounts 2. Grouped integers past [`USAGE_COST_INT_WIDTH`]
/// keep rendering, merely unaligned — alignment is guaranteed up to
/// $999,999 per bucket.
fn usage_cost_parts(cost: f64) -> (String, String) {
    let (dollars, frac) = if cost < 0.995 {
        let tenths_of_mill = (cost * 10_000.0).round() as u64;
        (
            tenths_of_mill / 10_000,
            format!(".{:04}", tenths_of_mill % 10_000),
        )
    } else {
        let cents = (cost * 100.0).round() as u64;
        (cents / 100, format!(".{:02}", cents % 100))
    };
    (group_thousands(dollars), frac)
}

/// `1234567` → `"1,234,567"` — plain thousands grouping for ledger integer
/// parts ([`format::human_count`] compresses to `1.2M`, which a cost column
/// must not do).
fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Logs overlay (`l`): a full-screen log tail (was the `l` size-cycle panel).
fn draw_logs_overlay(frame: &mut Frame, area: Rect, view: &DashboardView) {
    frame.render_widget(Clear, area);
    draw_logs(frame, area, view);
}

/// Sessions overlay (`s`, issue #34): the persisted raw-io log folded by
/// `metadata.user_id` into a confidence-labeled session timeline, above a
/// per-session detail pane for the cursored row. Renders from the snapshot held
/// on `Chrome` (taken when the overlay opened) — metadata only, no prompt
/// content. On a side-by-side width the detail sits beside the list; otherwise
/// the list takes the whole rect.
fn draw_sessions_overlay(
    frame: &mut Frame,
    area: Rect,
    ctx: &FrameCtx,
    chrome: &Chrome,
    hits: &mut Option<MainChrome>,
) {
    frame.render_widget(Clear, area);
    // The load runs on the blocking pool and streams progressive partials. Show
    // the full-screen spinner ONLY while loading AND no partial has arrived yet
    // (sessions still empty); once the table has content it renders below with a
    // `loading… N%` title (see `draw_sessions_table`) so the user watches it fill
    // in rather than staring at a spinner. On reopen, a prior open's `sessions`
    // stay visible (with the loading title) until the first fresh partial lands.
    if chrome.sessions_loading && chrome.sessions.is_empty() {
        let glyph = anim::braille_spin(chrome.frame);
        let loading = Paragraph::new(Line::from(vec![
            Span::styled(format!("{glyph} "), Style::new().fg(Color::Cyan)),
            Span::styled("loading sessions…", dim()),
        ]))
        .block(Block::new().borders(Borders::TOP).title(" sessions "));
        frame.render_widget(loading, area);
        return;
    }
    if chrome.sessions.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "no sessions yet — enable raw-io capture and send requests through the proxy",
            Style::new().fg(Color::Yellow),
        )))
        .block(Block::new().borders(Borders::TOP).title(" sessions "));
        frame.render_widget(empty, area);
        return;
    }
    let table_chrome = if area.width >= SIDE_BY_SIDE_AT {
        let [list_area, detail_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(46)]).areas(area);
        let tc = draw_sessions_table(frame, list_area, ctx, chrome);
        draw_session_detail(frame, detail_area, ctx, chrome);
        tc
    } else {
        draw_sessions_table(frame, area, ctx, chrome)
    };
    if let Some(hits) = hits.as_mut() {
        hits.sessions_table = table_chrome;
    }
}

/// The session timeline table. Columns: confidence label, user_id, request
/// count, tokens in/out, distinct models, distinct accounts + rotation count,
/// and the wall-clock span. The cursored row is highlighted; the title shows the
/// cursor position so it is obvious more rows exist off-screen.
fn draw_sessions_table(
    frame: &mut Frame,
    area: Rect,
    ctx: &FrameCtx,
    chrome: &Chrome,
) -> Option<SessionsChrome> {
    let total = chrome.sessions.len();
    let cursor = chrome.session_cursor.min(total.saturating_sub(1));
    let capacity = (area.height.saturating_sub(2) as usize).max(1); // border + header
    let start = if cursor >= capacity {
        cursor + 1 - capacity
    } else {
        0
    };
    let end = (start + capacity).min(total);

    let rows = chrome.sessions[start..end]
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let idx = start + i;
            let conf_color = match s.confidence {
                crate::session::Confidence::High => Color::Green,
                crate::session::Confidence::Low => Color::DarkGray,
            };
            // Honest per-session output rate: Σtimed output / Σrecorded
            // request duration (never tokens over the wall-clock span —
            // idle time between requests is not generation time). `—` when
            // no record carried a duration (pre-field raw-io history); dim
            // under 5 timed samples.
            let tps = (s.duration_ms_sum > 0)
                .then(|| s.tokens_out_timed as f64 * 1000.0 / s.duration_ms_sum as f64);
            let tps_style = if s.timed_requests < 5 {
                dim()
            } else {
                Style::new()
            };
            let cells = vec![
                Cell::from(Span::styled(
                    s.confidence.label(),
                    Style::new().fg(conf_color),
                )),
                Cell::from(session_id_label(s)),
                Cell::from(format::human_count(s.requests)),
                Cell::from(format::human_count(s.tokens_in)),
                Cell::from(format::human_count(s.tokens_out)),
                Cell::from(Span::styled(tps_cell(tps), tps_style)),
                Cell::from(format::human_count(s.models.len() as u64)),
                Cell::from(session_accounts_label(s)),
                Cell::from(Span::styled(session_span_label(s), dim())),
            ];
            let row = Row::new(cells);
            if idx == cursor {
                row.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                row
            }
        });

    let header = vec![
        "conf", "session", "req", "in", "out", "t/s", "mdl", "acct", "span",
    ];
    let constraints = vec![
        Constraint::Length(5),
        Constraint::Fill(1),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(5),
        Constraint::Length(10),
        Constraint::Length(9),
    ];
    // While a progressive load is still streaming, append the read progress so
    // the growing table reads as "filling in", not stalled.
    let sort = chrome.session_sort.label();
    let title = if chrome.sessions_loading {
        format!(
            " sessions — {} of {total} — sort {sort} (o) — loading… {}% ",
            cursor + 1,
            chrome.sessions_pct
        )
    } else {
        format!(" sessions — {} of {total} — sort {sort} (o) ", cursor + 1)
    };
    let table = Table::new(rows, constraints)
        .header(Row::new(header).style(dim().add_modifier(Modifier::BOLD)))
        .block(Block::new().borders(Borders::TOP).title(title));
    frame.render_widget(table, area);
    // Data rows start under the top border + header row.
    let rows_rect = Rect {
        x: area.x,
        y: area.y.saturating_add(2),
        width: area.width,
        height: area.height.saturating_sub(2),
    };
    let _ = ctx;
    (rows_rect.height > 0).then_some(SessionsChrome {
        rows: rows_rect,
        start,
    })
}

/// Drill-down pane for the cursored session: the grouping confidence, the full
/// user_id, the model list, the account list + rotation count, the token split,
/// and the absolute time span. Metadata only.
fn draw_session_detail(frame: &mut Frame, area: Rect, ctx: &FrameCtx, chrome: &Chrome) {
    let cursor = chrome
        .session_cursor
        .min(chrome.sessions.len().saturating_sub(1));
    let Some(s) = chrome.sessions.get(cursor) else {
        return;
    };
    let models = if s.models.is_empty() {
        "—".to_string()
    } else {
        s.models.join(", ")
    };
    let accounts = if s.accounts.is_empty() {
        "—".to_string()
    } else {
        // Session accounts are emails from the raw-io log — same
        // email-anonymous masking as every other account surface.
        s.accounts
            .iter()
            .map(|a| masked_name(a, ctx.mask))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let first = format::absolute_label(ms_to_systemtime(s.first_ms), ctx.now, ctx.tz_offset);
    let last = format::absolute_label(ms_to_systemtime(s.last_ms), ctx.now, ctx.tz_offset);
    let lines = vec![
        Line::from(vec![
            Span::styled("confidence  ", dim()),
            Span::styled(
                s.confidence.label(),
                match s.confidence {
                    crate::session::Confidence::High => Style::new().fg(Color::Green),
                    crate::session::Confidence::Low => dim(),
                },
            ),
        ]),
        Line::from(vec![
            Span::styled("user_id     ", dim()),
            Span::raw(s.user_id.clone().unwrap_or_else(|| "(ungrouped)".into())),
        ]),
        Line::from(vec![
            Span::styled("requests    ", dim()),
            Span::raw(format::human_count(s.requests)),
        ]),
        Line::from(vec![
            Span::styled("tokens      ", dim()),
            Span::raw(format!(
                "{} in / {} out",
                format::human_count(s.tokens_in),
                format::human_count(s.tokens_out)
            )),
        ]),
        Line::from(vec![Span::styled("models      ", dim()), Span::raw(models)]),
        Line::from(vec![
            Span::styled("accounts    ", dim()),
            Span::raw(accounts),
        ]),
        Line::from(vec![
            Span::styled("rotations   ", dim()),
            Span::raw(s.account_rotations.to_string()),
        ]),
        Line::from(vec![Span::styled("first       ", dim()), Span::raw(first)]),
        Line::from(vec![Span::styled("last        ", dim()), Span::raw(last)]),
        Line::from(vec![
            Span::styled("span        ", dim()),
            Span::raw(session_span_label(s)),
        ]),
    ];
    let detail = Paragraph::new(lines).block(Block::new().borders(Borders::TOP).title(" session "));
    frame.render_widget(detail, area);
}

/// Display label for a session's grouping key: the user_id, or `(ungrouped)` for
/// the catch-all bucket of records with no `metadata.user_id`.
fn session_id_label(s: &crate::session::Session) -> String {
    s.user_id.clone().unwrap_or_else(|| "(ungrouped)".into())
}

/// `acct ×N` where N is the distinct-account count; rotations are shown in the
/// detail pane. A single account drops the multiplier.
fn session_accounts_label(s: &crate::session::Session) -> String {
    let n = s.accounts.len();
    if n <= 1 {
        n.to_string()
    } else {
        format!("{n} ×{}", s.account_rotations)
    }
}

/// Wall-clock span of a session as a coarse duration ("3m 04s", "2h 11m").
fn session_span_label(s: &crate::session::Session) -> String {
    format::countdown(Duration::from_millis(s.span_ms()))
}

/// Millis-since-epoch → `SystemTime` for the absolute-time labels. Pure; a value
/// that would overflow saturates to the epoch (never panics).
fn ms_to_systemtime(ms: u64) -> SystemTime {
    UNIX_EPOCH
        .checked_add(Duration::from_millis(ms))
        .unwrap_or(UNIX_EPOCH)
}

// ---------------------------------------------------------------------------
// Perf overlay (perf telemetry v1): observed performance per provider/model.
// ---------------------------------------------------------------------------

/// One aggregated perf series over the selected window (perf telemetry v1):
/// (group, model, fast) + the window's raw sums. Throughput is derived here
/// as `Σoutput/Σms` — never an average of per-request rates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PerfAgg {
    group: String,
    model: String,
    fast: Option<bool>,
    requests: u64,
    errors: u64,
    tps_n: u64,
    output_tokens: u64,
    e2e_ms: u64,
    measured_n: u64,
    measured_output: u64,
    post_ttft_ms: u64,
    ttfb_n: u64,
    ttfb_ms_sum: u64,
}

impl PerfAgg {
    /// e2e observed throughput (`Σoutput/Σduration`) — `None` when the
    /// window has no throughput sample (never fabricate a rate).
    fn e2e_tps(&self) -> Option<f64> {
        (self.tps_n > 0 && self.e2e_ms > 0)
            .then(|| self.output_tokens as f64 * 1000.0 / self.e2e_ms as f64)
    }

    /// Estimated post-delta throughput — measured samples only (requests
    /// whose STREAM recorded a positive first-delta→end span; the request
    /// duration never enters this denominator), and the summed span must
    /// clear the stability floor (a bucket with <50ms of total span yields a
    /// numerically meaningless ratio; raw sums are kept, so it fills in as
    /// data accrues).
    fn est_tps(&self) -> Option<f64> {
        (self.measured_n > 0 && self.post_ttft_ms >= 50)
            .then(|| self.measured_output as f64 * 1000.0 / self.post_ttft_ms as f64)
    }

    fn err_pct(&self) -> f64 {
        if self.requests == 0 {
            0.0
        } else {
            self.errors as f64 * 100.0 / self.requests as f64
        }
    }
}

/// `45t/s` / `7.2t/s` — or `—` for `None` (no sample / below the stability
/// floor). The honest empty cell, never `0`.
fn tps_cell(tps: Option<f64>) -> String {
    match tps {
        Some(t) if t >= 10.0 => format!("{t:.0}t/s"),
        Some(t) => format!("{t:.1}t/s"),
        None => "—".to_string(),
    }
}

/// Series display label: `codex gpt-5.5⚡` / `claude opus?` — `⚡` = fast on,
/// `?` = recorded before the fast field existed ("unknown", its own series).
fn perf_series_label(group: &str, model: &str, fast: Option<bool>) -> String {
    let marker = match fast {
        Some(true) => "⚡",
        Some(false) => "",
        None => "?",
    };
    format!("{group} {model}{marker}")
}

/// The trailing-window day range `[start, today]` for a span of `days`.
fn perf_window(now: SystemTime, days: u64) -> (u64, u64) {
    let today = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0);
    (today.saturating_sub(days.saturating_sub(1)), today)
}

/// Aggregate the window's perf rows into per-(group, model, fast) series,
/// sorted by output tokens desc (ties: key order, deterministic).
fn perf_series(view: &DashboardView, days: u64, now: SystemTime) -> Vec<PerfAgg> {
    let (start, today) = perf_window(now, days);
    let mut agg: std::collections::BTreeMap<(String, String, Option<bool>), PerfAgg> =
        Default::default();
    for r in view
        .daily_perf
        .iter()
        .filter(|r| r.day >= start && r.day <= today)
    {
        let a = agg
            .entry((r.group.clone(), r.model.clone(), r.fast))
            .or_insert_with(|| PerfAgg {
                group: r.group.clone(),
                model: r.model.clone(),
                fast: r.fast,
                ..Default::default()
            });
        a.requests += r.requests;
        a.errors += r.errors;
        a.tps_n += r.tps_n;
        a.output_tokens += r.output_tokens;
        a.e2e_ms += r.e2e_ms;
        a.measured_n += r.measured_n;
        a.measured_output += r.measured_output;
        a.post_ttft_ms += r.post_ttft_ms;
        a.ttfb_n += r.ttfb_n;
        a.ttfb_ms_sum += r.ttfb_ms_sum;
    }
    let mut rows: Vec<PerfAgg> = agg.into_values().collect();
    rows.sort_by_key(|a| std::cmp::Reverse(a.output_tokens));
    rows
}

/// Number of series rows the Perf overlay shows for the span (or the
/// drilled-down day) — the key handler clamps its cursor with this.
pub(crate) fn perf_series_count(view: &DashboardView, days: u64, day_off: Option<u64>) -> usize {
    let now = SystemTime::now();
    match day_off {
        None => perf_series(view, days, now).len(),
        Some(off) => perf_series_for_day(view, off, now).len(),
    }
}

/// The per-(model, fast) series of ONE day, `off` days back from today —
/// the contract C5 daily drill-down (`Σoutput/Σms` within that day).
fn perf_series_for_day(view: &DashboardView, off: u64, now: SystemTime) -> Vec<PerfAgg> {
    let (_, today) = perf_window(now, 1);
    let day = today.saturating_sub(off);
    let mut rows: Vec<PerfAgg> = Vec::new();
    let mut agg: std::collections::BTreeMap<(String, String, Option<bool>), PerfAgg> =
        Default::default();
    for r in view.daily_perf.iter().filter(|r| r.day == day) {
        let a = agg
            .entry((r.group.clone(), r.model.clone(), r.fast))
            .or_insert_with(|| PerfAgg {
                group: r.group.clone(),
                model: r.model.clone(),
                fast: r.fast,
                ..Default::default()
            });
        a.requests += r.requests;
        a.errors += r.errors;
        a.tps_n += r.tps_n;
        a.output_tokens += r.output_tokens;
        a.e2e_ms += r.e2e_ms;
        a.measured_n += r.measured_n;
        a.measured_output += r.measured_output;
        a.post_ttft_ms += r.post_ttft_ms;
        a.ttfb_n += r.ttfb_n;
        a.ttfb_ms_sum += r.ttfb_ms_sum;
    }
    rows.extend(agg.into_values());
    rows.sort_by_key(|a| std::cmp::Reverse(a.output_tokens));
    rows
}

/// Colors for the perf chart series (rank order, distinct on dark terminals).
const PERF_CHART_COLORS: [Color; 4] = [Color::Magenta, Color::Cyan, Color::Yellow, Color::Green];

/// Perf overlay (`p`, perf telemetry v1 — "observed performance"): a daily
/// e2e tokens/sec chart for the top series, a date × provider health matrix,
/// and the per-(model, fast) series table. Passive telemetry from real
/// requests — deliberately NOT called a healthcheck; quiet days render `—`,
/// never a fabricated `0 t/s`.
fn draw_perf_overlay(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    chrome: &Chrome,
) {
    frame.render_widget(Clear, area);
    let days = chrome.perf_days.clamp(2, 366);
    if view.daily_perf.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "no perf data yet — collecting starts with this daemon version (metric v1); \
             send requests through the proxy",
            Style::new().fg(Color::Yellow),
        )))
        .block(Block::new().borders(Borders::TOP).title(" perf "));
        frame.render_widget(empty, area);
        return;
    }
    let series = match chrome.perf_day_off {
        None => perf_series(view, days, ctx.now),
        Some(off) => perf_series_for_day(view, off, ctx.now),
    };
    // "Collecting since" = the first day that actually observed v1 TIMING
    // (ttfb or a measured span) — legacy replayed rows contribute e2e sums
    // but must not backdate the collection start (review MUST-FIX).
    let since = view
        .daily_perf
        .iter()
        .filter(|r| r.ttfb_n > 0 || r.measured_n > 0)
        .map(|r| r.day)
        .min()
        .map(day_label);
    let title = match since {
        Some(since) => {
            format!(" perf — observed, last {days}d · timing since {since} · v1 · d cycles ")
        }
        None => format!(" perf — observed, last {days}d · no timing data yet · v1 · d cycles "),
    };

    let chart_h = if area.height >= 24 { 13 } else { 0 };
    let table_h = (series.len().min(8) as u16).saturating_add(3);
    let [chart_area, health_area, table_area] = Layout::vertical([
        Constraint::Length(chart_h),
        Constraint::Min(4),
        Constraint::Length(table_h),
    ])
    .areas(area);
    if chart_h > 0 {
        draw_perf_chart(frame, chart_area, view, ctx, days, &title);
        draw_perf_health(frame, health_area, view, ctx, days, None);
    } else {
        draw_perf_health(frame, health_area, view, ctx, days, Some(&title));
    }
    let scope = match chrome.perf_day_off {
        None => format!("last {days}d"),
        Some(off) => {
            let (_, today) = perf_window(ctx.now, 1);
            format!(
                "day {} · h/l walks days",
                day_label(today.saturating_sub(off))
            )
        }
    };
    draw_perf_table(frame, table_area, &series, chrome.perf_cursor, &scope);
}

/// `MM-DD`-style label for an epoch day (UTC), reusing the month/day half of
/// [`format::month_day_hm`].
fn day_label(d: u64) -> String {
    let at = UNIX_EPOCH + Duration::from_secs(d * 86_400);
    let full = format::month_day_hm(at, 0);
    full.split(' ').next().unwrap_or("").to_string()
}

/// Daily e2e tokens/sec line chart: one braille line per top series over the
/// span. A day without throughput samples plots 0 (trend view; the tables
/// stay the honest `—` surface).
fn draw_perf_chart(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    days: u64,
    title: &str,
) {
    let (start, today) = perf_window(ctx.now, days);
    let in_span = |r: &&crate::dashboard::DailyPerfDoc| r.day >= start && r.day <= today;
    // Rank series by window output; plot the top 4.
    let ranked = {
        let mut totals: std::collections::BTreeMap<(&str, &str, Option<bool>), u64> =
            Default::default();
        for r in view.daily_perf.iter().filter(&in_span) {
            *totals
                .entry((r.group.as_str(), r.model.as_str(), r.fast))
                .or_default() += r.output_tokens;
        }
        let mut ranked: Vec<_> = totals.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        ranked.truncate(PERF_CHART_COLORS.len());
        ranked
    };
    if ranked.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled("no requests in this span", dim())))
            .block(Block::new().borders(Borders::TOP).title(title.to_string()));
        frame.render_widget(empty, area);
        return;
    }
    let span_days = days as usize;
    // Per-day per-series raw sums → tps points. A day WITHOUT throughput
    // samples is a GAP, never a fabricated 0 (contract C5): each series is
    // split into contiguous sampled segments and each segment becomes its
    // own same-colored dataset, so the line breaks over quiet days.
    let mut sums: Vec<Vec<Option<(u64, u64, u64)>>> = vec![vec![None; span_days]; ranked.len()];
    for r in view.daily_perf.iter().filter(&in_span) {
        if let Some(idx) = ranked
            .iter()
            .position(|((g, m, f), _)| *g == r.group && *m == r.model && *f == r.fast)
        {
            let x = (r.day - start) as usize;
            if r.tps_n > 0 {
                let slot = sums[idx][x].get_or_insert((0, 0, 0));
                slot.0 += r.output_tokens;
                slot.1 += r.e2e_ms;
                slot.2 += r.tps_n;
            }
        }
    }
    // Segments split on BOTH gaps and confidence flips: low-sample (n<5)
    // stretches render dim so a thin day can't read as a confident trend
    // point; quiet days stay gaps (never a fabricated 0).
    let mut y_max = 1f64;
    // (series rank, low-confidence?, points)
    type Segment = (usize, bool, Vec<(f64, f64)>);
    let mut segments: Vec<Segment> = Vec::new();
    for (idx, day_sums) in sums.iter().enumerate() {
        let mut current: Vec<(f64, f64)> = Vec::new();
        let mut current_low = false;
        for (x, slot) in day_sums.iter().enumerate() {
            match slot {
                Some((out, ms, n)) if *ms > 0 => {
                    let y = *out as f64 * 1000.0 / *ms as f64;
                    y_max = y_max.max(y);
                    let low = *n < 5;
                    if low != current_low && !current.is_empty() {
                        let last = *current.last().expect("non-empty");
                        segments.push((idx, current_low, std::mem::take(&mut current)));
                        // Connect across the flip only INTO the dim segment
                        // — a low-confidence point must never be repainted
                        // at full brightness (review MUST-FIX: dim = n<5).
                        if low {
                            current.push(last);
                        }
                    }
                    current_low = low;
                    current.push((x as f64, y));
                }
                _ => {
                    if !current.is_empty() {
                        segments.push((idx, current_low, std::mem::take(&mut current)));
                    }
                }
            }
        }
        if !current.is_empty() {
            segments.push((idx, current_low, current));
        }
    }
    let datasets: Vec<Dataset> = segments
        .iter()
        .map(|(idx, low, data)| {
            let style = if *low {
                dim()
            } else {
                Style::new().fg(PERF_CHART_COLORS[*idx])
            };
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(style)
                .data(data)
        })
        .collect();
    let x_axis = Axis::default()
        .bounds([0.0, (span_days.saturating_sub(1)) as f64])
        .labels(vec![
            Span::styled(day_label(start), dim()),
            Span::styled(day_label(start + days / 2), dim()),
            Span::styled(day_label(today), dim()),
        ])
        .style(dim());
    let y_axis = Axis::default()
        .bounds([0.0, y_max])
        .labels(vec![
            Span::styled("0", dim()),
            Span::styled(format!("{:.0}", y_max / 2.0), dim()),
            Span::styled(format!("{y_max:.0} t/s"), dim()),
        ])
        .style(dim());
    let [plot_area, legend_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let chart = Chart::new(datasets)
        .block(Block::new().borders(Borders::TOP).title(title.to_string()))
        .x_axis(x_axis)
        .y_axis(y_axis);
    frame.render_widget(chart, plot_area);
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    for (i, ((group, model, fast), _)) in ranked.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(Span::styled("● ", Style::new().fg(PERF_CHART_COLORS[i])));
        spans.push(Span::raw(perf_series_label(group, model, *fast)));
    }
    spans.push(Span::styled(
        "   (e2e t/s · dim = n<5 — estimated post-delta rates in the table below)",
        dim(),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), legend_area);
}

/// Date × provider health matrix: per day and backend group — sample count,
/// error %, avg TTFB, e2e t/s. `—` = no data (never a fabricated 0); rows
/// with n < 5 render dim (low confidence, still shown — low traffic is
/// itself a signal).
fn draw_perf_health(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    days: u64,
    title: Option<&str>,
) {
    let (start, today) = perf_window(ctx.now, days);
    let in_span = |r: &&crate::dashboard::DailyPerfDoc| r.day >= start && r.day <= today;
    // Providers present in the window, stable order.
    let mut groups: Vec<&str> = Vec::new();
    for r in view.daily_perf.iter().filter(&in_span) {
        if !groups.contains(&r.group.as_str()) {
            groups.push(r.group.as_str());
        }
    }
    groups.sort_unstable();
    // Fold per (day, group).
    let mut cells: std::collections::BTreeMap<(u64, &str), PerfAgg> = Default::default();
    for r in view.daily_perf.iter().filter(&in_span) {
        let a = cells.entry((r.day, r.group.as_str())).or_default();
        a.requests += r.requests;
        a.errors += r.errors;
        a.tps_n += r.tps_n;
        a.output_tokens += r.output_tokens;
        a.e2e_ms += r.e2e_ms;
        a.measured_n += r.measured_n;
        a.measured_output += r.measured_output;
        a.post_ttft_ms += r.post_ttft_ms;
        a.ttfb_n += r.ttfb_n;
        a.ttfb_ms_sum += r.ttfb_ms_sum;
    }
    let capacity = (area.height.saturating_sub(2) as usize).max(1);
    let mut rows: Vec<Row> = Vec::new();
    // Newest day first, EVERY day in the span shown — a day with no traffic
    // renders as an explicit `—` row, never silently dropped (contract C5).
    for day in (start..=today).rev().take(capacity) {
        let mut row_cells: Vec<Cell> = vec![Cell::from(Span::styled(day_label(day), dim()))];
        for group in &groups {
            match cells.get(&(day, *group)) {
                Some(a) => {
                    let ttfb = match a.ttfb_ms_sum.checked_div(a.ttfb_n) {
                        Some(avg) => format::elapsed_secs(Duration::from_millis(avg)),
                        None => "—".into(),
                    };
                    let err = a.err_pct();
                    let err_style = if err >= 10.0 {
                        Style::new().fg(Color::Red)
                    } else if err > 0.0 {
                        Style::new().fg(Color::Yellow)
                    } else {
                        Style::new().fg(Color::Green)
                    };
                    // Confidence is judged PER CELL on the stat's own sample
                    // count (e2e → tps_n, est → measured_n) — never on the
                    // day total across providers (review MUST-FIX 3/5).
                    let e2e_style = if a.tps_n < 5 { dim() } else { Style::new() };
                    let est_style = if a.measured_n < 5 {
                        dim()
                    } else {
                        Style::new()
                    };
                    row_cells.push(Cell::from(Line::from(vec![
                        Span::raw(format!("{:>4} ", format::human_count(a.requests))),
                        Span::styled(format!("{err:>4.0}% "), err_style),
                        Span::raw(format!("{ttfb:>6} ")),
                        Span::styled(format!("{:>8} ", tps_cell(a.e2e_tps())), e2e_style),
                        Span::styled(format!("{:>8}", tps_cell(a.est_tps())), est_style),
                    ])));
                }
                None => row_cells.push(Cell::from(Span::styled("—", dim()))),
            }
        }
        rows.push(Row::new(row_cells));
    }
    let mut header: Vec<Cell> = vec![Cell::from("date")];
    for group in &groups {
        header.push(Cell::from(format!("{group}: n err% ttfb e2e est")));
    }
    let mut constraints = vec![Constraint::Length(7)];
    constraints.extend(groups.iter().map(|_| Constraint::Fill(1)));
    let block = Block::new().borders(Borders::TOP).title(match title {
        Some(t) => t.to_string(),
        None => " provider health (observed) ".to_string(),
    });
    let table = Table::new(rows, constraints)
        .header(Row::new(header).style(dim().add_modifier(Modifier::BOLD)))
        .block(block);
    frame.render_widget(table, area);
}

/// Per-(group, model, fast) series table over the span: samples, error %,
/// output tokens, e2e + estimated post-delta t/s, avg TTFB, and measured
/// coverage (`measured/tps` samples). The estimated column is derived from
/// measured samples only — approximate/legacy samples never mix in.
fn draw_perf_table(frame: &mut Frame, area: Rect, series: &[PerfAgg], cursor: usize, scope: &str) {
    let total = series.len();
    let cursor = cursor.min(total.saturating_sub(1));
    let capacity = (area.height.saturating_sub(2) as usize).max(1);
    let start = if cursor >= capacity {
        cursor + 1 - capacity
    } else {
        0
    };
    let end = (start + capacity).min(total);
    let rows = series[start..end].iter().enumerate().map(|(i, a)| {
        let idx = start + i;
        let ttfb = match a.ttfb_ms_sum.checked_div(a.ttfb_n) {
            Some(avg) => format::elapsed_secs(Duration::from_millis(avg)),
            None => "—".into(),
        };
        let coverage = format!("{}/{}", a.measured_n, a.tps_n);
        // Per-cell confidence: e2e dims under its own tps_n, est under its
        // measured_n — a busy series must not lend confidence to a sparse
        // stat (review MUST-FIX 3/5). Rows are never hidden.
        let e2e_style = if a.tps_n < 5 { dim() } else { Style::new() };
        let est_style = if a.measured_n < 5 {
            dim()
        } else {
            Style::new()
        };
        let row = Row::new(vec![
            Cell::from(perf_series_label(&a.group, &a.model, a.fast)),
            Cell::from(format!("{:>5}", format::human_count(a.requests))),
            Cell::from(format!("{:>5.1}%", a.err_pct())),
            Cell::from(format!("{:>7}", format::human_count(a.output_tokens))),
            Cell::from(Span::styled(
                format!("{:>8}", tps_cell(a.e2e_tps())),
                e2e_style,
            )),
            Cell::from(Span::styled(
                format!("{:>8}", tps_cell(a.est_tps())),
                est_style,
            )),
            Cell::from(format!("{ttfb:>6}")),
            Cell::from(format!("{coverage:>7}")),
        ]);
        if idx == cursor {
            row.style(Style::new().add_modifier(Modifier::REVERSED))
        } else {
            row
        }
    });
    let header = vec![
        "series", "req", "err%", "out", "e2e t/s", "est t/s", "ttfb", "meas",
    ];
    let constraints = vec![
        Constraint::Fill(1),
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Length(8),
    ];
    let table = Table::new(rows, constraints)
        .header(Row::new(header).style(dim().add_modifier(Modifier::BOLD)))
        .block(Block::new().borders(Borders::TOP).title(format!(
            " series ({scope}) — {} of {total} · est = post-first-delta (measured only) ",
            cursor.saturating_add(1).min(total.max(1))
        )));
    frame.render_widget(table, area);
}

/// Misc overlay (`?`, UI-3 U6 "기타"): the everything-else surface —
/// keybindings and build/daemon facts. Read-only.
fn draw_misc_overlay(frame: &mut Frame, area: Rect, view: &DashboardView) {
    frame.render_widget(Clear, area);
    let key = |k: &'static str, what: &'static str| {
        Line::from(vec![
            Span::styled(format!("   {k:<10}"), Style::new().fg(Color::Cyan)),
            Span::raw(what),
        ])
    };
    let bytes = |b: Option<u64>| match b {
        Some(b) => format!("{}B", format::human_count(b)),
        None => "—".into(),
    };
    let f = &view.config_facts;
    let lines = vec![
        Line::from(Span::styled(" keys", dim().add_modifier(Modifier::BOLD))),
        key(
            "a g U p l s",
            "accounts / stats / usage / perf / logs / sessions",
        ),
        key("? c", "this surface / config editor"),
        key(
            "click",
            "tabs switch · activity row expands · config value edits · session selects",
        ),
        key("wheel", "activity history · overlay cursors"),
        key("f m e", "codex fast / model / effort"),
        key("u t S R", "gauge fill / reset display / scheduler / reload"),
        key("o d", "sessions sort · perf/stats span"),
        key("h l", "perf day drill-down (←/→)"),
        key("j k ⏎ y/n", "config editor: cursor · activate · confirm"),
        key("q Esc", "quit / back to dashboard"),
        Line::default(),
        Line::from(Span::styled(" build", dim().add_modifier(Modifier::BOLD))),
        Line::from(Span::raw(format!("   llmux {}", view.display_version()))),
        Line::from(Span::raw(format!(
            "   daemon pid {} · port {} · up {}",
            view.pid,
            view.port,
            format::countdown(view.uptime)
        ))),
        Line::default(),
        Line::from(Span::styled(" daemon", dim().add_modifier(Modifier::BOLD))),
        Line::from(Span::raw(format!(
            "   config {}",
            view.config_path.clone().unwrap_or_else(|| "—".into())
        ))),
        Line::from(Span::raw(format!(
            "   accounts {} · raw-io {} ({}) · activity log {}",
            view.snapshot.accounts.len(),
            if f.raw_io_enabled { "on" } else { "off" },
            bytes(f.raw_io_bytes),
            bytes(f.activity_log_bytes),
        ))),
    ];
    let block = Block::new().borders(Borders::TOP).title(" misc ");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// One editable (or honestly-labeled read-only) config row's action
/// (config-editor v1, trinity contract C6). `Copy` so it rides `Mode` and
/// the hit list without allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ConfigAction {
    SchedMode,
    Ceiling5h,
    Ceiling7d,
    CeilingFbl,
    UsageMaxAge,
    CodexModel,
    CodexEffort,
    CodexFast,
    GrokEffort,
    EmailMask,
    QuotaFill,
    ResetDisplay,
    TuiEffects,
    FableWeekly,
    GradientSpeed,
    RoutingEnabled,
    RoutingDefaultGroup,
    RoutingOnEmptyGroup,
    RawIoEnabled,
    RawIoRetention,
    RawIoMaxBody,
    Upstream,
    CodexUpstream,
    ProxyPort,
    ProxyMaxBody,
}

impl ConfigAction {
    /// Whether activating this row needs the y/n confirm step (blast-radius
    /// rule, not input-type rule): live traffic routing / scheduler policy /
    /// capture gate / upstream endpoints.
    pub(crate) fn needs_confirm(self) -> bool {
        matches!(
            self,
            ConfigAction::SchedMode
                | ConfigAction::RoutingEnabled
                | ConfigAction::RawIoEnabled
                | ConfigAction::Upstream
                | ConfigAction::CodexUpstream
        )
    }

    /// Input hint for the edit prompt (`None` = toggle/cycle, no text entry).
    pub(crate) fn input_hint(self) -> Option<&'static str> {
        match self {
            ConfigAction::Ceiling5h | ConfigAction::Ceiling7d | ConfigAction::CeilingFbl => {
                Some("percent 0-100")
            }
            ConfigAction::UsageMaxAge => Some("seconds 5-3600"),
            ConfigAction::GradientSpeed => Some("0.01-10.0"),
            ConfigAction::RoutingDefaultGroup => Some("claude | codex | grok"),
            ConfigAction::RoutingOnEmptyGroup => Some("error | fallback"),
            ConfigAction::RawIoRetention => Some("days 0-3650 (0 = keep forever)"),
            ConfigAction::RawIoMaxBody | ConfigAction::ProxyMaxBody => Some("bytes"),
            ConfigAction::Upstream | ConfigAction::CodexUpstream => Some("https://…"),
            ConfigAction::ProxyPort => Some("port 1-65535"),
            _ => None,
        }
    }
}

/// One clickable config value cell (config-editor v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfigHit {
    pub area: Rect,
    pub row: usize,
    pub action: ConfigAction,
}

/// How a config row applies (trinity contract C6 3-state honesty).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CfgState {
    /// Applies immediately (a live holder or an existing live path).
    Live,
    /// Persisted now, effective on the next daemon start.
    Restart,
    /// This TUI session only (not persisted).
    Session,
    /// Not editable here — the note says where/why.
    ReadOnly,
}

/// One rendered config row: `None` action = read-only (note explains).
struct CfgRow {
    section: &'static str,
    label: &'static str,
    value: String,
    state: CfgState,
    note: &'static str,
    action: Option<ConfigAction>,
}

/// Build the FULL config inventory (trinity contract C6: the acceptance
/// denominator is the whole `config/schema.rs`, not the rows we felt like
/// showing) — every top-level field appears here as editable, session,
/// restart-required, or read-only-with-reason.
fn config_rows(view: &DashboardView, chrome: &Chrome) -> Vec<CfgRow> {
    let p = &view.select_params;
    let f = &view.config_facts;
    let quota = chrome.quota_display_override.unwrap_or(view.quota_display);
    let pct = |v: f64| format!("{:.0}%", v * 100.0);
    let onoff = |v: bool| if v { "on" } else { "off" }.to_string();
    vec![
        CfgRow {
            section: "scheduler",
            label: "mode",
            value: p.mode.label().to_string(),
            state: CfgState::Live,
            note: "confirm",
            action: Some(ConfigAction::SchedMode),
        },
        CfgRow {
            section: "scheduler",
            label: "5h ceiling",
            value: pct(p.five_hour_max),
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::Ceiling5h),
        },
        CfgRow {
            section: "scheduler",
            label: "7d ceiling",
            value: pct(p.seven_day_max),
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::Ceiling7d),
        },
        CfgRow {
            section: "scheduler",
            label: "fbl ceiling",
            value: pct(p.fable_weekly_max),
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::CeilingFbl),
        },
        CfgRow {
            section: "scheduler",
            label: "usage max age",
            value: format!("{}s", p.usage_max_age.as_secs()),
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::UsageMaxAge),
        },
        CfgRow {
            section: "scheduler",
            label: "per-account limits",
            value: "per account".into(),
            state: CfgState::ReadOnly,
            note: "accounts tab [L]",
            action: None,
        },
        CfgRow {
            section: "scheduler",
            label: "paused accounts",
            value: "per account".into(),
            state: CfgState::ReadOnly,
            note: "accounts tab / context menu",
            action: None,
        },
        CfgRow {
            section: "codex",
            label: "model",
            value: view.codex.model.clone(),
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::CodexModel),
        },
        CfgRow {
            section: "codex",
            label: "effort",
            value: view
                .codex
                .effort
                .clone()
                .unwrap_or_else(|| "bypass (client)".into()),
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::CodexEffort),
        },
        CfgRow {
            section: "codex",
            label: "fast",
            value: onoff(view.codex.fast),
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::CodexFast),
        },
        CfgRow {
            section: "codex",
            label: "codex upstream",
            value: f.codex_upstream.clone(),
            state: CfgState::Restart,
            note: "confirm",
            action: Some(ConfigAction::CodexUpstream),
        },
        CfgRow {
            section: "codex",
            label: "token url / client model / trace",
            value: "config file".into(),
            state: CfgState::ReadOnly,
            note: "codex.* plumbing in config",
            action: None,
        },
        CfgRow {
            section: "grok",
            label: "effort",
            value: view
                .grok
                .effort
                .clone()
                .unwrap_or_else(|| "bypass (client)".into()),
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::GrokEffort),
        },
        CfgRow {
            section: "grok",
            label: "upstream / model / trace",
            value: "config file".into(),
            state: CfgState::ReadOnly,
            note: "grok.* plumbing in config",
            action: None,
        },
        CfgRow {
            section: "display",
            label: "quota fill",
            value: match quota {
                crate::config::QuotaDisplay::Used => "used%".into(),
                crate::config::QuotaDisplay::Remaining => "remaining%".into(),
            },
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::QuotaFill),
        },
        CfgRow {
            section: "display",
            label: "reset display",
            value: if chrome.reset_absolute {
                "absolute UTC"
            } else {
                "countdown"
            }
            .into(),
            state: CfgState::Session,
            note: "",
            action: Some(ConfigAction::ResetDisplay),
        },
        CfgRow {
            section: "display",
            label: "email mask",
            value: onoff(view.email_anonymous),
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::EmailMask),
        },
        CfgRow {
            section: "display",
            label: "tui effects",
            value: onoff(view.tui_effects),
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::TuiEffects),
        },
        CfgRow {
            section: "display",
            label: "fable weekly gauge",
            value: onoff(view.show_fable_weekly),
            state: CfgState::Live,
            note: "",
            action: Some(ConfigAction::FableWeekly),
        },
        CfgRow {
            section: "display",
            label: "gradient speed",
            value: format!("{:.2}", f.gradient_speed),
            state: CfgState::Restart,
            note: "",
            action: Some(ConfigAction::GradientSpeed),
        },
        CfgRow {
            section: "display",
            label: "gradient colors",
            value: "config file".into(),
            state: CfgState::ReadOnly,
            note: "tui_gradient in config",
            action: None,
        },
        CfgRow {
            section: "display",
            label: "domain abbrev",
            value: "config file".into(),
            state: CfgState::ReadOnly,
            note: "domain_abbrev map in config",
            action: None,
        },
        CfgRow {
            section: "routing",
            label: "enabled",
            value: onoff(f.routing_enabled),
            state: CfgState::Live,
            note: "confirm — reroutes live traffic",
            action: Some(ConfigAction::RoutingEnabled),
        },
        CfgRow {
            section: "routing",
            label: "default group",
            value: f.routing_default_group.clone(),
            state: CfgState::Restart,
            note: "",
            action: Some(ConfigAction::RoutingDefaultGroup),
        },
        CfgRow {
            section: "routing",
            label: "on empty group",
            value: f.routing_on_empty_group.clone(),
            state: CfgState::Restart,
            note: "",
            action: Some(ConfigAction::RoutingOnEmptyGroup),
        },
        CfgRow {
            section: "routing",
            label: "model lists",
            value: "config file".into(),
            state: CfgState::ReadOnly,
            note: "routing.*_models in config",
            action: None,
        },
        CfgRow {
            section: "raw-io",
            label: "capture",
            value: onoff(f.raw_io_enabled),
            state: CfgState::Live,
            note: "confirm — writes request/response payloads to disk",
            action: Some(ConfigAction::RawIoEnabled),
        },
        CfgRow {
            section: "raw-io",
            label: "retention",
            value: format!("{}d", f.raw_io_retention_days),
            state: CfgState::Restart,
            note: "decrease deletes history",
            action: Some(ConfigAction::RawIoRetention),
        },
        CfgRow {
            section: "raw-io",
            label: "max body bytes",
            value: format::human_count(f.raw_io_max_body_bytes),
            state: CfgState::Restart,
            note: "",
            action: Some(ConfigAction::RawIoMaxBody),
        },
        CfgRow {
            section: "daemon",
            label: "upstream",
            value: view.upstream.clone().unwrap_or_else(|| "—".into()),
            state: CfgState::Restart,
            note: "confirm",
            action: Some(ConfigAction::Upstream),
        },
        CfgRow {
            section: "daemon",
            label: "port",
            value: view.port.to_string(),
            state: CfgState::Restart,
            note: "",
            action: Some(ConfigAction::ProxyPort),
        },
        CfgRow {
            section: "daemon",
            label: "max request bytes",
            value: format::human_count(f.proxy_max_request_bytes),
            state: CfgState::Restart,
            note: "",
            action: Some(ConfigAction::ProxyMaxBody),
        },
        CfgRow {
            section: "daemon",
            label: "proxy api key",
            value: "•••".into(),
            state: CfgState::ReadOnly,
            note: "secret (lm-… key)",
            action: None,
        },
        CfgRow {
            section: "daemon",
            label: "idle timeout / idle probe",
            value: "config file".into(),
            state: CfgState::ReadOnly,
            note: "proxy.forward_idle_timeout_secs · proxy.idle_probe.*",
            action: None,
        },
        CfgRow {
            section: "scheduler",
            label: "poll / refresh-ahead",
            value: "config file".into(),
            state: CfgState::ReadOnly,
            note: "scheduler.usage_poll_secs · refresh_ahead_secs",
            action: None,
        },
        CfgRow {
            section: "daemon",
            label: "schema version",
            value: "derived".into(),
            state: CfgState::ReadOnly,
            note: "config version field",
            action: None,
        },
        CfgRow {
            section: "daemon",
            label: "accounts",
            value: format!("{} configured", view.snapshot.accounts.len()),
            state: CfgState::ReadOnly,
            note: "accounts tab (a)",
            action: None,
        },
        CfgRow {
            section: "daemon",
            label: "events",
            value: format!("{} banner(s)", view.events.len()),
            state: CfgState::ReadOnly,
            note: "POST /llmux/events",
            action: None,
        },
        CfgRow {
            section: "daemon",
            label: "pricing overrides",
            value: "config file".into(),
            state: CfgState::ReadOnly,
            note: "pricing map in config",
            action: None,
        },
        CfgRow {
            section: "daemon",
            label: "remote",
            value: "client-side".into(),
            state: CfgState::ReadOnly,
            note: "CLI-only; api_key is secret",
            action: None,
        },
    ]
}

/// Number of rows the config editor shows — the key handler clamps its
/// cursor with this (rows are static per view shape).
pub(crate) fn config_row_count(view: &DashboardView, chrome: &Chrome) -> usize {
    config_rows(view, chrome).len()
}

/// The action on config row `idx`, if that row is editable.
pub(crate) fn config_row_action(
    view: &DashboardView,
    chrome: &Chrome,
    idx: usize,
) -> Option<ConfigAction> {
    config_rows(view, chrome).get(idx).and_then(|r| r.action)
}

/// Config overlay (`c`) — the config EDITOR (config-editor v1, trinity
/// contract C6): every schema field listed with an honest apply-state label;
/// editable rows activate on Enter or a click on the value cell. `live` =
/// applies now (holder-backed), `restart` = persisted, effective next start,
/// `session` = this TUI only, read-only rows say where the value is managed.
fn draw_config_overlay(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    chrome: &Chrome,
    hits: &mut Option<MainChrome>,
) {
    frame.render_widget(Clear, area);
    let rows = config_rows(view, chrome);
    let cursor = chrome.config_cursor.min(rows.len().saturating_sub(1));
    // Reserve the last two rows for the edit/confirm prompt line + padding.
    let body_h = area.height.saturating_sub(3) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut hit_rows: Vec<ConfigHit> = Vec::new();
    let mut last_section = "";
    // Build (section header + row) lines, tracking which screen line each row
    // lands on for the hit list; scroll so the cursor stays visible.
    let mut row_lines: Vec<(usize, Option<usize>)> = Vec::new(); // (line idx, row idx)
    for (i, row) in rows.iter().enumerate() {
        if row.section != last_section {
            last_section = row.section;
            row_lines.push((row_lines.len(), None));
        }
        row_lines.push((row_lines.len(), Some(i)));
    }
    // Find the cursor's line to derive scroll.
    let cursor_line = row_lines
        .iter()
        .find(|(_, r)| *r == Some(cursor))
        .map(|(l, _)| *l)
        .unwrap_or(0);
    let scroll = cursor_line.saturating_sub(body_h.saturating_sub(1));
    for (line_idx, row_idx) in row_lines.iter().skip(scroll).take(body_h) {
        match row_idx {
            None => {
                // Section header: derive from the first row at or after this line.
                let section = rows
                    .iter()
                    .enumerate()
                    .scan("", |prev, (_, r)| {
                        let is_new = r.section != *prev;
                        *prev = r.section;
                        Some((is_new, r.section))
                    })
                    .filter(|(is_new, _)| *is_new)
                    .map(|(_, s)| s)
                    .nth(
                        row_lines[..*line_idx]
                            .iter()
                            .filter(|(_, r)| r.is_none())
                            .count(),
                    )
                    .unwrap_or("");
                lines.push(Line::from(Span::styled(
                    format!(" {section}"),
                    dim().add_modifier(Modifier::BOLD),
                )));
            }
            Some(i) => {
                let row = &rows[*i];
                let selected = *i == cursor;
                let marker = if selected { "▸" } else { " " };
                let (state_label, state_style) = match row.state {
                    CfgState::Live => ("live", Style::new().fg(Color::Green)),
                    CfgState::Restart => ("restart", Style::new().fg(Color::Yellow)),
                    CfgState::Session => ("session", dim()),
                    CfgState::ReadOnly => ("ro", dim()),
                };
                let value_style = if row.action.is_some() {
                    if selected {
                        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(Color::Cyan)
                    }
                } else {
                    dim()
                };
                let label_span = if selected {
                    Span::styled(
                        format!(" {marker} {:<18}", row.label),
                        Style::new().add_modifier(Modifier::REVERSED),
                    )
                } else {
                    Span::styled(format!(" {marker} {:<18}", row.label), dim())
                };
                let value_txt = pad_cells(&row.value, 28);
                // Record the VALUE cell as the click target (contract: click
                // the control, not the whole row).
                if let Some(action) = row.action {
                    let y = area.y + 1 + (lines.len() as u16);
                    let x = area.x + 22;
                    if y < area.bottom() {
                        hit_rows.push(ConfigHit {
                            area: Rect {
                                x,
                                y,
                                width: 28,
                                height: 1,
                            },
                            row: *i,
                            action,
                        });
                    }
                }
                let mut spans = vec![
                    label_span,
                    Span::styled(value_txt, value_style),
                    Span::styled(format!(" {state_label:<8}"), state_style),
                ];
                // A restart-only save this session: the value shown is still
                // the EFFECTIVE boot value; the note carries the pending one.
                if let Some(saved) = row.action.and_then(|a| chrome.config_saved.get(&a)) {
                    spans.push(Span::styled(
                        format!(" saved: {saved} (restart)"),
                        Style::new().fg(Color::Yellow),
                    ));
                }
                if !row.note.is_empty() {
                    spans.push(Span::styled(format!(" {}", row.note), dim()));
                }
                lines.push(Line::from(spans));
            }
        }
    }
    // Edit / confirm prompt line (config-editor v1).
    match chrome.mode {
        Mode::ConfigEdit { action } => {
            let hint = action.input_hint().unwrap_or("");
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled(" edit ", Style::new().fg(Color::Cyan)),
                Span::raw(format!("{}▏", chrome.config_input)),
                Span::styled(format!("   ({hint} · Enter apply · Esc cancel)"), dim()),
            ]));
        }
        Mode::ConfigConfirm { .. } => {
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled(" confirm ", Style::new().fg(Color::Yellow)),
                Span::raw(chrome.config_input.clone()),
                Span::styled("   (y apply · n/Esc cancel)", dim()),
            ]));
        }
        _ => {}
    }
    let block = Block::new()
        .borders(Borders::TOP)
        .title(" config — Enter/click value edits · live|restart|session|ro ");
    frame.render_widget(Paragraph::new(lines).block(block), area);
    if let Some(hits) = hits.as_mut() {
        hits.config_rows = hit_rows;
    }
}

/// The right-click account context menu (UI-3 U11): a small floating list
/// anchored at the click cell, clamped to the frame. Items mirror the key
/// flows exactly: switch now (`Enter` in the switcher), pause/resume (`p`),
/// set limit (`L`), delete (`r` + confirm). Returns the hit layout.
fn draw_context_menu(
    frame: &mut Frame,
    view: &DashboardView,
    ctx: &FrameCtx,
    chrome: &Chrome,
    anchor: (u16, u16),
) -> MenuChrome {
    let Mode::ContextMenu { idx, item } = chrome.mode else {
        return MenuChrome::default();
    };
    // The PINNED account id governs what the menu says — the same identity
    // execution acts on (review R2/R3). The explicit match keeps "no pin"
    // (index fallback — keyboard-driven/test states) distinct from "pinned
    // account VANISHED": a vanished pin must never fall back to whoever now
    // occupies the row (execution would cancel while the menu named someone
    // else) — it renders as gone instead.
    let (target, gone) = match chrome.menu_account.as_deref() {
        Some(name) => {
            let found = view.snapshot.accounts.iter().find(|a| a.id.0 == name);
            (found, found.is_none().then(|| name.to_string()))
        }
        None => (
            ctx.order
                .get(idx)
                .and_then(|&i| view.snapshot.accounts.get(i)),
            None,
        ),
    };
    let paused = target.is_some_and(|a| a.paused);
    let name = match (&gone, target) {
        (Some(pinned), _) => format!(
            "{} — gone",
            row_account_name(pinned, ctx.mask, &view.domain_abbrev)
        ),
        (None, Some(a)) => row_account_name(&a.id.0, ctx.mask, &view.domain_abbrev),
        (None, None) => "?".into(),
    };
    let items: [&str; 4] = [
        "switch now",
        if paused { "resume" } else { "pause" },
        "set limit",
        "delete",
    ];
    let width = (items
        .iter()
        .map(|i| i.chars().count())
        .max()
        .unwrap_or(0)
        .max(name.chars().count() + 1) as u16
        + 4)
    .min(frame.area().width);
    let height = items.len() as u16 + 1; // + title border row
    let frame_area = frame.area();
    let x = anchor
        .0
        .min(frame_area.right().saturating_sub(width))
        .max(frame_area.x);
    let y = anchor
        .1
        .saturating_add(1)
        .min(frame_area.bottom().saturating_sub(height))
        .max(frame_area.y);
    let area = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, area);
    let mut lines: Vec<Line> = Vec::with_capacity(items.len());
    let mut rects: Vec<Rect> = Vec::with_capacity(items.len());
    for (i, label) in items.iter().enumerate() {
        let style = if i == item {
            Style::new()
                .fg(Color::Cyan)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new()
        };
        lines.push(Line::from(Span::styled(format!(" {label} "), style)));
        rects.push(Rect {
            x: area.x,
            y: area.y + 1 + i as u16,
            width: area.width,
            height: 1,
        });
    }
    let block = Block::new()
        .borders(Borders::TOP)
        .title(format!(" {name} "));
    frame.render_widget(Paragraph::new(lines).block(block), area);
    MenuChrome { area, items: rects }
}

/// The rect an overlay draws into. Rows left visible at the top: header + tab
/// strip, plus the event banner ONLY while one is active. Keeping the tab row
/// on screen is what makes its always-armed click targets legitimate (review
/// R1 MUST-FIX 3: overlays used to paint over the tabs while their hit boxes
/// stayed live — invisible navigation). The reservation is banner-aware: a
/// fixed 3-row reserve assumed the banner row, so with no active banner MAIN's
/// accounts-table border (` accounts ────`) leaked through as a stale
/// separator right under the tab bar on EVERY tab (Z 2026-07-16).
fn overlay_rect(area: Rect, banner: bool) -> Rect {
    let top = (2 + u16::from(banner)).min(area.height);
    Rect {
        x: area.x,
        y: area.y + top,
        width: area.width,
        height: area.height.saturating_sub(2).saturating_sub(top),
    }
}

/// A rect centered in `area` at `pct_w`%/`pct_h`% of its size (UI-6 item 3
/// modal). Clamped so it never exceeds `area`.
fn centered_rect(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let width = ((area.width as u32 * pct_w as u32) / 100).min(area.width as u32) as u16;
    let height = ((area.height as u32 * pct_h as u32) / 100).min(area.height as u32) as u16;
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Number of terminal rows `text` occupies when wrapped at `width` cells to
/// match ratatui's `Wrap { trim: false }`: greedy word wrap, whitespace is
/// rendered (leading/interior spaces occupy cells, never dropped from the width
/// tally), over-wide words hard-split, explicit newlines honored. Used to bound
/// the input modal's scroll (UI-6 item 3).
///
/// INVARIANT: this NEVER underestimates the row count. Ratatui trims trailing
/// whitespace on wrapped rows, which we may charge — so we can overshoot by a
/// row or two, but never undershoot. That direction matters: an underestimate
/// would shrink `max_scroll`, letting the post-draw clamp cut off the tail of a
/// long/indented prompt (operator can't reach "전체 input"); an overestimate at
/// most leaves a blank overscroll row while the tail stays reachable.
fn wrapped_line_count(text: &str, width: u16) -> usize {
    let width = (width as usize).max(1);
    let mut rows = 0usize;
    for source in text.split('\n') {
        rows += wrap_rows(source, width);
    }
    rows.max(1)
}

/// Rows one newline-free `source` string wraps into at `width` cells. Iterates
/// GRAPHEME CLUSTERS (the unit ratatui measures/renders), grouping non-space
/// clusters into words and treating each space as its own 1-cell token, so that
/// EVERY cell — including leading indentation — is charged, then greedily packs
/// them, hard-splitting any token too wide for a full row. Empty string counts
/// as 1 row.
fn wrap_rows(source: &str, width: usize) -> usize {
    let mut rows = 1usize;
    let mut col = 0usize;
    let mut word = String::new();
    for g in source.graphemes(true) {
        if g == " " {
            if !word.is_empty() {
                place_token(&word, width, &mut rows, &mut col);
                word.clear();
            }
            // Each space is its own 1-cell token — leading/interior spaces
            // occupy width exactly as `Wrap { trim: false }` renders them.
            place_token(" ", width, &mut rows, &mut col);
        } else {
            word.push_str(g);
        }
    }
    if !word.is_empty() {
        place_token(&word, width, &mut rows, &mut col);
    }
    rows
}

/// Place one token (a word or a single space) into the greedy wrap accounting,
/// advancing `rows`/`col`. A token that overflows the current row moves to a
/// fresh one; a token wider than a whole row hard-splits by GRAPHEME CLUSTER
/// using str-based cell width — the identical basis ratatui measures with — so
/// a multi-scalar cluster (a 2-cell CJK glyph, an emoji + VS16) is never split
/// mid-cluster and never mis-measured by summing scalar widths.
fn place_token(token: &str, width: usize, rows: &mut usize, col: &mut usize) {
    let w = cell_width(token);
    if w == 0 {
        return;
    }
    if *col + w <= width {
        *col += w;
        return;
    }
    // Does not fit on the current row: break to a fresh one first.
    if *col > 0 {
        *rows += 1;
        *col = 0;
    }
    if w <= width {
        *col = w;
        return;
    }
    // Token wider than a full row: hard-split, charging each cluster's cell width.
    for g in token.graphemes(true) {
        let cw = cell_width(g);
        if cw == 0 {
            continue;
        }
        if *col + cw > width {
            *rows += 1;
            *col = 0;
        }
        *col += cw;
    }
}

/// Draw the click-opened input-text modal (UI-6 item 3): a centered, bordered,
/// scrollable box showing an entry's FULL stored excerpt. The content is looked
/// up from `view.completed` by the modal's stable key EVERY frame, so it renders
/// identically in local and attach mode and needs no extra wire field. Returns
/// `Some(max_scroll)` (wrapped line count minus the visible inner height) when
/// the entry was found and drawn, or `None` when it has aged out of the ring —
/// the caller then closes the modal. `Clear` blanks the rect first, so nothing
/// beneath shows through.
fn draw_input_modal(frame: &mut Frame, view: &DashboardView, modal: &InputModal) -> Option<u16> {
    // Look up the clicked entry by its stable key; a miss = it aged out → close.
    let entry = view
        .completed
        .iter()
        .find(|c| c.activity_key().as_ref() == Some(&modal.key))?;
    let CompletedBody::Request { kind, excerpt, .. } = &entry.body else {
        return None;
    };
    let excerpt = excerpt.as_deref()?;

    let area = centered_rect(frame.area(), 80, 80);
    frame.render_widget(Clear, area);

    let title = format!(
        " 🔍 input — {} · {} ",
        kind.as_deref().unwrap_or("?"),
        format::clock_hms_utc(entry.at),
    );
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(dim())
        .title(Span::styled(
            title,
            Style::new().add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(Span::styled(" ↑↓ scroll · esc close ", dim())).centered());
    let inner = block.inner(area);

    let text = masked_text(excerpt, view.email_anonymous);
    let total = wrapped_line_count(&text, inner.width) as u16;
    let max_scroll = total.saturating_sub(inner.height);
    let scroll = modal.scroll.min(max_scroll);

    let para = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(para, area);
    Some(max_scroll)
}

// ---------------------------------------------------------------------------
// Raw request/response viewer (UI-7): a CDT-style modal over MAIN with
// Request/Response tabs — general metadata, headers, and the FULL captured
// body, JSON pretty-printed with syntax highlighting.
// ---------------------------------------------------------------------------

/// Prebuilt, immutable content of the raw viewer — one tab per captured
/// payload leg, built ONCE off the UI thread when the raw-io record arrives
/// (a body can be 8 MiB; per-frame construction would stutter the render
/// loop). The modal holds it behind an `Arc` so the per-frame `Chrome` clone
/// stays cheap.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawContent {
    /// Payload tabs in wire order (UI-8): client→llmux request, llmux→api
    /// request, api→llmux response, llmux→client response. The upstream pair
    /// exists only on TRANSLATED exchanges (codex/grok) — the claude
    /// passthrough is byte-identity, so those records show the classic 2 tabs.
    pub tabs: Vec<RawTabContent>,
    /// Whole-record pretty JSON — the `save all` payload.
    pub record_json: String,
    /// Every tab's labeled plain body concatenated — the `copy all` payload.
    pub all_text: String,
}

/// One payload tab: prebuilt styled lines plus the plain-text payloads the
/// copy/save actions hand out (prebuilt too — a button press must not walk
/// megabytes of styled spans on the UI thread).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawTabContent {
    pub label: &'static str,
    pub lines: Vec<Line<'static>>,
    /// Widest line in display cells — the horizontal scroll bound.
    pub width: u16,
    /// The raw body verbatim — the `copy` / `save` payload.
    pub body_text: String,
    /// `copy as curl` for this tab's SIDE of the exchange: client tabs carry
    /// the client request against llmux, upstream tabs the rewritten request
    /// against the provider (a response is reproduced by replaying its
    /// request).
    pub curl: String,
}

/// What the opener knows at click time, threaded into the off-thread content
/// build: the general metadata lines plus what the curl builder needs — the
/// raw-io record stores neither the client method/path nor the local base
/// URL.
#[derive(Debug, Clone)]
pub(crate) struct RawGeneral {
    pub lines: Vec<Line<'static>>,
    pub method: String,
    pub path: String,
    /// `http://localhost:<port>` (local) or the attach base URL (remote).
    pub base_url: String,
}

/// Reconstruct a copy-pasteable `curl` for one side of the exchange. Redacted
/// header values stay verbatim (`•••redacted` — the user substitutes real
/// credentials); headers curl manages itself (content-length, host,
/// accept-encoding) are dropped so the command replays cleanly. Single quotes
/// are shell-escaped.
fn curl_command(
    method: &str,
    url: &str,
    headers: Option<&[(String, String)]>,
    body: &str,
) -> String {
    let sh = |t: &str| t.replace('\'', "'\\''");
    // Quote the method too: `http::Method` accepts RFC 7230 `tchar`
    // extension tokens (backtick, `$`, `|`, `&`, `'`…), and the client's raw
    // method rides into here — an unquoted `-X `id`` would run a command
    // substitution when the operator pastes the "copy as curl" output.
    let mut out = format!("curl -X '{}' '{}'", sh(method), sh(url));
    for (name, value) in headers.unwrap_or_default() {
        if matches!(
            name.as_str(),
            "content-length" | "host" | "accept-encoding" | "connection"
        ) {
            continue;
        }
        out.push_str(&format!(" \\\n  -H '{}: {}'", sh(name), sh(value)));
    }
    if !body.is_empty() {
        out.push_str(&format!(" \\\n  --data-raw '{}'", sh(body)));
    }
    out
}

/// Assemble one [`RawTabContent`], measuring the widest line for the
/// horizontal scroll bound.
fn raw_tab(
    label: &'static str,
    lines: Vec<Line<'static>>,
    body_text: String,
    curl: String,
) -> RawTabContent {
    let width = lines.iter().map(Line::width).max().unwrap_or(0);
    RawTabContent {
        label,
        lines,
        width: u16::try_from(width).unwrap_or(u16::MAX),
        body_text,
        curl,
    }
}

/// Hard wrap applied to raw body lines at build time (cells are clipped by the
/// Paragraph anyway; the wrap bounds the cost of cloning the VISIBLE slice per
/// frame — a truncated non-JSON stream body can be one single 8 MiB line).
const RAW_LINE_WRAP: usize = 2048;

/// Section header line (`── request headers ──` style) for the raw viewer.
fn raw_section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("── {title} ──"),
        Style::new()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ))
}

/// One `name: value` header line (name cyan, value plain — CDT's palette).
fn raw_header_line(name: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name}: "), Style::new().fg(Color::Cyan)),
        Span::raw(value.to_string()),
    ])
}

/// Header block for one side of the exchange: each captured pair, or a single
/// dim placeholder when the record predates header capture.
fn raw_header_lines(headers: Option<&[(String, String)]>) -> Vec<Line<'static>> {
    match headers {
        Some(pairs) if !pairs.is_empty() => {
            pairs.iter().map(|(n, v)| raw_header_line(n, v)).collect()
        }
        Some(_) => vec![Line::from(Span::styled("(no headers)", dim()))],
        None => vec![Line::from(Span::styled(
            "(not captured — record predates header capture)",
            dim(),
        ))],
    }
}

/// Highlight one line of (pretty-printed) JSON into styled spans: keys cyan,
/// string values green, numbers yellow, `true`/`false`/`null` magenta,
/// punctuation dim. Pretty-printed JSON never splits a string across lines
/// (escapes keep them single-line), so a per-line pass is lossless. Scanning
/// is byte-based but only ever splits at ASCII bytes — always char-safe.
fn highlight_json_line(line: &str) -> Line<'static> {
    let bytes = line.as_bytes();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                let end = i.min(bytes.len());
                let is_key = line[end..].trim_start().starts_with(':');
                let color = if is_key { Color::Cyan } else { Color::Green };
                spans.push(Span::styled(
                    line[start..end].to_string(),
                    Style::new().fg(color),
                ));
            }
            b'0'..=b'9' | b'-' => {
                let start = i;
                while i < bytes.len()
                    && matches!(bytes[i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                {
                    i += 1;
                }
                spans.push(Span::styled(
                    line[start..i].to_string(),
                    Style::new().fg(Color::Yellow),
                ));
            }
            b't' | b'f' | b'n' => {
                let rest = &line[i..];
                match ["true", "false", "null"]
                    .iter()
                    .find(|k| rest.starts_with(*k))
                {
                    Some(k) => {
                        spans.push(Span::styled(
                            (*k).to_string(),
                            Style::new().fg(Color::Magenta),
                        ));
                        i += k.len();
                    }
                    None => {
                        spans.push(Span::raw(rest[..1].to_string()));
                        i += 1;
                    }
                }
            }
            _ => {
                // Whitespace, punctuation, and any non-ASCII bytes: batch until
                // the next token start. ASCII stops are always char boundaries.
                let start = i;
                i += 1;
                while i < bytes.len()
                    && !matches!(bytes[i], b'"' | b'0'..=b'9' | b'-' | b't' | b'f' | b'n')
                {
                    i += 1;
                }
                spans.push(Span::styled(line[start..i].to_string(), dim()));
            }
        }
    }
    Line::from(spans)
}

/// Hard-wrap a single logical line into `RAW_LINE_WRAP`-char chunks on char
/// boundaries (continuation chunks lose highlighting context by design — they
/// only occur on machine-generated monster lines).
fn wrap_raw_line(line: &str) -> Vec<&str> {
    if line.len() <= RAW_LINE_WRAP {
        return vec![line];
    }
    let mut chunks = Vec::new();
    let mut rest = line;
    while rest.len() > RAW_LINE_WRAP {
        let mut end = RAW_LINE_WRAP;
        while end > 0 && !rest.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            break; // pathological; emit whole remainder below
        }
        chunks.push(&rest[..end]);
        rest = &rest[end..];
    }
    chunks.push(rest);
    chunks
}

/// Render a captured body as styled lines. A body that parses as ONE JSON
/// document is pretty-printed and highlighted (the CDT "preview" view). An
/// SSE / plain-text body keeps its own lines; `data: {json}` SSE lines get
/// their payload highlighted inline. Anything else renders plain.
pub(crate) fn raw_body_lines(body: &str) -> Vec<Line<'static>> {
    if body.is_empty() {
        return vec![Line::from(Span::styled("(empty body)", dim()))];
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Ok(pretty) = serde_json::to_string_pretty(&value) {
            return pretty
                .lines()
                .flat_map(|l| {
                    wrap_raw_line(l)
                        .into_iter()
                        .map(highlight_json_line)
                        .collect::<Vec<_>>()
                })
                .collect();
        }
    }
    body.lines()
        .flat_map(|line| {
            wrap_raw_line(line)
                .into_iter()
                .map(|chunk| {
                    if let Some(rest) = chunk.strip_prefix("data: ") {
                        if serde_json::from_str::<serde_json::Value>(rest).is_ok() {
                            let mut spans = vec![Span::styled("data: ".to_string(), dim())];
                            spans.extend(highlight_json_line(rest).spans);
                            return Line::from(spans);
                        }
                    }
                    Line::from(Span::raw(chunk.to_string()))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// General-metadata lines for the raw viewer's Request tab, built at OPEN time
/// from the activity entry (the raw-io record repeats only a subset). Mirrors
/// CDT's "General" block. `id == 0` marks a pre-UI-7 entry with no raw link.
pub(crate) fn raw_general_lines(entry: &Completed) -> Vec<Line<'static>> {
    let CompletedBody::Request {
        id,
        method,
        path,
        account,
        status,
        duration,
        group,
        model,
        effort,
        fast,
        user_id,
        kind,
        ..
    } = &entry.body
    else {
        return Vec::new();
    };
    let field = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!("{label:<12}"), Style::new().fg(Color::Cyan)),
            Span::raw(value),
        ])
    };
    let status_color = if *status < 400 {
        Color::Green
    } else {
        Color::Red
    };
    let model_label = match (group.as_deref(), model.as_deref()) {
        (Some(g), Some(m)) => format!("{g} {m}"),
        (Some(g), None) => g.to_string(),
        (None, Some(m)) => m.to_string(),
        (None, None) => "—".to_string(),
    };
    let effort_label = effort
        .as_deref()
        .map(|e| format!(" · effort {e}"))
        .unwrap_or_default();
    let fast_label = match fast {
        Some(true) => " · fast",
        Some(false) => "",
        None => " · fast?", // pre-field history: unknown, never claimed off
    };
    let mut lines = vec![
        raw_section("general"),
        field("request", format!("{method} {path}")),
        Line::from(vec![
            Span::styled(format!("{:<12}", "status"), Style::new().fg(Color::Cyan)),
            Span::styled(status.to_string(), Style::new().fg(status_color)),
            Span::styled(
                format!("  ·  {} elapsed", format::elapsed_secs(*duration)),
                dim(),
            ),
        ]),
        field("time", format!("{} UTC", format::clock_hms_utc(entry.at))),
        field("id", format!("#{id}")),
        field("model", format!("{model_label}{effort_label}{fast_label}")),
        field("account", account.as_deref().unwrap_or("?").to_string()),
    ];
    if let Some(kind) = kind.as_deref() {
        lines.push(field("kind", kind.to_string()));
    }
    if let Some(uid) = user_id.as_deref() {
        lines.push(field("client", uid.to_string()));
    }
    lines
}

/// Build the full per-tab content from a fetched raw-io record + what the
/// opener captured from the entry ([`RawGeneral`]). Runs OFF the UI thread.
/// Tab order is the wire order (UI-8): client request → upstream request →
/// upstream response → client response; the upstream pair appears only when
/// the record carries an `upstream` half (translated exchanges).
pub(crate) fn raw_content_from_record(
    general: RawGeneral,
    record: &crate::proxy::raw_io::RawIoRecord,
) -> RawContent {
    let client_url = format!("{}{}", general.base_url.trim_end_matches('/'), general.path);
    let client_curl = curl_command(
        &general.method,
        &client_url,
        record.request_headers.as_deref(),
        &record.request_body,
    );

    let mut tabs: Vec<RawTabContent> = Vec::new();

    // Tab 1: the request Claude Code sent llmux.
    let mut lines = general.lines;
    lines.push(Line::default());
    lines.push(raw_section("request headers"));
    lines.extend(raw_header_lines(record.request_headers.as_deref()));
    lines.push(Line::default());
    lines.push(raw_section(&format!(
        "request body · {} bytes",
        record.request_body.len()
    )));
    lines.extend(raw_body_lines(&record.request_body));
    tabs.push(raw_tab(
        "Request",
        lines,
        record.request_body.clone(),
        client_curl.clone(),
    ));

    // Tabs 2+3: the rewritten exchange with the provider (translate path).
    if let Some(up) = record.upstream.as_ref() {
        let upstream_curl = curl_command(
            &general.method,
            up.url.as_deref().unwrap_or("<upstream url not captured>"),
            up.request_headers.as_deref(),
            up.request_body.as_deref().unwrap_or(""),
        );
        if up.url.is_some() || up.request_body.is_some() || up.request_headers.is_some() {
            let mut lines = vec![raw_section("upstream request — llmux → api")];
            if let Some(url) = up.url.as_deref() {
                lines.push(raw_header_line("url", url));
            }
            lines.push(Line::default());
            lines.push(raw_section("request headers"));
            lines.extend(raw_header_lines(up.request_headers.as_deref()));
            lines.push(Line::default());
            let body = up.request_body.clone().unwrap_or_default();
            lines.push(raw_section(&format!("request body · {} bytes", body.len())));
            lines.extend(raw_body_lines(&body));
            tabs.push(raw_tab("Upstream Req", lines, body, upstream_curl.clone()));
        }
        if up.response_body.is_some() || up.response_headers.is_some() {
            let mut lines = vec![raw_section("upstream response — api → llmux")];
            lines.push(Line::default());
            lines.push(raw_section("response headers"));
            lines.extend(raw_header_lines(up.response_headers.as_deref()));
            lines.push(Line::default());
            let body = up.response_body.clone().unwrap_or_default();
            lines.push(raw_section(&format!(
                "response body · {} bytes",
                body.len()
            )));
            lines.extend(raw_body_lines(&body));
            tabs.push(raw_tab("Upstream Resp", lines, body, upstream_curl));
        }
    }

    // Last tab: the response llmux delivered to Claude Code.
    let mut lines = vec![raw_section("response")];
    lines.push(Line::from(vec![
        Span::styled(format!("{:<12}", "status"), Style::new().fg(Color::Cyan)),
        Span::raw(
            record
                .status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "?".into()),
        ),
    ]));
    lines.push(Line::default());
    lines.push(raw_section("response headers"));
    lines.extend(raw_header_lines(record.response_headers.as_deref()));
    lines.push(Line::default());
    lines.push(raw_section(&format!(
        "response body · {} bytes",
        record.response_body.len()
    )));
    lines.extend(raw_body_lines(&record.response_body));
    tabs.push(raw_tab(
        "Response",
        lines,
        record.response_body.clone(),
        client_curl,
    ));

    let all_text = tabs
        .iter()
        .map(|t| format!("── {} ──\n{}\n\n", t.label, t.body_text))
        .collect::<String>();
    let record_json = serde_json::to_string_pretty(record).unwrap_or_else(|_| "{}".to_string());
    RawContent {
        tabs,
        record_json,
        all_text,
    }
}

/// Draw the raw request/response viewer (UI-7/UI-8): a near-full-screen modal
/// with a payload tab bar (2 or 4 tabs), top-right action buttons, the
/// prebuilt content scrolled by whole lines both ways, and proportional
/// scrollbars on the right + bottom edges. Returns the frame's
/// [`RawModalChrome`] (scroll clamps + click rects) so the runtime can clamp
/// offsets and route clicks.
fn draw_raw_modal(frame: &mut Frame, modal: &RawModal) -> RawModalChrome {
    let area = centered_rect(frame.area(), 94, 92);
    frame.render_widget(Clear, area);

    // The bottom hint doubles as the action-feedback line: a fresh flash
    // ("copied 4132 bytes → pbcopy", "saved → ~/Downloads/…") replaces the key
    // legend until it expires.
    let hint = match &modal.flash {
        Some((msg, at)) if at.elapsed() < std::time::Duration::from_secs(3) => Line::from(
            Span::styled(format!(" {msg} "), Style::new().fg(Color::Yellow)),
        )
        .centered(),
        _ => Line::from(Span::styled(
            " ←→/tab switch · ↑↓ scroll · H/L pan · c copy · C curl · a copy all · s save · S save all · esc ",
            dim(),
        ))
        .centered(),
    };
    // Action buttons ride the top border, right-aligned (UI-8). Reserve their
    // width FIRST and clip the title to what's left (+ a 1-cell gap) so the
    // buttons — the requested interactive affordance — never overwrite the
    // title on a narrow (~80-col) terminal; the title degrades instead.
    const BUTTONS: [RawButton; 5] = [
        RawButton::Copy,
        RawButton::CopyCurl,
        RawButton::CopyAll,
        RawButton::Save,
        RawButton::SaveAll,
    ];
    let btn_w = |b: RawButton| b.label().len() as u16 + 2; // " label "
    let total: u16 = BUTTONS.iter().map(|b| btn_w(*b)).sum::<u16>() + (BUTTONS.len() as u16 - 1);
    let border_row_w = area.width.saturating_sub(2);
    let buttons_fit = total <= border_row_w;
    let title_budget = if buttons_fit {
        border_row_w.saturating_sub(total + 1)
    } else {
        border_row_w
    };
    let title = truncate_cells(&modal.title, usize::from(title_budget));

    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(dim())
        .title(Span::styled(
            title,
            Style::new().add_modifier(Modifier::BOLD),
        ))
        .title_bottom(hint);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut chrome = RawModalChrome::default();
    if inner.height < 2 || inner.width == 0 {
        return chrome;
    }

    // Rendered as an exact-rect Paragraph so the recorded hit rects are
    // authoritative.
    if buttons_fit {
        let mut x = area.x + 1 + border_row_w - total;
        let mut spans: Vec<Span<'static>> = Vec::new();
        let row_area = Rect {
            x,
            y: area.y,
            width: total,
            height: 1,
        };
        for (i, btn) in BUTTONS.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("│", dim()));
                x += 1;
            }
            let w = btn_w(*btn);
            chrome.buttons.push((
                *btn,
                Rect {
                    x,
                    y: area.y,
                    width: w,
                    height: 1,
                },
            ));
            spans.push(Span::styled(
                format!(" {} ", btn.label()),
                Style::new().fg(Color::Cyan),
            ));
            x += w;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), row_area);
    }

    // Content viewport: tab bar (row 0) + spacer, right column + bottom row
    // reserved for the scrollbars.
    let viewport = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width.saturating_sub(1),
        height: inner.height.saturating_sub(3),
    };
    let content = match &modal.state {
        RawModalState::Loading => {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {} loading raw record…", anim::braille_spin(modal.spin)),
                    Style::new().fg(Color::Yellow),
                ))),
                viewport,
            );
            return chrome;
        }
        RawModalState::Failed(msg) => {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {msg}"),
                    Style::new().fg(Color::Red),
                )))
                .wrap(Wrap { trim: false }),
                viewport,
            );
            return chrome;
        }
        RawModalState::Ready(content) => content,
    };

    // Tab bar: one hit rect per label, active tab inverted.
    let active = modal.tab.min(content.tabs.len().saturating_sub(1));
    let mut tab_spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let mut x = inner.x + 1;
    for (i, t) in content.tabs.iter().enumerate() {
        let w = t.label.len() as u16 + 2;
        chrome.tabs.push((
            i,
            Rect {
                x,
                y: inner.y,
                width: w,
                height: 1,
            },
        ));
        tab_spans.push(if i == active {
            Span::styled(
                format!(" {} ", t.label),
                Style::new()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {} ", t.label), dim())
        });
        tab_spans.push(Span::raw(" "));
        x += w + 1;
    }
    frame.render_widget(
        Paragraph::new(Line::from(tab_spans)),
        Rect { height: 1, ..inner },
    );

    if viewport.height == 0 || viewport.width == 0 {
        return chrome;
    }
    let tab = &content.tabs[active];
    let lines = &tab.lines;
    let max_v = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_sub(viewport.height);
    let max_h = tab.width.saturating_sub(viewport.width);
    chrome.max_scroll = (max_v, max_h);
    let scroll = usize::from(modal.scroll.min(max_v));
    let hscroll = modal.hscroll.min(max_h);
    let visible: Vec<Line<'static>> = lines
        .iter()
        .skip(scroll)
        .take(usize::from(viewport.height))
        .cloned()
        .collect();
    frame.render_widget(Paragraph::new(visible).scroll((0, hscroll)), viewport);

    // Proportional scrollbars (UI-8): right edge = vertical, bottom =
    // horizontal. Rendered only when the content overflows that axis.
    if max_v > 0 {
        let mut state =
            ScrollbarState::new(usize::from(max_v)).position(usize::from(modal.scroll.min(max_v)));
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(dim())
                .thumb_style(Style::new().fg(Color::Cyan)),
            Rect {
                x: inner.right().saturating_sub(1),
                y: viewport.y,
                width: 1,
                height: viewport.height,
            },
            &mut state,
        );
    }
    if max_h > 0 {
        let mut state = ScrollbarState::new(usize::from(max_h)).position(usize::from(hscroll));
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
                .style(dim())
                .thumb_style(Style::new().fg(Color::Cyan)),
            Rect {
                x: viewport.x,
                y: inner.bottom().saturating_sub(1),
                width: viewport.width,
                height: 1,
            },
            &mut state,
        );
    }
    chrome
}

/// Attach-mode pre-first-document screen: identity + a "connecting…" /
/// reconnect line, plus the footer so `q` is discoverable.
fn draw_connecting(frame: &mut Frame, chrome: &Chrome) {
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    let mut header = vec![
        Span::styled(
            " llmux ",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(crate::build_info::version_with_build(), dim()),
    ];
    header.extend(attach_spans(chrome));
    frame.render_widget(Paragraph::new(Line::from(header)), header_area);

    let connecting = match chrome.attach {
        Some(attach) if attach.connected => "connecting — waiting for the first document…",
        _ => "connecting to daemon — retrying…",
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {connecting}"),
            Style::new().fg(Color::Yellow),
        ))),
        body_area,
    );
    // No document yet → no live setting to honor; nothing here shows emails.
    draw_footer(frame, footer_area, chrome, false);
}

/// Attach-mode header markers: `attached → pid N` (or `pid ?`), turning into a
/// red `reconnecting…` while the poller cannot reach the daemon. Empty in
/// local mode.
fn attach_spans(chrome: &Chrome) -> Vec<Span<'static>> {
    let Some(attach) = chrome.attach else {
        return Vec::new();
    };
    let pid = attach
        .pid
        .map(|p| p.to_string())
        .unwrap_or_else(|| "?".into());
    if attach.connected {
        vec![Span::styled(
            format!(" attached → pid {pid} "),
            Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        )]
    } else {
        vec![Span::styled(
            format!(" reconnecting → pid {pid}… "),
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        )]
    }
}

fn draw_header(frame: &mut Frame, area: Rect, view: &DashboardView, chrome: &Chrome) {
    // glance-triage atom 1: the header row IS the health verdict — always
    // present (green included: a positive signal, never health-by-absence),
    // zero added height, [OK]/[WARN]/[FAIL] text first so the state survives
    // a no-truecolor panel. Identity (version/port/pid/up) compresses right.
    let now = SystemTime::now();
    let verdict = triage::health_verdict(view, now);
    let (tag, color) = match verdict.level() {
        VerdictLevel::Ok => ("[OK]", Color::Green),
        VerdictLevel::Warn => ("[WARN]", Color::Yellow),
        VerdictLevel::Fail => ("[FAIL]", Color::Red),
    };
    let mut spans = vec![Span::styled(
        format!(" {tag} "),
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    )];
    match verdict.headline() {
        Some(condition) => {
            spans.push(Span::styled(
                condition.text.clone(),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            ));
            if let Some(account) = &condition.account {
                spans.push(Span::styled(
                    format!(" {}", masked_name(account, view.email_anonymous)),
                    Style::new().fg(color),
                ));
            }
            if verdict.more() > 0 {
                spans.push(Span::styled(format!(" +{}", verdict.more()), dim()));
            }
        }
        None => {
            spans.push(Span::styled("healthy", Style::new().fg(color)));
            // An old daemon sends no health telemetry: say the err surface
            // is unavailable — never a fabricated healthy zero.
            let err = view
                .health
                .map_or("—".to_string(), |h| h.errors.to_string());
            spans.push(Span::styled(
                format!(" · {:.1} req/m · {err} err/5m", view.rpm_5m),
                dim(),
            ));
        }
    }
    spans.push(Span::styled(
        format!(
            "  llmux {} :{} pid {} up {} ·{}",
            view.display_version(),
            view.port,
            view.pid,
            format::countdown(view.uptime),
            view.snapshot.accounts.len()
        ),
        dim(),
    ));
    if let Some(upstream) = &view.upstream {
        spans.push(Span::styled(format!(" → {upstream} "), dim()));
    }
    if let Some(path) = &view.config_path {
        spans.push(Span::styled(format!(" cfg {path} "), dim()));
    }
    spans.extend(attach_spans(chrome));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_accounts(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    chrome: &Chrome,
) -> Vec<AccountRowHit> {
    let snapshot = &view.snapshot;
    let block = Block::new().borders(Borders::TOP).title(" accounts ");
    if snapshot.accounts.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "no accounts — run `llmux login` or `llmux import`, then press R",
            Style::new().fg(Color::Yellow),
        )))
        .block(block);
        frame.render_widget(empty, area);
        return Vec::new();
    }
    let show_fable = view.show_fable_weekly;
    // The account column is a fixed `Length(name_width)` that fits the widest
    // display name up to NAME_COL_MAX (floor = the header word "account").
    // Because it is Length, not Min, leftover width after the fixed data
    // columns is NO LONGER poured into it (Z 2026-07-13: supersedes the
    // 2026-07-09 "남는 공간을 account에 최대 할당" directive — that made the
    // column too wide). Names longer than the cap are clipped by the cell.
    let name_width = (ctx
        .order
        .iter()
        .map(|&idx| {
            row_account_name(&snapshot.accounts[idx].id.0, ctx.mask, &view.domain_abbrev)
                .chars()
                .count()
        })
        .max()
        .unwrap_or(0)
        .max("account".len()) as u16)
        .min(NAME_COL_MAX);
    // The wide column set is used whenever it actually FITS (Z 2026-07-13:
    // the fixed 150 threshold predated the NAME_COL_MAX cap — at ~149 cols it
    // hid req/tok and poured the width into fat bars instead). Minimum wide
    // width = the wide fixed columns + minimum-size gauges + column spacing,
    // mirroring the bar_width math below. WIDE_TABLE_AT now governs only the
    // models table.
    let wide = {
        let n_gauges = 2 + show_fable as usize;
        let min_wide = 47 + name_width as usize + n_gauges * QUOTA_CELL_WIDTH + (8 + n_gauges - 1);
        area.width as usize >= min_wide
    };
    // Leftover terminal width — everything past the FIXED columns and the 1-col
    // inter-column spacing — is poured into the quota gauge BARS instead of
    // dying as dead space on the right (Z 2026-07-13, follow-up to the
    // NAME_COL_MAX cap: every column is a fixed `Length` now, so without this
    // the table packs left and wastes the right edge). Each STRETCHY gauge
    // column grows its bar by an equal share of the leftover; the compact
    // narrow Fbl marker never stretches. Floor = QUOTA_BAR_WIDTH (leftover 0 →
    // today's exact layout), ceiling = GAUGE_BAR_MAX.
    let bar_width = {
        // `fixed_total` sums the constraint Lengths exactly as built below
        // (name + the base QUOTA_CELL_WIDTH gauge cells + the conditional
        // compact-7 Fbl marker) plus the (ncols - 1) inter-column spaces; the
        // `n_gauges` stretchy gauge columns share whatever width is left.
        let n_gauges = if wide { 2 + show_fable as usize } else { 2 };
        let (fixed_total, ncols) = if wide {
            // marker 2 + group 7 + # 2 + status 20 + if 3 + req 6 + tok 7 = 47.
            let ncols = 8 + n_gauges;
            let fixed = 47 + name_width as usize + n_gauges * QUOTA_CELL_WIDTH;
            (fixed, ncols)
        } else {
            // marker 2 + group 7 + # 2 + status 20 + if 3 = 34, + the compact
            // 7-wide Fbl marker when shown (it does NOT stretch).
            let ncols = 8 + show_fable as usize;
            let fixed =
                34 + name_width as usize + 2 * QUOTA_CELL_WIDTH + if show_fable { 7 } else { 0 };
            (fixed, ncols)
        };
        let leftover = (area.width as usize).saturating_sub(fixed_total + (ncols - 1));
        (QUOTA_BAR_WIDTH + leftover / n_gauges).min(GAUGE_BAR_MAX)
    };
    let gauge_cell = (bar_width + 1 + QUOTA_LABEL_WIDTH) as u16;

    let selected = match chrome.mode {
        Mode::Select { idx }
        | Mode::ConfirmRemove { idx }
        | Mode::EditLimits { idx }
        | Mode::ContextMenu { idx, .. } => Some(idx.min(ctx.order.len().saturating_sub(1))),
        // NewLogin is a provider picker, not an account-row cursor; the
        // config-editor modes live in the Config overlay.
        Mode::Normal
        | Mode::AddKey
        | Mode::NewLogin { .. }
        | Mode::ConfigEdit { .. }
        | Mode::ConfigConfirm { .. } => None,
    };
    let rows = ctx.order.iter().enumerate().map(|(pos, &account_idx)| {
        let account = &snapshot.accounts[account_idx];
        let cursor = selected == Some(pos);
        let row = account_row(account, view, ctx, pos, wide, cursor, bar_width);
        if cursor {
            row.style(Style::new().add_modifier(Modifier::REVERSED))
        } else {
            row
        }
    });

    // "group" (claude/codex — the model group, colored + prominent) leads the
    // data columns. Issue #70: the default row is the COMPRESSED set — group,
    // #, account, status, the three gauges, if (+ lifetime req/tok in wide).
    // The `auth` type, the per-window reset times, and the token expiry/refresh
    // cluster moved to the selected-account detail pane (`draw_detail`, on MAIN
    // beside the summary and full-width in the Accounts overlay) — relocated,
    // not dropped.
    //
    // The `Fbl` gauge (fable-usage U9a) is inserted AFTER the `7d` gauge, and
    // ONLY when `show_fable_weekly` is on — off renders the table with no
    // column, no width taken. Wide gets a full gauge column; narrow gets a
    // compact marker column (the width budget is tight there).
    let (header, constraints): (Vec<&'static str>, Vec<Constraint>) = if wide {
        let mut header = vec!["", "group", "#", "account", "status", "5h", "7d"];
        let mut constraints = vec![
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Length(2),
            Constraint::Length(name_width),
            Constraint::Length(20),
            Constraint::Length(gauge_cell),
            Constraint::Length(gauge_cell),
        ];
        if show_fable {
            header.push("7d Fbl");
            constraints.push(Constraint::Length(gauge_cell));
        }
        header.extend(["if", "req", "tok"]);
        constraints.extend([
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Length(7),
        ]);
        (header, constraints)
    } else {
        let mut header = vec!["", "group", "#", "account", "status", "5h", "7d"];
        let mut constraints = vec![
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Length(2),
            Constraint::Length(name_width),
            Constraint::Length(20),
            Constraint::Length(gauge_cell),
            Constraint::Length(gauge_cell),
        ];
        if show_fable {
            header.push("7d Fbl");
            // Compact marker column ("F 100%!" fits in 7): no bar, no stretch.
            constraints.push(Constraint::Length(7));
        }
        header.extend(["if"]);
        constraints.extend([Constraint::Length(3)]);
        (header, constraints)
    };

    let table = Table::new(rows, constraints)
        .header(Row::new(header).style(dim().add_modifier(Modifier::BOLD)))
        .block(block);
    frame.render_widget(table, area);
    // Row hit map (UI-3 U11): row `pos` renders at area.y + 2 + pos (top
    // border/title + header row), one line high, full table width. Rows
    // clipped by the pane height are not clickable.
    ctx.order
        .iter()
        .enumerate()
        .filter(|(pos, _)| area.y as usize + 2 + pos < area.bottom() as usize)
        .map(|(pos, _)| AccountRowHit {
            area: Rect {
                x: area.x,
                y: area.y + 2 + pos as u16,
                width: area.width,
                height: 1,
            },
            display_idx: pos,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn account_row<'a>(
    account: &'a AccountSnapshot,
    view: &'a DashboardView,
    ctx: &FrameCtx,
    pos: usize,
    wide: bool,
    cursor: bool,
    bar_width: usize,
) -> Row<'a> {
    let snapshot = &view.snapshot;
    let params = &view.select_params;
    let now = ctx.now;
    let is_current = snapshot.is_current(&account.id);
    let gate = select::eligibility(account, params, now, ctx.headers_only);

    // Urgency marker (glance-triage atom 2): tiers 0–1 (exhausted /
    // auth-broken) ALWAYS carry a `!` — a text signal that survives
    // no-truecolor panels and is never displaced by the cursor (`>`) or
    // current (`►`) glyph; the 2-wide cell holds both.
    let urgent = triage::urgent(account, gate);
    let marker = {
        let lead = match (cursor, is_current) {
            (true, _) => ">",
            (false, true) => "►",
            (false, false) => "",
        };
        // Fixed 2-column content in every state, so cursor/urgent
        // combinations never nudge the columns to their right.
        let text = match (lead, urgent) {
            ("", true) => "! ".to_string(),
            ("", false) => "  ".to_string(),
            (lead, true) => format!("{lead}!"),
            (lead, false) => format!("{lead} "),
        };
        let style = if urgent {
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else if cursor {
            Style::new().fg(Color::Cyan)
        } else if is_current {
            Style::new().fg(Color::Green)
        } else {
            Style::new()
        };
        Span::styled(text, style)
    };
    let name = if is_current {
        Span::styled(
            row_account_name(&account.id.0, ctx.mask, &view.domain_abbrev),
            Style::new().add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(row_account_name(
            &account.id.0,
            ctx.mask,
            &view.domain_abbrev,
        ))
    };
    let parked = matches!(gate, Some(IneligibleReason::CoolingDown));
    // Poller-health overlay (issue #33): a failing usage poll makes every
    // window's value suspect, so it surfaces as a distinct display state rather
    // than collapsing to a plain percent/—. Codex has no usage poller, so it
    // never reads as poll-degraded.
    let consecutive_failures = view
        .poll_health
        .get(&account.id.0)
        .map_or(0, |h| h.consecutive_failures);
    let max_age = params.usage_max_age;
    let five_gauge = window_gauge_cell(
        &account.five_hour,
        params.five_hour_max,
        parked,
        now,
        max_age,
        consecutive_failures,
        ctx.quota_display,
        ctx.reset_absolute,
        bar_width,
    );
    let seven_gauge = window_gauge_cell(
        &account.seven_day,
        params.seven_day_max,
        parked,
        now,
        max_age,
        consecutive_failures,
        ctx.quota_display,
        ctx.reset_absolute,
        bar_width,
    );
    let totals = view.totals_for(&account.id.0);

    let group_label = account.group.as_str();
    let mut cells = vec![
        Cell::from(marker),
        Cell::from(Span::styled(
            group_label.to_uppercase(),
            group_color(Some(group_label)).add_modifier(Modifier::BOLD),
        )),
        Cell::from(Span::styled(format!("{}", pos + 1), dim())),
        Cell::from(name),
    ];
    cells.push(Cell::from(status_span(
        account, gate, is_current, params, now, ctx.frame,
    )));
    cells.extend([five_gauge, seven_gauge]);
    // Fbl gauge (fable-usage U9a): rendered only when the toggle is on, in the
    // same slot (after 7d) as the header/constraints reserve above, so the
    // cells stay column-aligned. Absent-window → the same cold state 5h/7d use.
    if view.show_fable_weekly {
        cells.push(fable_gauge_cell(
            account.fable_weekly(),
            now,
            wide,
            max_age,
            consecutive_failures,
            ctx.quota_display,
            ctx.reset_absolute,
            select::effective_limits(account, params).2,
            bar_width,
        ));
    }
    cells.push(Cell::from(in_flight_span(account.in_flight)));
    if wide {
        cells.push(Cell::from(format::human_count(totals.requests)));
        cells.push(Cell::from(format::human_count(totals.tokens())));
    }
    Row::new(cells)
}

/// Status column: active (green) / ready (default) / the concrete blocking
/// reason from the scheduler's own gate ("cooldown 3m12s", "7d 99.4% > 99%",
/// "usage stale 14m", "auth failed") so the TUI never disagrees with the
/// selector about WHY an account is parked.
fn status_span(
    account: &AccountSnapshot,
    gate: Option<IneligibleReason>,
    is_current: bool,
    params: &select::SelectParams,
    now: SystemTime,
    frame: usize,
) -> Span<'static> {
    let Some(reason) = gate else {
        // Eligible. The current account is "active" — a braille working
        // spinner while it has in-flight traffic, otherwise a calm bar
        // heartbeat. Other eligible accounts get a faint "ready" drift.
        return if is_current {
            let glyph = if account.in_flight > 0 {
                anim::braille_spin(frame)
            } else {
                anim::bar_pulse(frame)
            };
            Span::styled(
                format!("{glyph} active"),
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!("{} ready", anim::idle_drift(frame)), dim())
        };
    };
    // Cooldowns render TIME-ONLY (`▌ 45m 34s`, yellow): the rotating-timer
    // glyph + yellow already say "waiting", so the word "cooldown" was pure
    // width (Z 2026-07-09). The `/llmux/status` API string is untouched —
    // `blocking_reason` still says "cooldown 45m34s" there.
    if reason == IneligibleReason::CoolingDown {
        let left = account
            .cooldown_until
            .and_then(|until| until.duration_since(now).ok())
            .map(format::countdown)
            .unwrap_or_default();
        return Span::styled(
            format!("{} {left}", anim::half_block_clock(frame)),
            Style::new().fg(Color::Yellow),
        );
    }
    let text = select::blocking_reason(account, reason, params, now);
    // Each blocked state gets its own animated glyph so the WHY reads at a
    // glance: blinking alert (auth), shade filling up (over quota), a rotating
    // timer (cooldown), a faint drift (stale data), a steady held block
    // (operator pause).
    let (glyph, style) = match reason {
        IneligibleReason::AuthUnhealthy => (
            anim::blink(frame, '!'),
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        IneligibleReason::Paused => ('⠿', Style::new().fg(Color::Yellow)),
        IneligibleReason::FiveHourOverThreshold | IneligibleReason::SevenDayOverThreshold => {
            (anim::shade_breathe(frame), Style::new().fg(Color::Red))
        }
        IneligibleReason::CoolingDown | IneligibleReason::FableCoolingDown => (
            anim::half_block_clock(frame),
            Style::new().fg(Color::Yellow),
        ),
        IneligibleReason::FableWeeklyExhausted => {
            (anim::shade_breathe(frame), Style::new().fg(Color::Red))
        }
        IneligibleReason::UsageStale => (anim::idle_drift(frame), dim()),
    };
    Span::styled(format!("{glyph} {text}"), style)
}

fn in_flight_span(in_flight: u32) -> Span<'static> {
    if in_flight == 0 {
        Span::styled("0", dim())
    } else {
        Span::styled(
            in_flight.to_string(),
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )
    }
}

/// Build one in-bar quota line: a `width`-column bar whose leading
/// `fill`-fraction of columns render REVERSE-video in `color` (the fill), with
/// `text` overlaid centered on top. The first `bold_chars` characters of
/// `text` (the larger countdown unit) are emboldened. Spans are split at the
/// fill boundary so the fill shows behind the text — a single-style span
/// could not paint a partial background. Only spaces + the caller's text are
/// emitted (no bar glyphs), so the CJK narrow-width invariant guarded in
/// `anim.rs` is untouched.
///
/// `marker` (issue #33 display-state glyph — ○ cold / ◑ stale / …) renders in
/// the FIXED last bar column, independent of the centered `text`, so the
/// time never shifts when the marker appears or disappears (Z 2026-07-15).
/// A text long enough to reach that column loses its last char to the
/// marker — the marker is the rarer, higher-signal fact.
fn quota_bar_line(
    fill: f64,
    color: Color,
    text: &str,
    bold_chars: usize,
    width: usize,
    marker: Option<char>,
) -> Line<'static> {
    let fill_cols = ((fill.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    let chars: Vec<char> = text.chars().collect();
    let text_len = chars.len().min(width);
    let start = (width - text_len) / 2;
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_style: Option<Style> = None;
    for col in 0..width {
        let ch = if marker.is_some() && col == width.saturating_sub(1) {
            marker.unwrap_or(' ')
        } else if col >= start && col < start + text_len {
            chars[col - start]
        } else {
            ' '
        };
        let mut style = Style::new().fg(color);
        if col < fill_cols {
            style = style.add_modifier(Modifier::REVERSED);
        }
        if col >= start && col < start + bold_chars.min(text_len) {
            style = style.add_modifier(Modifier::BOLD);
        }
        if run_style == Some(style) {
            run.push(ch);
        } else {
            if let Some(prev) = run_style {
                spans.push(Span::styled(std::mem::take(&mut run), prev));
            }
            run.push(ch);
            run_style = Some(style);
        }
    }
    if let Some(prev) = run_style {
        spans.push(Span::styled(run, prev));
    }
    Line::from(spans)
}

/// The text overlaid inside a quota bar: the top-2-unit reset countdown
/// (`7d 10h`), plus the window display-state glyph when the value is not
/// fresh (issue #33) — returned SEPARATELY so the renderer can pin it to a
/// fixed trailing column instead of appending it to the centered time (the
/// append shifted the time sideways whenever the glyph came and went — Z
/// 2026-07-15 "가운데 시간 표시 위치 고정"). The parked/over `!` is NOT here —
/// it rides the percent label to the right of the bar, where it always
/// lived. Returns `(text, bold_chars, marker)` where `bold_chars` covers the
/// FIRST countdown unit — the day/hour magnitude carries most of the signal,
/// so it gets the emphasis. An expired window (no live reset) reads `0s` —
/// honest and ASCII-narrow.
fn quota_bar_text(
    window: &QuotaWindow,
    now: SystemTime,
    display: WindowDisplayState,
    absolute: bool,
) -> (String, usize, Option<char>) {
    let live = window
        .resets_at
        .duration_since(now)
        .ok()
        .filter(|rem| !rem.is_zero());
    let (text, bold_chars) = match (absolute, live) {
        // Absolute stamp (`t` toggle): `MM/DD HH:MM` UTC, date part bold.
        (true, Some(_)) => (format::absolute_utc_label(window.resets_at), 5),
        (false, Some(rem)) => {
            let (head, tail) = format::countdown_units(rem);
            let bold = head.chars().count();
            let mut text = head;
            if let Some(tail) = tail {
                text.push(' ');
                text.push_str(&tail);
            }
            (text, bold)
        }
        // Expired window: no live reset to point at in either mode.
        (_, None) => ("0s".to_string(), 2),
    };
    let marker = (!matches!(display, WindowDisplayState::Populated)).then(|| display.glyph());
    (text, bold_chars, marker)
}

/// Assemble one full quota gauge cell line: the countdown bar
/// ([`quota_bar_line`], `bar_width` cols) + a space + the right-aligned
/// percent label (`QUOTA_LABEL_WIDTH` cols, `!`-marked when parked/over) —
/// `bar_width + 1 + QUOTA_LABEL_WIDTH` columns total (`QUOTA_CELL_WIDTH` is the
/// minimum, when the terminal has no leftover width to pour into the bar). The
/// label is the percent of the
/// FILL fraction, so number and bar always say the same thing: in the default
/// `remaining` mode a fresh account reads a full green bar + `100%` and
/// drains toward `0%`; in `used` mode it reads `0%` growing. `over` (parked /
/// past threshold) stays keyed on USED utilization either way, as do the
/// color bands.
#[allow(clippy::too_many_arguments)]
fn quota_cell_line(
    fill: f64,
    color: Color,
    bar_text: &str,
    bold_chars: usize,
    over: bool,
    bar_width: usize,
    marker: Option<char>,
) -> Line<'static> {
    let mut label = format::percent(fill);
    if over {
        label.push('!');
    }
    let mut line = quota_bar_line(fill, color, bar_text, bold_chars, bar_width, marker);
    line.spans.push(Span::raw(" "));
    line.spans.push(Span::styled(
        format!("{:>width$}", label, width = QUOTA_LABEL_WIDTH),
        Style::new().fg(color),
    ));
    line
}

/// One quota window → its gauge cell: the WHOLE cell is one bar whose
/// reverse-video fill is the utilization (fill direction per `quota_display`:
/// `used` grows as quota burns, `remaining` drains toward the reset), with the
/// reset countdown overlaid inside ([`quota_bar_text`]). This supersedes req4
/// ("the gauge label is ALWAYS the percentage"): that rule existed because the
/// countdown once REPLACED the percent label — now both facts render at once
/// (fill + color carry utilization, the in-bar text carries the reset), so
/// neither hides the other. Color bands stay keyed on USED utilization
/// regardless of the fill direction.
///
/// Issue #33: the cell still carries the [`WindowDisplayState`] so a
/// never-used (`cold`), stale, or poll-degraded window is visibly distinct
/// from an honest fresh value — render-only, derived from recorded state.
#[allow(clippy::too_many_arguments)]
fn window_gauge_cell(
    window: &Option<QuotaWindow>,
    threshold: f64,
    parked: bool,
    now: SystemTime,
    max_age: Duration,
    consecutive_failures: u32,
    mode: crate::config::QuotaDisplay,
    reset_absolute: bool,
    bar_width: usize,
) -> Cell<'static> {
    let display = classify_window_display(window, now, max_age, consecutive_failures);
    let Some(window) = window else {
        // Cold (or poll-degraded with no window yet): show the state, not a bare
        // — that reads the same as "0% used".
        return Cell::from(Span::styled(
            format!("{} {}", display.glyph(), display.label()),
            dim(),
        ));
    };
    let utilization = window.effective_utilization(now);
    let color = level_color(format::gauge_level(utilization));
    // The `!` flags an account that is parked or past its threshold — carried
    // on the percent label, where it always lived.
    let over = parked || utilization > threshold;
    let fill = match mode {
        crate::config::QuotaDisplay::Used => utilization,
        crate::config::QuotaDisplay::Remaining => 1.0 - utilization,
    };
    let (text, bold_chars, marker) = quota_bar_text(window, now, display, reset_absolute);
    Cell::from(quota_cell_line(
        fill, color, &text, bold_chars, over, bar_width, marker,
    ))
}

/// The model-scoped "Fable" weekly gauge cell (fable-usage U9a, W0 Q3),
/// following the same pattern as [`window_cells`] but as a single cell (no
/// paired reset column — W0 keeps the Fbl slot light) and with scope-aware
/// critical coloring:
///
/// - Present window: the same in-bar countdown gauge as 5h/7d
///   ([`quota_bar_line`]) in wide mode, a compact `F 7d!` countdown marker in
///   narrow mode. Colored by fill level through the SAME [`format::gauge_level`]
///   / [`level_color`] palette as 5h/7d, EXCEPT the scope's own signal wins —
///   a *constraining* Fable limit reads red regardless of the raw percent (the
///   limit is engaged upstream even if the number looks calm). "Constraining"
///   is [`ScopedQuotaWindow::is_constraining`], so the red is **reset-aware**:
///   an expired / just-reset window is NOT red even when its `severity` field
///   is still a stale `Critical`, because `is_constraining` short-circuits on
///   `is_expired` before it ever inspects `severity`. (Previously the cell
///   keyed on the raw `severity == Critical`, which is not reset-aware, so a
///   post-reset window flashed red `F 0%!` until the next usage poll.)
///   `is_active` alone does NOT force red: it marks the representative/governing
///   limit, NOT an exhausted one, so a 76%/warning/is_active row keeps its
///   normal utilization-based hue (also via `is_constraining`).
///   A trailing `!` flags the red state, mirroring the over-threshold marker on
///   the account windows.
/// - Absent window (no Fable scope on this account): the same cold/stale/
///   poll-degraded state 5h/7d show for an absent window, via
///   [`classify_window_display`] — never a crash or blank.
#[allow(clippy::too_many_arguments)]
fn fable_gauge_cell(
    scoped: Option<&ScopedQuotaWindow>,
    now: SystemTime,
    wide: bool,
    max_age: Duration,
    consecutive_failures: u32,
    mode: crate::config::QuotaDisplay,
    reset_absolute: bool,
    fable_max: f64,
    bar_width: usize,
) -> Cell<'static> {
    let window = scoped.map(|s| s.window);
    let display = classify_window_display(&window, now, max_age, consecutive_failures);
    let Some(scoped) = scoped else {
        // Cold / absent: mirror the `window_cells` absent branch — the glyph +
        // label in wide mode, a compact `F ○`-style marker in narrow mode — so
        // a never-seen Fable window reads distinctly from an honest 0%.
        return if wide {
            Cell::from(Span::styled(
                format!("{} {}", display.glyph(), display.label()),
                dim(),
            ))
        } else {
            Cell::from(Span::styled(format!("F {}", display.glyph()), dim()))
        };
    };
    let utilization = scoped.window.effective_utilization(now);
    // Scope signal wins: a *constraining* Fable limit is red no matter the
    // percent — but `is_constraining` is reset-aware (it short-circuits on an
    // expired/reset window), so a stale-`Critical` severity no longer paints a
    // just-reset 0% window red. `is_active` is NOT a red trigger either — it
    // marks the representative limit, not an exhausted one. Otherwise fall to
    // the fill band.
    let critical = scoped.is_constraining(now, fable_max);
    let level = if critical {
        GaugeLevel::Red
    } else {
        format::gauge_level(utilization)
    };
    let color = level_color(level);
    // `!` on the red-critical read, same signal window_cells carries with its
    // over-threshold `!`.
    let over = matches!(level, GaugeLevel::Red);
    if wide {
        // Same in-bar countdown gauge as the 5h/7d cells; the critical
        // override only changes the color/`!`, never the fill math.
        let fill = match mode {
            crate::config::QuotaDisplay::Used => utilization,
            crate::config::QuotaDisplay::Remaining => 1.0 - utilization,
        };
        let (text, bold_chars, marker) =
            quota_bar_text(&scoped.window, now, display, reset_absolute);
        Cell::from(quota_cell_line(
            fill, color, &text, bold_chars, over, bar_width, marker,
        ))
    } else {
        // Compact narrow marker: `F` + the top countdown unit + critical `!`
        // (`F 7d!`), colored. No bar — the narrow width budget has no room for
        // one. An expired window (no live reset) falls back to the mode-flipped
        // percent so the marker never reads as a live countdown to a past
        // reset (just-reset in `remaining` mode = `F 100%`, full quota back).
        let label = match scoped.window.resets_at.duration_since(now) {
            Ok(rem) if !rem.is_zero() => format::countdown_units(rem).0,
            _ => match mode {
                crate::config::QuotaDisplay::Used => format::percent(utilization),
                crate::config::QuotaDisplay::Remaining => format::percent(1.0 - utilization),
            },
        };
        let text = if over {
            format!("F {label}!")
        } else {
            format!("F {label}")
        };
        Cell::from(Span::styled(text, Style::new().fg(color)))
    }
}

/// Reset text for the detail pane: compact countdown plus the absolute local
/// time — "1h02m (14:30)", "2d4h (06-15 09:00)". (Issue #70: this used to
/// feed the per-row reset columns too; those moved into the detail pane.)
fn reset_label(window: &QuotaWindow, now: SystemTime, tz_offset: i64) -> Option<String> {
    let remaining = window.resets_at.duration_since(now).ok()?;
    if remaining.is_zero() {
        return None;
    }
    Some(format!(
        "{} ({})",
        select::compact_duration(remaining),
        format::absolute_label(window.resets_at, now, tz_offset)
    ))
}

/// Middle row: scheduler/poller/totals summary, with the selected-account
/// detail pane beside it when there is room. The old `d` toggle is gone (issue
/// #5): on MAIN the detail rides alongside the summary whenever the width
/// allows, and the Accounts overlay (`a`) gives detail the full-width slot.
fn draw_middle(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    chrome: &Chrome,
) {
    let has_accounts = !view.snapshot.accounts.is_empty();
    if has_accounts && area.width >= SIDE_BY_SIDE_AT {
        let [summary_area, detail_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(48)]).areas(area);
        draw_summary(frame, summary_area, view, ctx);
        draw_detail(frame, detail_area, view, ctx, chrome);
    } else {
        // Too narrow for both (or no accounts): MAIN shows the summary; the
        // full detail is one keystroke away in the Accounts overlay.
        draw_summary(frame, area, view, ctx);
    }
}

/// Scheduler / poller / totals summary pane.
fn draw_summary(frame: &mut Frame, area: Rect, view: &DashboardView, ctx: &FrameCtx) {
    let snapshot = &view.snapshot;
    let now = ctx.now;
    let label = |text: &'static str| Span::styled(format!(" {text:<9}"), dim());
    let mut lines: Vec<Line> = Vec::with_capacity(6);

    // Per-group current subscription (req1): claude and codex pick their
    // current account INDEPENDENTLY, so show one line per group present.
    let groups_present: Vec<BackendGroup> = [BackendGroup::Claude, BackendGroup::Codex]
        .into_iter()
        .filter(|g| snapshot.accounts.iter().any(|a| a.group == *g))
        .collect();
    if groups_present.is_empty() {
        lines.push(Line::from(vec![
            label("current"),
            Span::styled("(none)", Style::new().fg(Color::Red)),
        ]));
    }
    for (i, g) in groups_present.iter().enumerate() {
        let mut spans = vec![
            label(if i == 0 { "current" } else { "" }),
            Span::styled(
                format!("{:<7}", g.as_str()),
                group_color(Some(g.as_str())).add_modifier(Modifier::BOLD),
            ),
        ];
        match snapshot.current_for_group(*g) {
            Some(current) => {
                spans.push(Span::styled(
                    masked_name(&current.0, ctx.mask),
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                ));
                if let Some(switch) = view.last_switch.as_ref().filter(|s| s.to == current.0) {
                    let ago = now
                        .duration_since(switch.at)
                        .map(select::compact_duration)
                        .unwrap_or_else(|_| "0s".into());
                    let why = switch.reason.as_deref().unwrap_or("switch");
                    let from = switch
                        .from
                        .as_deref()
                        .map(|f| format!("{} → ", masked_name(f, ctx.mask)))
                        .unwrap_or_default();
                    spans.push(Span::styled(format!("  {from}{why}, {ago} ago"), dim()));
                }
            }
            None => spans.push(Span::styled("(none)", Style::new().fg(Color::Red))),
        }
        lines.push(Line::from(spans));
    }

    // Per-group next-in-line (req1 symmetry with the current block) + the
    // shared eval-tick countdown. One tick re-evaluates every group, so the
    // "eval in ~Xs" is shown once, on the first row.
    let tick = view.evaluate_tick.as_secs().max(1);
    let to_next_eval = tick - (view.uptime.as_secs() % tick);
    if groups_present.is_empty() {
        lines.push(Line::from(vec![label("next"), Span::raw("—")]));
    }
    for (i, g) in groups_present.iter().enumerate() {
        let next = select::next_in_line(snapshot, &view.select_params, now, Some(*g));
        let mut spans = vec![
            label(if i == 0 { "next" } else { "" }),
            Span::styled(
                format!("{:<7}", g.as_str()),
                group_color(Some(g.as_str())).add_modifier(Modifier::BOLD),
            ),
            Span::raw(
                next.map(|n| masked_name(&n.0, ctx.mask))
                    .unwrap_or_else(|| "—".into()),
            ),
        ];
        if i == 0 {
            spans.push(Span::styled(
                format!(
                    "  eval in ~{}",
                    select::compact_duration(Duration::from_secs(to_next_eval))
                ),
                dim(),
            ));
        }
        lines.push(Line::from(spans));
    }

    // Scheduler mode line (S toggles): round-robin reads emphasized — it is
    // the "you asked for minimal switching" state worth noticing at a glance.
    let mode = view.select_params.mode;
    lines.push(Line::from(vec![
        label("mode"),
        Span::styled(
            mode.label().to_string(),
            if mode == crate::config::SchedulerMode::RoundRobin {
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            },
        ),
        Span::styled("   [S switch]", dim()),
    ]));

    lines.push(Line::from(vec![
        label("poller"),
        Span::raw(poller_summary(view, now)),
    ]));

    let totals = view.global_totals;
    lines.push(Line::from(vec![
        label("totals"),
        Span::raw(format!("{} req · ", format::human_count(totals.requests))),
        Span::styled(
            format!("{} ok", format::human_count(totals.ok)),
            Style::new().fg(Color::Green),
        ),
        Span::raw(" / "),
        Span::styled(
            format!("{} err", format::human_count(totals.errors)),
            if totals.errors > 0 {
                Style::new().fg(Color::Red)
            } else {
                dim()
            },
        ),
        Span::raw(format!(
            " · in {} / out {} tok",
            format::human_count(totals.tokens_in),
            format::human_count(totals.tokens_out)
        )),
    ]));

    let in_flight: u32 = snapshot.accounts.iter().map(|a| a.in_flight).sum();
    lines.push(Line::from(vec![
        label("load"),
        Span::raw(format!(
            "{:.1} req/min (5m) · {in_flight} in flight",
            view.rpm_5m
        )),
    ]));

    // Codex group settings (req8.1): model / fast tier / reasoning effort, with
    // the keys that change them. Only when a codex account exists.
    if view.codex.available {
        let c = &view.codex;
        let fast_style = if c.fast {
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            dim()
        };
        lines.push(Line::from(vec![
            label("codex"),
            Span::styled(
                c.model.clone(),
                group_color(Some("codex")).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" · fast "),
            Span::styled(if c.fast { "on" } else { "off" }, fast_style),
            Span::raw(" · effort "),
            Span::raw(c.effort.clone().unwrap_or_else(|| "default".into())),
            Span::styled("   [f fast · m model · e effort]", dim()),
        ]));
    }

    let block = Block::new().borders(Borders::TOP).title(" scheduler ");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// One-line usage-poller health: oauth count, last-success age spread,
/// backing-off accounts, soonest next poll.
fn poller_summary(view: &DashboardView, now: SystemTime) -> String {
    let oauth: Vec<&AccountSnapshot> = view
        .snapshot
        .accounts
        .iter()
        .filter(|a| a.credential_kind == "oauth")
        .collect();
    if oauth.is_empty() {
        return "no oauth accounts (header-driven only)".into();
    }
    let mut ok_ages: Vec<Duration> = Vec::new();
    let mut next_in: Option<Duration> = None;
    let mut backoff: Vec<String> = Vec::new();
    for account in &oauth {
        let Some(health) = view.poll_health(&account.id.0) else {
            continue;
        };
        if let Some(age) = health.last_ok.and_then(|at| now.duration_since(at).ok()) {
            ok_ages.push(age);
        }
        if let Ok(eta) = health.next_at.duration_since(now) {
            next_in = Some(next_in.map_or(eta, |cur| cur.min(eta)));
        }
        if health.consecutive_failures > 0 {
            backoff.push(format!(
                "{}×{}",
                masked_name(&account.id.0, view.email_anonymous),
                health.consecutive_failures
            ));
        }
    }
    if ok_ages.is_empty() && backoff.is_empty() {
        return format!("{} oauth · warming up", oauth.len());
    }
    let mut out = format!("{} oauth", oauth.len());
    if let (Some(min), Some(max)) = (ok_ages.iter().min(), ok_ages.iter().max()) {
        if min == max {
            out.push_str(&format!(
                " · last ok {} ago",
                select::compact_duration(*min)
            ));
        } else {
            out.push_str(&format!(
                " · last ok {}–{} ago",
                select::compact_duration(*min),
                select::compact_duration(*max)
            ));
        }
    } else {
        out.push_str(" · no successful poll yet");
    }
    if let Some(eta) = next_in {
        out.push_str(&format!(" · next ~{}", select::compact_duration(eta)));
    }
    if !backoff.is_empty() {
        out.push_str(&format!(" · backoff {}", backoff.join(" ")));
    }
    out
}

/// Selected-account detail pane: the cursor row in select mode, otherwise
/// the current account, otherwise the head of the order.
fn draw_detail(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    chrome: &Chrome,
) {
    let snapshot = &view.snapshot;
    let pos = match chrome.mode {
        Mode::Select { idx }
        | Mode::ConfirmRemove { idx }
        | Mode::EditLimits { idx }
        | Mode::ContextMenu { idx, .. } => idx.min(ctx.order.len().saturating_sub(1)),
        // NewLogin keeps the detail pane on the current account.
        Mode::Normal
        | Mode::AddKey
        | Mode::NewLogin { .. }
        | Mode::ConfigEdit { .. }
        | Mode::ConfigConfirm { .. } => snapshot
            .representative_current()
            .and_then(|cur| {
                ctx.order
                    .iter()
                    .position(|&i| &snapshot.accounts[i].id == cur)
            })
            .unwrap_or(0),
    };
    let Some(account) = ctx.order.get(pos).map(|&i| &snapshot.accounts[i]) else {
        return;
    };
    let params = &view.select_params;
    let now = ctx.now;
    let gate = select::eligibility(account, params, now, ctx.headers_only);
    let is_current = snapshot.is_current(&account.id);

    let mut lines: Vec<Line> = Vec::with_capacity(7);
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {}", masked_name(&account.id.0, ctx.mask)),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {}", account.credential_kind), dim()),
    ]));
    lines.push(Line::from(vec![
        Span::styled(format!(" order #{}", pos + 1), Style::new()),
        Span::raw(" · "),
        status_span(account, gate, is_current, params, now, ctx.frame),
    ]));
    let mut token_line = vec![Span::styled(" token ", dim())];
    token_line.extend(token_detail_spans(
        account,
        view.refresh_ahead,
        now,
        ctx.tz_offset,
    ));
    lines.push(Line::from(token_line));
    let consecutive_failures = view
        .poll_health
        .get(&account.id.0)
        .map_or(0, |h| h.consecutive_failures);
    let max_age = params.usage_max_age;
    lines.push(window_detail_line(
        "5h",
        &account.five_hour,
        ctx,
        max_age,
        consecutive_failures,
    ));
    lines.push(window_detail_line(
        "7d",
        &account.seven_day,
        ctx,
        max_age,
        consecutive_failures,
    ));
    let totals = view.totals_for(&account.id.0);
    lines.push(Line::from(vec![
        Span::styled(" life  ", dim()),
        Span::raw(format!(
            "{} req ({} ok/{} err) · in {}/out {}",
            format::human_count(totals.requests),
            format::human_count(totals.ok),
            format::human_count(totals.errors),
            format::human_count(totals.tokens_in),
            format::human_count(totals.tokens_out),
        )),
    ]));
    let poll = match view.poll_health(&account.id.0) {
        Some(health) => {
            let last = health
                .last_ok
                .and_then(|at| now.duration_since(at).ok())
                .map(|age| format!("ok {} ago", select::compact_duration(age)))
                .unwrap_or_else(|| "no success yet".into());
            let next = health
                .next_at
                .duration_since(now)
                .map(|eta| format!(" · next ~{}", select::compact_duration(eta)))
                .unwrap_or_default();
            let backoff = if health.consecutive_failures > 0 {
                format!(" · backoff ×{}", health.consecutive_failures)
            } else {
                String::new()
            };
            format!("{last}{next}{backoff}")
        }
        None if account.credential_kind == "oauth" => "not polled yet".into(),
        // apikey/codex accounts have no Anthropic usage endpoint to poll.
        None => format!("n/a ({})", account.credential_kind),
    };
    lines.push(Line::from(vec![
        Span::styled(" poll  ", dim()),
        Span::raw(poll),
    ]));

    let block = Block::new().borders(Borders::TOP).title(" detail ");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Token line for the detail pane: countdown + absolute local expiry,
/// when the token was last refreshed, and when the background refresher
/// will act — "expires 6h52m (08:21) · refreshed 3m ago (01:26) · refresh
/// due in 52m". Already-due and expired states keep their warning colors.
fn token_detail_spans(
    account: &AccountSnapshot,
    refresh_ahead: Duration,
    now: SystemTime,
    tz_offset: i64,
) -> Vec<Span<'static>> {
    let Some(expires_ms) = account.token_expires_at_ms else {
        return vec![Span::styled("— (apikey)", dim())];
    };
    let expires_at = UNIX_EPOCH + Duration::from_millis(expires_ms);
    let mut spans = match expires_at.duration_since(now) {
        Ok(left) => {
            let absolute = format::absolute_label(expires_at, now, tz_offset);
            let head = format!("expires {} ({absolute})", select::compact_duration(left));
            if left > refresh_ahead {
                vec![Span::raw(head)]
            } else {
                vec![Span::styled(
                    format!("{head} · refresh due"),
                    Style::new().fg(Color::Yellow),
                )]
            }
        }
        Err(_) => vec![Span::styled(
            "expired — refresh overdue".to_string(),
            Style::new().fg(Color::Red),
        )],
    };
    let refreshed = match account.last_refresh_ms {
        Some(ms) => {
            let at = UNIX_EPOCH + Duration::from_millis(ms);
            let ago = now.duration_since(at).unwrap_or_default();
            format!(
                " · refreshed {} ago ({})",
                select::compact_duration(ago),
                format::absolute_label(at, now, tz_offset),
            )
        }
        None => " · refreshed never".to_string(),
    };
    spans.push(Span::styled(refreshed, dim()));
    if let Ok(left) = expires_at.duration_since(now) {
        if left > refresh_ahead {
            spans.push(Span::raw(format!(
                " · refresh due in {}",
                select::compact_duration(left - refresh_ahead)
            )));
        }
    }
    spans
}

/// Detail line for one window: utilization, reset (countdown + absolute),
/// observation source + age.
fn window_detail_line(
    name: &'static str,
    window: &Option<QuotaWindow>,
    ctx: &FrameCtx,
    max_age: Duration,
    consecutive_failures: u32,
) -> Line<'static> {
    let label = Span::styled(format!(" {name:<5} "), dim());
    let now = ctx.now;
    // Issue #33: surface the distinct display state in the detail pane too.
    let display = classify_window_display(window, now, max_age, consecutive_failures);
    let Some(window) = window else {
        return Line::from(vec![
            label,
            Span::styled(format!("no data ({})", display.label()), dim()),
        ]);
    };
    let utilization = window.effective_utilization(now);
    let color = level_color(format::gauge_level(utilization));
    let reset = reset_label(window, now, ctx.tz_offset).unwrap_or_else(|| "expired".into());
    let source = match window.source {
        crate::scheduler::window::WindowSource::Headers => "headers",
        crate::scheduler::window::WindowSource::UsagePoll => "poll",
    };
    let age = now
        .duration_since(window.fetched_at)
        .map(select::compact_duration)
        .unwrap_or_else(|_| "0s".into());
    let mut spans = vec![
        label,
        Span::styled(format::percent(utilization), Style::new().fg(color)),
        Span::raw(format!(" · resets {reset}")),
        Span::styled(format!(" · {source} {age} ago"), dim()),
    ];
    if !matches!(display, WindowDisplayState::Populated) {
        spans.push(Span::styled(format!(" · {}", display.label()), dim()));
    }
    Line::from(spans)
}

/// One click-target inside the activity panel: a completed *request* entry, its
/// stable [`ActivityKey`], and the absolute screen rows it occupies this frame
/// (`y_start..y_start+height`). Recorded during [`draw_activity`] so the mouse
/// handler can map a click to the entry without re-deriving the layout.
/// The tab strip (UI-3 U6): label + which surface it opens. `Overlay::None`
/// is the dashboard (MAIN) itself.
pub(crate) const TABS: &[(&str, Overlay)] = &[
    ("dashboard", Overlay::None),
    ("accounts", Overlay::Accounts),
    ("stats", Overlay::Stats),
    ("usage", Overlay::Usage),
    ("perf", Overlay::Perf),
    ("logs", Overlay::Logs),
    ("sessions", Overlay::Sessions),
    ("misc", Overlay::Misc),
    ("config", Overlay::Config),
];

/// The sessions table's clickable data area for one frame: `rows` covers the
/// data rows only (border + header excluded); a click at `rows.y + k` selects
/// session `start + k`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionsChrome {
    pub rows: Rect,
    pub start: usize,
}

/// One clickable tab label's rendered rect (UI-3 U6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TabHit {
    pub area: Rect,
    pub overlay: Overlay,
}

/// Pure hit-test: which tab, if any, does the click at absolute `(col, row)`
/// land on?
pub(crate) fn hit_test_tabs(tabs: &[TabHit], col: u16, row: u16) -> Option<Overlay> {
    tabs.iter()
        .find(|t| {
            row >= t.area.y && row < t.area.bottom() && col >= t.area.x && col < t.area.right()
        })
        .map(|t| t.overlay)
}

/// Minimum drag-set pane height (top border + one content row) and a sanity
/// ceiling (UI-3 U7/U8) — the layout solver squeezes overflow anyway, this
/// just keeps the override numbers honest.
pub(crate) const PANE_MIN_HEIGHT: u16 = 3;
pub(crate) const PANE_MAX_HEIGHT: u16 = 40;

/// Which MAIN pane a drag-separator resizes (UI-3 U7/U8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneId {
    Accounts,
    Middle,
    Strip,
}

/// One draggable separator: the top-border row `y` of the pane BELOW, whose
/// drag resizes the pane starting at `pane_top` (UI-3 U7/U8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeparatorHit {
    pub y: u16,
    pub pane: PaneId,
    pub pane_top: u16,
}

/// One accounts-table row's rendered rect + its DISPLAY index (selection
/// order — the same index the switch/pause/limits/remove flows take).
/// Right-click target map (UI-3 U11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountRowHit {
    pub area: Rect,
    pub display_idx: usize,
}

/// The rendered context menu's layout (UI-3 U11): the whole popup rect plus
/// one row rect per item, for the click hit-test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MenuChrome {
    pub area: Rect,
    pub items: Vec<Rect>,
}

impl MenuChrome {
    /// Which menu item a click lands on, if any.
    pub(crate) fn hit_item(&self, col: u16, row: u16) -> Option<usize> {
        self.items
            .iter()
            .position(|r| row == r.y && col >= r.x && col < r.right())
    }
}

/// One rotatable setting on the group-settings bar (UI-3 U9/U10): a click on
/// its segment cycles the setting to its next value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingAction {
    SchedMode,
    CodexModel,
    CodexEffort,
    CodexFast,
    GrokEffort,
}

/// One clickable segment of the group-settings bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SettingHit {
    pub area: Rect,
    pub action: SettingAction,
}

/// Everything MAIN rendered this frame that the mouse can hit (UI-3 U5): the
/// activity panel's rows plus the tab bar, the drag separators, the accounts
/// rows (right-click), the group-settings bar, and the open context menu.
/// Threaded back to the runtime the same way `ActivityChrome` alone used to
/// be.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MainChrome {
    pub activity: ActivityChrome,
    pub tabs: Vec<TabHit>,
    pub separators: Vec<SeparatorHit>,
    pub account_rows: Vec<AccountRowHit>,
    pub menu: Option<MenuChrome>,
    pub settings: Vec<SettingHit>,
    /// Sessions overlay table layout this frame (issue: sessions mouse
    /// select): the data-row rect + the index of its first visible session.
    pub sessions_table: Option<SessionsChrome>,
    /// Config overlay clickable value cells this frame (config-editor v1):
    /// row index + action, so a click activates exactly like Enter.
    pub config_rows: Vec<ConfigHit>,
    /// Set by `draw` after rendering the input modal (UI-6 item 3): `Some(max)`
    /// is the largest valid scroll offset (wrapped line count minus the modal's
    /// visible inner height) so the runtime can clamp its stored offset; `None`
    /// means no modal was open OR its entry aged out of the ring (→ close it).
    pub input_modal_max_scroll: Option<u16>,
    /// Draw feedback for the raw request/response viewer (UI-7/UI-8): scroll
    /// clamps plus the clickable tab/button rects this frame rendered. Unlike
    /// the input modal, `None` only means "no raw modal drawn this frame" —
    /// the raw modal owns its content and never closes on entry aging.
    pub raw_modal: Option<RawModalChrome>,
}

/// Per-frame raw-viewer chrome (UI-8): what the runtime needs to clamp scroll
/// offsets and to route clicks — the tab bar and the top-right action buttons
/// are mouse targets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RawModalChrome {
    /// Largest valid (vertical, horizontal) scroll for the ACTIVE tab.
    pub max_scroll: (u16, u16),
    /// One rect per rendered tab label, with the tab index it selects.
    pub tabs: Vec<(usize, Rect)>,
    /// One rect per rendered action button.
    pub buttons: Vec<(RawButton, Rect)>,
}

/// The raw viewer's top-right action buttons (UI-8):
/// `copy | copy as curl | copy all | save | save all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawButton {
    Copy,
    CopyCurl,
    CopyAll,
    Save,
    SaveAll,
}

impl RawButton {
    pub(crate) fn label(self) -> &'static str {
        match self {
            RawButton::Copy => "copy",
            RawButton::CopyCurl => "copy as curl",
            RawButton::CopyAll => "copy all",
            RawButton::Save => "save",
            RawButton::SaveAll => "save all",
        }
    }
}

/// What kind of row a hit rect belongs to, deciding what a click does
/// (UI-5): plain entries (singles AND expanded run members) toggle their
/// detail lines; a folded-run HEADER splits by column — the leading marker
/// toggles the fold, the body only ever expands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityHitKind {
    Entry,
    RunHeader {
        expanded: bool,
    },
    /// The `🔍 input` detail line of an expanded entry (UI-6 item 3). Its own
    /// one-row hit, layered ABOVE the entry's block hit, so clicking exactly
    /// this line opens the full-text modal instead of collapsing the entry.
    InputLine,
    /// The `🔍 request` detail line of an expanded entry (UI-7): clicking it
    /// opens the raw request/response viewer (CDT-style tabs) instead of
    /// collapsing the entry. Same layering as `InputLine`. Carries the
    /// entry's activity `id` so the opener selects THIS row, not the first
    /// entry sharing its (id-less) [`ActivityKey`] — two requests completing
    /// in the same millisecond with the same method/path/status collide on
    /// the key alone.
    RawLine {
        id: u64,
    },
}

/// The resolved meaning of one activity click (UI-5), returned by
/// [`hit_test_activity`] so the mouse handler stays a dumb dispatcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActivityClick {
    /// Toggle this entry's detail lines (single rows and run member rows).
    Entry(ActivityKey),
    /// Toggle the folded run open/closed (click on the `▸`/`▾` marker).
    RunToggle(ActivityKey),
    /// Click on a run header's body: expand when collapsed; while expanded it
    /// does NOT collapse (Z 2026-07-15 — only the marker closes a group).
    RunExpand(ActivityKey),
    /// Click on an entry's `🔍 input` detail line (UI-6 item 3): open the
    /// full-text modal for that entry.
    OpenInput(ActivityKey),
    /// Click on an entry's `🔍 request` detail line (UI-7): open the raw
    /// request/response viewer for that entry. The `id` pins the exact row
    /// (the key alone is ambiguous under a same-ms collision).
    OpenRaw(ActivityKey, u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityHit {
    pub key: ActivityKey,
    pub y_start: u16,
    pub height: u16,
    pub kind: ActivityHitKind,
}

/// The activity panel's rendered layout for one frame: the panel rect plus the
/// ordered hit-targets (request rows only — notes/in-flight are not clickable).
/// Threaded back to the runtime so a left-click can be mapped to an entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ActivityChrome {
    pub area: Rect,
    pub hits: Vec<ActivityHit>,
}

/// Width of the leading `▸`/`▾` marker zone on a folded-run header — clicks in
/// these leftmost panel columns toggle the fold; clicks past them are body
/// clicks (expand-only). Covers `" ▸ "` plus one slack cell.
const RUN_MARKER_ZONE: u16 = 4;

/// Pure hit-test (unit-tested): what does the click at absolute `(col, row)`
/// mean? `None` when the click is outside the panel, on the title border, or
/// on a non-request line. Used by the mouse handler.
pub(crate) fn hit_test_activity(
    chrome: &ActivityChrome,
    col: u16,
    row: u16,
) -> Option<ActivityClick> {
    let area = chrome.area;
    // Outside the panel rect → not ours.
    if col < area.x || col >= area.right() || row < area.y || row >= area.bottom() {
        return None;
    }
    let hit = chrome
        .hits
        .iter()
        .find(|hit| row >= hit.y_start && row < hit.y_start.saturating_add(hit.height))?;
    Some(match hit.kind {
        ActivityHitKind::InputLine => ActivityClick::OpenInput(hit.key.clone()),
        ActivityHitKind::RawLine { id } => ActivityClick::OpenRaw(hit.key.clone(), id),
        ActivityHitKind::Entry => ActivityClick::Entry(hit.key.clone()),
        ActivityHitKind::RunHeader { .. } if col < area.x.saturating_add(RUN_MARKER_ZONE) => {
            ActivityClick::RunToggle(hit.key.clone())
        }
        ActivityHitKind::RunHeader { .. } => ActivityClick::RunExpand(hit.key.clone()),
    })
}

/// Per-frame column widths for the activity rows (Z 2026-07-15 "최대 넓이"):
/// each column is padded to the WIDEST value visible this frame, so every row
/// lines up, and the input excerpt takes whatever is left of the terminal.
/// e2e observed output throughput for one finished request: `output /
/// total_duration` — the always-available metric (perf telemetry v1). `None`
/// when there is no output or no measurable duration (never fabricate a
/// rate). NOT a model decode-speed claim; the expanded detail carries the
/// estimated post-delta figure when the stream recorded one.
fn e2e_tps(tokens: Option<&TokenCounts>, duration: Duration) -> Option<f64> {
    let output = tokens.map(|t| t.output).unwrap_or(0);
    let secs = duration.as_secs_f64();
    if output == 0 || secs <= 0.0 {
        return None;
    }
    Some(output as f64 / secs)
}

/// Collapsed-row cell for the throughput column: `45t/s` / `7.2t/s`, `—`
/// when not a throughput sample.
fn row_tps_label(tokens: Option<&TokenCounts>, duration: Duration) -> String {
    match e2e_tps(tokens, duration) {
        Some(tps) if tps >= 10.0 => format!("{tps:.0}t/s"),
        Some(tps) => format!("{tps:.1}t/s"),
        None => "—".to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowMetrics {
    /// Panel width in cells — the excerpt budget is derived from it.
    width: u16,
    /// `[model effort]` badge slot.
    meta_w: usize,
    /// Duration column (`3.1s`).
    dur_w: usize,
    /// Token column (`269tok`).
    tok_w: usize,
    /// Output-throughput column (`45t/s` — e2e observed output tokens/sec).
    tps_w: usize,
    /// Cost column (`$0.0079`).
    cost_w: usize,
}

/// Hard cap on the meta badge slot — group/model/effort ride in from request
/// bodies, so a hostile body must not push the whole table off-screen.
const META_W_MAX: usize = 32;

impl RowMetrics {
    /// Measure the visible rows. `completed` is the already-windowed slice the
    /// frame will render (plus slack); in-flight rows share the meta slot.
    fn measure(width: u16, in_flight: &[InFlight], completed: &[&Completed]) -> Self {
        let mut m = RowMetrics {
            width,
            meta_w: 0,
            dur_w: 4,
            tok_w: 6,
            tps_w: 5,
            cost_w: 7,
        };
        for request in in_flight {
            let meta = activity_meta_body(
                request.group.as_deref(),
                request.model.as_deref(),
                request.effort.as_deref(),
            );
            m.meta_w = m.meta_w.max(cell_width(&meta));
        }
        for entry in completed {
            let CompletedBody::Request {
                duration,
                tokens,
                group,
                model,
                effort,
                ..
            } = &entry.body
            else {
                continue;
            };
            let meta = activity_meta_body(group.as_deref(), model.as_deref(), effort.as_deref());
            m.meta_w = m.meta_w.max(cell_width(&meta));
            m.dur_w = m.dur_w.max(format::elapsed_secs(*duration).len());
            if let Some(tokens) = tokens {
                m.tok_w = m
                    .tok_w
                    .max(format!("{}tok", format::human_count(tokens.total())).len());
                m.tps_w = m.tps_w.max(row_tps_label(Some(tokens), *duration).len());
                if let (Some(group), Some(model)) = (group, model) {
                    let cost = crate::pricing::cost_usd(
                        group,
                        model,
                        tokens,
                        &std::collections::HashMap::new(),
                    );
                    m.cost_w = m.cost_w.max(format_cost(cost).len());
                }
            }
        }
        m.meta_w = m.meta_w.min(META_W_MAX);
        m
    }
}

fn draw_activity(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    chrome: &Chrome,
    now: SystemTime,
) -> ActivityChrome {
    let in_flight = &view.in_flight;
    let capacity = area.height.saturating_sub(1) as usize; // top border

    let anim_frame = chrome.frame;
    let mut lines: Vec<Line> = Vec::with_capacity(capacity);
    // Fold BEFORE measuring so the metrics cover exactly the rows this frame
    // can show (plus expansion slack).
    let rows = triage::collapse_completed(&view.completed);
    let total = rows.len();
    let scroll = chrome.activity_scroll.min(total.saturating_sub(1));
    // Rows that can actually paint cells this frame (review MUST-FIX 4): a
    // COLLAPSED run contributes only its newest member — every member shares
    // the fold key (group/model/effort), so one carries the badge width, and
    // the hidden members' dur/tok values never render. An EXPANDED run
    // contributes at most a screenful of members. Without this bound a folded
    // count wall re-measured (and re-priced) its whole raw length per frame.
    let visible: Vec<&Completed> = rows
        .iter()
        .skip(scroll)
        .take(capacity.saturating_add(8))
        .flat_map(|row| match row {
            ActivityRow::Single(idx) => vec![&view.completed[*idx]],
            ActivityRow::Run { start, len } => {
                let run = &view.completed[*start..*start + *len];
                let (expanded, _) = triage::run_toggle_key(run, chrome.expanded_run.as_ref());
                if expanded {
                    run.iter().take(capacity.saturating_add(8)).collect()
                } else {
                    vec![&run[0]]
                }
            }
        })
        .collect();
    let metrics = if chrome.activity_scroll == 0 {
        RowMetrics::measure(area.width, in_flight, &visible)
    } else {
        RowMetrics::measure(area.width, &[], &visible)
    };
    // In-flight rows pinned on top ONLY when viewing the live tail (scroll==0);
    // while scrolled into history they'd steal rows from the page being read.
    if chrome.activity_scroll == 0 {
        for request in in_flight.iter().rev().take(capacity) {
            let elapsed = now.duration_since(request.started_at).unwrap_or_default();
            // Working spinner differs by backend group: Claude gets the braille
            // orbit (magenta), Codex a quarter-block orbit (cyan) — the same
            // colors as the group labels — so you can tell what's running where
            // at a glance. Pre-routing rows (no account yet) are a dim braille.
            let (glyph, color) = match request.account.as_deref().and_then(|a| group_of(view, a)) {
                Some(BackendGroup::Codex) => (anim::block_spin(anim_frame), Color::Cyan),
                Some(BackendGroup::Claude) => (anim::braille_spin(anim_frame), Color::Magenta),
                Some(BackendGroup::Grok) => (anim::block_spin(anim_frame), Color::Yellow),
                None => (anim::braille_spin(anim_frame), Color::DarkGray),
            };
            let mut spans = vec![
                // Mirror the completed row's stamp spacing (`▸ HH:MM:SS  `):
                // spinner+2 spaces so the following `kind` column lines up.
                Span::styled(format!(" {glyph} "), Style::new().fg(color)),
                Span::styled(
                    format!("{}  ", format::clock_hms_utc(request.started_at)),
                    dim(),
                ),
            ];
            // `kind` column (TUI UI-6 item 1): same `{:<8} ` slot the completed
            // row uses, so the meta/email columns align across in-flight and
            // completed rows. Unknown kind → 8 blank cells (alignment holds).
            let kind = request.kind.as_deref().unwrap_or("");
            spans.push(Span::styled(format!("{kind:<8} "), kind_style(kind)));
            // `[model effort]` badge while in flight (issue #2, 2a): filled at
            // routing time (req11) with the same per-request values the finish
            // will record, so the running badge reads exactly like its
            // eventual completed row. Padded to the frame's shared meta slot.
            spans.extend(pad_spans(
                activity_meta_spans(
                    request.group.as_deref(),
                    request.model.as_deref(),
                    request.effort.as_deref(),
                    anim_frame,
                    view.tui_effects,
                    view.gradient,
                ),
                metrics.meta_w,
            ));
            if let Some(account) = &request.account {
                spans.push(Span::raw(format!(
                    " → {}",
                    row_account_name(account, view.email_anonymous, &view.domain_abbrev)
                )));
            }
            spans.push(Span::styled(
                format!(" ({}…)", format::elapsed_secs(elapsed)),
                dim(),
            ));
            lines.push(Line::from(spans));
        }
    }
    // Completed entries, newest first, windowed by the scroll offset (req6:
    // the whole history is reachable, not just the rows that happen to fit).
    // Each request row may expand into several detail lines when clicked; the
    // hit list records the absolute screen rows each entry owns so the click
    // handler maps a (col,row) back to its stable key. Paragraph renders line 0
    // at `area.y + 1` (the title takes the top border row).
    // glance-triage atom 3 (narrowed, Z 2026-07-15): fold runs of ≥FOLD_MIN
    // consecutive same-key `count` probes into one counted row — count_tokens
    // is the only traffic allowed to group; everything else renders 1:1.
    // Scrolling walks RENDER rows (a folded run is one step of history).
    let body_top = area.y.saturating_add(1);
    let mut hits: Vec<ActivityHit> = Vec::new();
    for row in rows.iter().skip(scroll) {
        if lines.len() >= capacity {
            break;
        }
        match row {
            ActivityRow::Single(idx) => {
                let entry = &view.completed[*idx];
                let expanded = entry
                    .activity_key()
                    .is_some_and(|k| chrome.expanded_activity.as_ref() == Some(&k));
                let row_y = body_top.saturating_add(lines.len() as u16);
                lines.push(completed_line(
                    entry,
                    expanded,
                    view.email_anonymous,
                    &view.session_labels,
                    &view.domain_abbrev,
                    &metrics,
                    anim_frame,
                    view.tui_effects,
                    view.gradient,
                ));
                let mut height = 1u16;
                if expanded {
                    for detail in
                        completed_detail_lines(entry, view.email_anonymous, &view.session_labels)
                    {
                        if lines.len() >= capacity {
                            break;
                        }
                        lines.push(detail);
                        height = height.saturating_add(1);
                    }
                }
                // Only request rows are clickable (notes have no key).
                if let Some(key) = entry.activity_key() {
                    push_raw_line_hit(&mut hits, &key, entry, row_y, height);
                    push_input_line_hit(&mut hits, &key, entry, row_y, height);
                    hits.push(ActivityHit {
                        key,
                        y_start: row_y,
                        height,
                        kind: ActivityHitKind::Entry,
                    });
                }
            }
            ActivityRow::Run { start, len } => {
                let run = &view.completed[*start..*start + *len];
                // Fold expansion matches ANY member (not just the oldest), so
                // a tail run on a FULL ring — whose oldest member is evicted
                // on every append — stays expanded until the clicked member
                // itself ages out. The header is its OWN one-row hit (the
                // marker toggles the fold); expanded members each get an
                // Entry hit so clicking one opens ITS detail instead of
                // collapsing the group (Z 2026-07-15).
                let (expanded, key) = triage::run_toggle_key(run, chrome.expanded_run.as_ref());
                let row_y = body_top.saturating_add(lines.len() as u16);
                lines.push(folded_run_line(
                    run,
                    expanded,
                    view.email_anonymous,
                    &view.domain_abbrev,
                    &metrics,
                    anim_frame,
                    view.tui_effects,
                    view.gradient,
                ));
                if let Some(key) = key {
                    hits.push(ActivityHit {
                        key,
                        y_start: row_y,
                        height: 1,
                        kind: ActivityHitKind::RunHeader { expanded },
                    });
                }
                if expanded {
                    for entry in run {
                        if lines.len() >= capacity {
                            break;
                        }
                        let member_expanded = entry
                            .activity_key()
                            .is_some_and(|k| chrome.expanded_activity.as_ref() == Some(&k));
                        let member_y = body_top.saturating_add(lines.len() as u16);
                        lines.push(completed_line(
                            entry,
                            member_expanded,
                            view.email_anonymous,
                            &view.session_labels,
                            &view.domain_abbrev,
                            &metrics,
                            anim_frame,
                            view.tui_effects,
                            view.gradient,
                        ));
                        let mut member_height = 1u16;
                        if member_expanded {
                            for detail in completed_detail_lines(
                                entry,
                                view.email_anonymous,
                                &view.session_labels,
                            ) {
                                if lines.len() >= capacity {
                                    break;
                                }
                                lines.push(detail);
                                member_height = member_height.saturating_add(1);
                            }
                        }
                        if let Some(key) = entry.activity_key() {
                            push_raw_line_hit(&mut hits, &key, entry, member_y, member_height);
                            push_input_line_hit(&mut hits, &key, entry, member_y, member_height);
                            hits.push(ActivityHit {
                                key,
                                y_start: member_y,
                                height: member_height,
                                kind: ActivityHitKind::Entry,
                            });
                        }
                    }
                }
            }
        }
    }

    // Title carries the scroll position so it's obvious you're in history. The
    // shown-range end is approximate when rows expanded, so report the count
    // windowed by the live-tail capacity.
    let shown_last = (scroll + capacity).min(total);
    let title = if scroll > 0 {
        format!(
            " activity — {}–{} of {total} (↑ history) ",
            scroll + 1,
            shown_last
        )
    } else if in_flight.is_empty() {
        format!(" activity — {total} ")
    } else {
        format!(" activity — {} in flight ", in_flight.len())
    };
    let block = Block::new().borders(Borders::TOP).title(title);
    frame.render_widget(Paragraph::new(lines).block(block), area);
    ActivityChrome { area, hits }
}

/// The account/email column width on activity rows (Z 2026-07-15: 이메일 10자).
const ACTIVITY_EMAIL_W: usize = 10;

/// Pad `text` with trailing spaces to exactly `width` display cells,
/// `…`-clipping first when it is too wide.
fn pad_cells(text: &str, width: usize) -> String {
    let clipped = truncate_cells(text, width);
    let pad = width.saturating_sub(cell_width(&clipped));
    format!("{clipped}{}", " ".repeat(pad))
}

/// Right-align `text` in `width` display cells (numeric columns).
fn pad_cells_left(text: &str, width: usize) -> String {
    let clipped = truncate_cells(text, width);
    let pad = width.saturating_sub(cell_width(&clipped));
    format!("{}{clipped}", " ".repeat(pad))
}

/// One folded activity row (glance-triage atom 3, narrowed to `count` runs):
/// `▸ HH:MM:SS count 33× [meta] email → (all 2xx)` — the START time only
/// (Z 2026-07-15), the run size riding the type column. The marker toggles
/// the fold; the body expands it.
#[allow(clippy::too_many_arguments)]
fn folded_run_line(
    run: &[Completed],
    expanded: bool,
    mask: bool,
    abbrev: &std::collections::BTreeMap<String, String>,
    m: &RowMetrics,
    frame: usize,
    effects_on: bool,
    g: GradientCfg,
) -> Line<'static> {
    let marker = if expanded { '▾' } else { '▸' };
    let oldest = &run[run.len() - 1];
    let stamp = Span::styled(
        format!(" {marker} {}  ", format::clock_hms_utc(oldest.at)),
        dim(),
    );
    let newest = &run[0];
    let CompletedBody::Request {
        account,
        group,
        model,
        effort,
        kind,
        ..
    } = &newest.body
    else {
        // Unreachable by construction (only requests fold); degrade to a note.
        return Line::from(stamp);
    };
    let account = account
        .as_deref()
        .map(|a| row_account_name(a, mask, abbrev))
        .unwrap_or_else(|| "?".to_string());
    let kind = kind.as_deref().unwrap_or("count");
    let mut spans = vec![
        stamp,
        Span::styled(format!("{kind:<8} "), kind_style(kind)),
        Span::styled(
            format!("{}× ", run.len()),
            Style::new().add_modifier(Modifier::BOLD),
        ),
    ];
    spans.extend(pad_spans(
        activity_meta_spans(
            group.as_deref(),
            model.as_deref(),
            effort.as_deref(),
            frame,
            effects_on,
            g,
        ),
        m.meta_w,
    ));
    spans.push(Span::raw(format!(
        " {} → (",
        pad_cells(&account, ACTIVITY_EMAIL_W)
    )));
    spans.push(Span::styled("all 2xx", Style::new().fg(Color::Green)));
    spans.push(Span::raw(")"));
    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
fn completed_line(
    entry: &Completed,
    expanded: bool,
    mask: bool,
    session_labels: &std::collections::BTreeMap<String, String>,
    abbrev: &std::collections::BTreeMap<String, String>,
    m: &RowMetrics,
    frame: usize,
    effects_on: bool,
    g: GradientCfg,
) -> Line<'static> {
    match &entry.body {
        CompletedBody::Request {
            id: _,
            method,
            path,
            account,
            status,
            duration,
            tokens,
            group,
            model,
            effort,
            fast: _,
            ttfb_ms: _,
            ttft_ms: _,
            gen_ms: _,
            aborted: _,
            user_id,
            kind,
            excerpt,
        } => {
            // Row layout (Z 2026-07-15): every column padded to the frame's
            // max width so rows line up, and the input excerpt LAST, spending
            // whatever terminal width remains:
            //   ▸ HH:MM:SS kind [model effort] email → 200 3.1s 269tok $0.0079 «label» "input…"
            let marker = if expanded { '▾' } else { '▸' };
            let stamp = Span::styled(
                format!(" {marker} {}  ", format::clock_hms_utc(entry.at)),
                dim(),
            );
            // Same display form as the accounts table (Z 2026-07-15
            // "똑같은 함수"): mask → strip the `group:` prefix → abbreviate
            // the domain, then clip to the 10-cell email column. The expanded
            // detail keeps the FULL raw id (the fidelity surface, issue #70).
            let account = account
                .as_deref()
                .map(|a| row_account_name(a, mask, abbrev))
                .unwrap_or_else(|| "?".to_string());
            let status_style = if *status < 400 {
                Style::new().fg(Color::Green)
            } else {
                Style::new().fg(Color::Red)
            };
            // method+path intentionally NOT on the collapsed row (Z
            // 2026-07-15 "쓸데 없는 데이터"): it is constant noise
            // (`POST /v1/messages?beta=true`); the expanded detail's
            // `request` line keeps the full form.
            let _ = (method, path);
            let (tok, cost, cost_usd) = match tokens {
                Some(tokens) => {
                    let tok = format!("{}tok", format::human_count(tokens.total()));
                    // API-equivalent cost via the built-in default rate table
                    // (the render path holds no config overrides). Priced only
                    // when (group, model) is known.
                    let (cost, cost_usd) = match (group, model) {
                        (Some(group), Some(model)) => {
                            let usd = crate::pricing::cost_usd(
                                group,
                                model,
                                tokens,
                                &std::collections::HashMap::new(),
                            );
                            (format_cost(usd), Some(usd))
                        }
                        _ => ("—".to_string(), None),
                    };
                    (tok, cost, cost_usd)
                }
                None => ("—".to_string(), "—".to_string(), None),
            };
            let mut spans = vec![stamp];
            spans.push(Span::styled(
                format!("{:<8} ", kind.as_deref().unwrap_or("")),
                kind_style(kind.as_deref().unwrap_or("")),
            ));
            spans.extend(pad_spans(
                activity_meta_spans(
                    group.as_deref(),
                    model.as_deref(),
                    effort.as_deref(),
                    frame,
                    effects_on,
                    g,
                ),
                m.meta_w,
            ));
            spans.push(Span::raw(format!(
                " {} → ",
                pad_cells(&account, ACTIVITY_EMAIL_W)
            )));
            spans.push(Span::styled(format!("{status:>3}"), status_style));
            spans.push(Span::styled(
                format!(
                    " {} {} {}",
                    pad_cells_left(&format::elapsed_secs(*duration), m.dur_w),
                    pad_cells_left(&tok, m.tok_w),
                    pad_cells_left(&row_tps_label(tokens.as_ref(), *duration), m.tps_w),
                ),
                dim(),
            ));
            // Cost ≥ $1 shouts (TUI UI-6 item 5): the color boundary matches
            // format_cost's 2dp boundary. Below $1 (or unpriced) renders plain.
            let cost_cell = format!(" {}", pad_cells_left(&cost, m.cost_w));
            if cost_usd.is_some_and(|usd| usd >= 1.0) {
                spans.push(Span::styled(
                    cost_cell,
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::raw(cost_cell));
            }
            // Derived session title (U2), when this client id has one.
            if let Some(label) = user_id.as_deref().and_then(|id| session_labels.get(id)) {
                spans.push(Span::styled(
                    format!(
                        " \u{ab}{}\u{bb}",
                        truncate_chars(&masked_text(label, mask), 16)
                    ),
                    dim().add_modifier(Modifier::ITALIC),
                ));
            }
            // Input excerpt LAST, filling the rest of the panel width.
            if let Some(excerpt) = excerpt.as_deref() {
                let consumed: usize = spans.iter().map(|s| cell_width(&s.content)).sum();
                // 1 leading space + a pair of quotes.
                let budget = (m.width as usize).saturating_sub(consumed + 3);
                if budget > 0 {
                    spans.push(Span::raw(format!(
                        " \u{201c}{}\u{201d}",
                        truncate_cells(&masked_text(excerpt, mask), budget)
                    )));
                }
            }
            Line::from(spans)
        }
        CompletedBody::Note { text, error } => {
            let stamp = Span::styled(format!("   {}  ", format::clock_hms_utc(entry.at)), dim());
            let style = if *error {
                Style::new().fg(Color::Red)
            } else {
                Style::new()
            };
            Line::from(vec![stamp, Span::styled(masked_text(text, mask), style)])
        }
    }
}

/// Indented detail lines for an expanded request row (Feature B): full
/// method+path, account, status, duration, group/model/effort, the token
/// breakdown, and the per-component + total API-equivalent cost via
/// [`crate::pricing`]. Empty for notes (never expandable). `mask` = the
/// email-anonymous display setting (the account line renders aliased).
fn completed_detail_lines(
    entry: &Completed,
    mask: bool,
    session_labels: &std::collections::BTreeMap<String, String>,
) -> Vec<Line<'static>> {
    let CompletedBody::Request {
        id: _,
        method,
        path,
        account,
        status,
        duration,
        tokens,
        group,
        model,
        effort,
        fast,
        ttfb_ms,
        ttft_ms,
        gen_ms,
        aborted,
        user_id,
        kind,
        excerpt,
    } = &entry.body
    else {
        return Vec::new();
    };
    let indent = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!("       {label:<8}"), dim()),
            Span::raw(value),
        ])
    };
    let mut lines = Vec::new();
    // The `🔍` marks this line as CLICKABLE — it opens the raw request/response
    // viewer (UI-7): full bodies + headers + metadata, CDT-style tabs. Same
    // emoji affordance as the `🔍 input` line below.
    lines.push(Line::from(vec![
        Span::styled("     🔍 request ", dim()),
        Span::raw(format!("{method} {path}")),
    ]));
    // The click-expanded input line (U3): the full stored excerpt on ONE line,
    // as wide as the terminal (the Paragraph clips, never wraps — so the line
    // is exactly "as long as fits").
    if let Some(kind) = kind.as_deref() {
        lines.push(Line::from(vec![
            Span::styled("       kind    ", dim()),
            Span::styled(kind.to_string(), kind_style(kind)),
        ]));
    }
    if let Some(excerpt) = excerpt.as_deref() {
        // The `🔍` marks this line as CLICKABLE — it opens the full-text modal
        // (UI-6 item 3). Emoji is safe here (the detail line owns its whole row,
        // unlike the aligned table columns where wide/ambiguous glyphs break the
        // grid). The collapsed row still width-clips; the modal shows it in full.
        lines.push(Line::from(vec![
            Span::styled("       🔍 input ", dim()),
            Span::raw(masked_text(excerpt, mask)),
        ]));
    }
    if let Some(uid) = user_id.as_deref() {
        let label = session_labels
            .get(uid)
            .map(|l| format!(" \u{ab}{}\u{bb}", masked_text(l, mask)))
            .unwrap_or_default();
        lines.push(indent("client", format!("{uid}{label}")));
    }
    lines.push(indent(
        "account",
        account
            .as_deref()
            .map(|a| masked_name(a, mask))
            .unwrap_or_else(|| "?".to_string()),
    ));
    let status_color = if *status < 400 {
        Color::Green
    } else {
        Color::Red
    };
    lines.push(Line::from(vec![
        Span::styled("       status  ", dim()),
        Span::styled(status.to_string(), Style::new().fg(status_color)),
        Span::styled(
            format!("  ·  {} elapsed", format::elapsed_secs(*duration)),
            dim(),
        ),
    ]));
    let model_label = match (group.as_deref(), model.as_deref()) {
        (Some(g), Some(m)) => format!("{g} {m}"),
        (Some(g), None) => g.to_string(),
        (None, Some(m)) => m.to_string(),
        (None, None) => "—".to_string(),
    };
    let effort_label = effort
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty() && *e != "-")
        .map(|e| format!(" · effort {e}"))
        .unwrap_or_default();
    let fast_label = match fast {
        Some(true) => " · fast",
        Some(false) => "",
        None => " · fast?", // pre-field history: unknown, never claimed off
    };
    lines.push(indent(
        "model",
        format!("{model_label}{effort_label}{fast_label}"),
    ));
    // Perf telemetry v1: e2e observed throughput (always derivable when
    // tokens+duration exist) + the ESTIMATED post-delta figure when the
    // stream recorded a first output delta. "est" is deliberate — the first
    // streamed delta is not a model-internal first token (hidden reasoning
    // may precede it), so no decode-speed claim is made.
    {
        let mut parts: Vec<String> = Vec::new();
        if let Some(tps) = e2e_tps(tokens.as_ref(), *duration) {
            parts.push(format!("e2e {tps:.1} t/s"));
        }
        if let (Some(gen), Some(t)) = (gen_ms, tokens) {
            if t.output > 0 && *gen > 0 {
                let est = t.output as f64 * 1000.0 / *gen as f64;
                parts.push(format!("est {est:.1} t/s post-delta"));
            }
        }
        if let Some(ttfb) = ttfb_ms {
            parts.push(format!(
                "ttfb {}",
                format::elapsed_secs(Duration::from_millis(*ttfb))
            ));
        }
        if let Some(ttft) = ttft_ms {
            parts.push(format!(
                "first output {}",
                format::elapsed_secs(Duration::from_millis(*ttft))
            ));
        }
        if *aborted {
            parts.push("stream aborted".into());
        }
        if !parts.is_empty() {
            lines.push(indent("perf", parts.join("  ·  ")));
        }
    }
    match tokens {
        Some(t) => {
            lines.push(indent(
                "tokens",
                format!(
                    "in {} · out {} · cache_read {} · cache_creation {} · total {}",
                    format::human_count(t.input),
                    format::human_count(t.output),
                    opt_count(t.cache_read),
                    opt_count(t.cache_creation),
                    format::human_count(t.total()),
                ),
            ));
            // Per-component + total API-equivalent cost (item #4). Empty
            // overrides = built-in default rate table. Each component is priced
            // in isolation via `cost_from_parts`, so the four add up to total.
            let empty = std::collections::HashMap::new();
            let (g, m) = (
                group.as_deref().unwrap_or(""),
                model.as_deref().unwrap_or(""),
            );
            let cost_in = crate::pricing::cost_from_parts(g, m, t.input, 0, None, None, &empty);
            let cost_out = crate::pricing::cost_from_parts(g, m, 0, t.output, None, None, &empty);
            let cost_cr = crate::pricing::cost_from_parts(g, m, 0, 0, t.cache_read, None, &empty);
            let cost_cc =
                crate::pricing::cost_from_parts(g, m, 0, 0, None, t.cache_creation, &empty);
            let cost_total = cost_in + cost_out + cost_cr + cost_cc;
            lines.push(Line::from(vec![
                Span::styled("       cost    ", dim()),
                Span::raw(format!(
                    "in {} · out {} · cache_read {} · cache_creation {} · ",
                    format_cost(cost_in),
                    format_cost(cost_out),
                    format_cost(cost_cr),
                    format_cost(cost_cc),
                )),
                Span::styled(format_cost(cost_total), Style::new().fg(Color::Green)),
            ]));
        }
        None => lines.push(indent("tokens", "—".to_string())),
    }
    lines
}

/// Row offset (0-based, within [`completed_detail_lines`]) of the clickable
/// `🔍 input` detail line for `entry`, or `None` when the entry stores no
/// excerpt. MUST track the line order emitted above (request, [kind], input, …)
/// — the UI-6 item-3 modal hit is anchored off it, so any reordering of those
/// leading lines must update this in lock-step.
fn completed_input_line_offset(entry: &Completed) -> Option<u16> {
    let CompletedBody::Request { kind, excerpt, .. } = &entry.body else {
        return None;
    };
    excerpt.as_ref()?;
    // Lines before `input`: always `request` (1), plus `kind` when present.
    Some(1 + u16::from(kind.is_some()))
}

/// Register the one-row `InputLine` hit for `entry`'s `🔍 input` detail line,
/// but only when that line was actually rendered this frame (the entry is
/// expanded AND the line was not clipped by the panel's line budget). Pushed
/// BEFORE the caller's block-level `Entry` hit so `hit_test_activity`'s
/// first-match resolves this exact row to "open the modal" (UI-6 item 3).
/// `row_y` is the entry's main row; detail lines follow it, so the input line
/// sits at `row_y + 1 + offset`, and `offset + 1 < height` (height counts the
/// main row plus rendered detail lines) proves it is on screen.
/// Register the one-row `RawLine` hit for `entry`'s `🔍 request` detail line
/// (UI-7), when the entry is expanded AND that line survived the panel's line
/// budget. The request line is ALWAYS the first detail line (offset 0), so it
/// sits at `row_y + 1`; `height > 1` proves at least one detail line rendered.
/// Pushed BEFORE the block-level `Entry` hit so first-match resolves the row
/// to "open the raw viewer" — same layering as [`push_input_line_hit`].
fn push_raw_line_hit(
    hits: &mut Vec<ActivityHit>,
    key: &ActivityKey,
    entry: &Completed,
    row_y: u16,
    height: u16,
) {
    if let CompletedBody::Request { id, .. } = &entry.body {
        if height > 1 {
            hits.push(ActivityHit {
                key: key.clone(),
                y_start: row_y.saturating_add(1),
                height: 1,
                kind: ActivityHitKind::RawLine { id: *id },
            });
        }
    }
}

fn push_input_line_hit(
    hits: &mut Vec<ActivityHit>,
    key: &ActivityKey,
    entry: &Completed,
    row_y: u16,
    height: u16,
) {
    if let Some(offset) = completed_input_line_offset(entry) {
        if offset + 1 < height {
            hits.push(ActivityHit {
                key: key.clone(),
                y_start: row_y.saturating_add(1).saturating_add(offset),
                height: 1,
                kind: ActivityHitKind::InputLine,
            });
        }
    }
}

/// Backend group of the account named `account` in the current snapshot, for
/// coloring/animating its in-flight rows. `None` if not found (pre-routing).
fn group_of(view: &DashboardView, account: &str) -> Option<BackendGroup> {
    view.snapshot
        .accounts
        .iter()
        .find(|a| a.id.0 == account)
        .map(|a| a.group)
}

/// Style for a message-kind tag (TUI UI-3 U1): plain user turns read as the
/// default text, security-monitor turns shout, the mechanical control turns
/// (compact/title/suggest/count/…) stay dim so user rows dominate the eye.
fn kind_style(kind: &str) -> Style {
    match kind {
        "user" => Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        "security" => Style::new().fg(Color::Yellow),
        "compact" | "summary" | "recap" => Style::new().fg(Color::Blue),
        "subagent" | "sdk" => Style::new().fg(Color::Cyan).add_modifier(Modifier::DIM),
        _ => dim(),
    }
}

/// Truncate to `max` chars (boundary-safe) with a `…` marker when clipped.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Color a backend-group label: codex = cyan, claude = magenta, unknown = gray.
/// Shared by the account table (req5) and the activity log (req7) so the group
/// reads the same everywhere.
fn group_color(group: Option<&str>) -> Style {
    match group {
        Some("codex") => Style::new().fg(Color::Cyan),
        Some("claude") => Style::new().fg(Color::Magenta),
        Some("grok") => Style::new().fg(Color::Yellow),
        _ => dim(),
    }
}

/// Abbreviate a model id for the activity badge by dropping the redundant
/// `claude-` prefix on Claude models (`claude-opus-4-8` → `opus-4-8`). Codex
/// and unknown models pass through unchanged (issue #2, 2b).
fn abbrev_model<'a>(group: Option<&str>, model: &'a str) -> &'a str {
    if group == Some("claude") {
        model.strip_prefix("claude-").unwrap_or(model)
    } else {
        model
    }
}

/// Smooth animated gradients (TUI UI-7): a port of herdr-mx's host-banner
/// "lolcat" effect (`herdr src/ui/sidebar.rs`), replacing the old discrete
/// ANSI marquee palettes — those slid hard color bands one cell per tick and
/// read as flicker, not a gradient. Two modes, exactly like herdr:
///
/// - **rainbow** — per-char truecolor from a 3-phase sine sweep (R/G/B offset
///   by 120°), hue rotating with the frame. Used for the `max` effort token
///   (the deliberately loud top-effort marker, UI-6 item 6).
/// - **solid (단색)** — a fixed per-group base color whose LUMA breathes with
///   the same sine phase, hue never changing. Used for headline-model names
///   (`fable-5*` magenta family, `gpt-5.6-sol*` cyan family, UI-6 item 7).
///
/// `phase = FREQ * char_index + DRIFT * frame` gives the spatial spread across
/// characters plus temporal drift; luma is floored at `MIN_LUMA` for
/// legibility. FREQ/luma mirror herdr's constants. DRIFT is the speed-1.0
/// baseline: at llmux's 120 ms render tick (~8 fps) it completes a luma cycle
/// in ~2.2 s — herdr's `normal` (0.09/frame at its faster tick) translated to
/// a tick this slow read as barely moving (UI-8 user report), hence the
/// larger baseline. Config `tui_gradient.speed` multiplies it.
const GRADIENT_MIN_LUMA: f32 = 0.45;
const GRADIENT_MAX_LUMA: f32 = 1.00;
const GRADIENT_FREQ: f32 = 0.30;
const GRADIENT_DRIFT: f32 = 0.35;

/// The shared gradient phase for `(frame, char_idx)`. The frame is bounded
/// before it meets f32: past ~2^24 an f32 can no longer step by 1, so
/// `DRIFT * frame` would freeze on a long-lived daemon/attach TUI. The modulo
/// wrap (~33 h at 120 ms/tick) is a single-frame phase jump — invisible next
/// to a frozen animation.
fn gradient_phase(frame: usize, char_idx: usize, speed: f32) -> f32 {
    GRADIENT_FREQ * char_idx as f32 + GRADIENT_DRIFT * speed * ((frame % 1_000_000) as f32)
}

/// Rainbow mode: deterministic truecolor for `(frame, char_idx)` — R/G/B are
/// the same sine offset by 0° / 120° / 240°, so the hue rotates while every
/// channel stays inside the legible luma band.
fn gradient_rainbow(frame: usize, char_idx: usize, speed: f32) -> Color {
    let phase = gradient_phase(frame, char_idx, speed);
    let chan = |offset: f32| -> u8 {
        let raw = (phase + offset).sin() * 0.5 + 0.5; // 0.0..=1.0
        let lit = GRADIENT_MIN_LUMA + raw * (GRADIENT_MAX_LUMA - GRADIENT_MIN_LUMA);
        (lit * 255.0).round().clamp(0.0, 255.0) as u8
    };
    Color::Rgb(
        chan(0.0),
        chan(std::f32::consts::TAU / 3.0),
        chan(2.0 * std::f32::consts::TAU / 3.0),
    )
}

/// Solid (단색) mode: scale a fixed base color by the sine luma factor — the
/// hue is the group's, only its brightness breathes along the text.
fn gradient_solid(base: (u8, u8, u8), frame: usize, char_idx: usize, speed: f32) -> Color {
    let raw = gradient_phase(frame, char_idx, speed).sin() * 0.5 + 0.5; // 0.0..=1.0
    let factor = GRADIENT_MIN_LUMA + raw * (GRADIENT_MAX_LUMA - GRADIENT_MIN_LUMA);
    let scale = |c: u8| (f32::from(c) * factor).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(scale(base.0), scale(base.1), scale(base.2))
}

/// Solid-gradient base colors per headline family: truecolor anchors of the
/// groups' ANSI families (claude = magenta, codex = cyan).
const CLAUDE_GRADIENT_BASE: (u8, u8, u8) = (255, 121, 198);
const CODEX_GRADIENT_BASE: (u8, u8, u8) = (86, 220, 220);

/// Render-ready gradient tuning (UI-8), resolved ONCE per document from
/// config `tui_gradient` at view-build time: hex colors parsed (unparseable →
/// built-in base), speed sanitized (non-finite/non-positive → 1.0). `Copy` —
/// handed by value into the span builders every frame, so `ui.rs` never
/// re-validates strings on the render path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientCfg {
    pub speed: f32,
    pub claude: (u8, u8, u8),
    pub codex: (u8, u8, u8),
    /// `Some(base)` replaces the max-effort rainbow with a solid gradient on
    /// that color; `None` keeps the rainbow.
    pub max_effort: Option<(u8, u8, u8)>,
}

impl GradientCfg {
    pub fn from_config(cfg: &crate::config::TuiGradient) -> Self {
        let speed = if cfg.speed.is_finite() && cfg.speed > 0.0 {
            cfg.speed
        } else {
            1.0
        };
        Self {
            speed,
            claude: parse_hex_color(&cfg.claude).unwrap_or(CLAUDE_GRADIENT_BASE),
            codex: parse_hex_color(&cfg.codex).unwrap_or(CODEX_GRADIENT_BASE),
            max_effort: cfg.max_effort.as_deref().and_then(parse_hex_color),
        }
    }
}

impl Default for GradientCfg {
    fn default() -> Self {
        Self::from_config(&crate::config::TuiGradient::default())
    }
}

/// Parse `#rrggbb` (case-insensitive, `#` required) into an RGB triple.
/// Anything else — short forms, missing `#`, bad hex — is `None`, and the
/// caller falls back to the built-in base rather than guessing.
fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

/// The solid-gradient base color for a model whose abbreviated slug marks it a
/// HEADLINE model (`fable-5*` / `gpt-5.6-sol*`), or `None` for ordinary models
/// (which keep their flat group color). Detection runs on the [`abbrev_model`]
/// slug so it matches whether the caller renders the raw id (models strip) or
/// the already-abbreviated badge slug (activity badge). The base comes from
/// the resolved config ([`GradientCfg`]), defaulting to the built-in anchors.
fn model_gradient_base(group: Option<&str>, model: &str, g: GradientCfg) -> Option<(u8, u8, u8)> {
    let slug = abbrev_model(group, model);
    if slug.starts_with("fable-5") || slug.starts_with("gpt-5.6-sol") {
        Some(match group {
            Some("codex") => g.codex,
            _ => g.claude,
        })
    } else {
        None
    }
}

/// Render `text` as a headline-model name (TUI UI-6 item 7): an animated
/// per-char group-family gradient when `effects_on` and the slug is a headline
/// model, a static bold group color when effects are off, and a plain
/// group-colored single span for ordinary models. The concatenated span text
/// is always exactly `text` (the per-char split is style-only).
fn model_name_spans(
    group: Option<&str>,
    text: &str,
    frame: usize,
    effects_on: bool,
    g: GradientCfg,
) -> Vec<Span<'static>> {
    match model_gradient_base(group, text, g) {
        Some(base) if effects_on => text
            .chars()
            .enumerate()
            .map(|(i, c)| {
                Span::styled(
                    c.to_string(),
                    Style::new()
                        .fg(gradient_solid(base, frame, i, g.speed))
                        .add_modifier(Modifier::BOLD),
                )
            })
            .collect(),
        Some(_) => vec![Span::styled(
            text.to_string(),
            group_color(group).add_modifier(Modifier::BOLD),
        )],
        None => vec![Span::styled(text.to_string(), group_color(group))],
    }
}

/// Render the effort token with its per-level styling (TUI UI-6 item 6):
/// `xhigh` → a static [`Color::LightRed`] bold (distinct, animation-free);
/// `max` → the rainbow marquee when `effects_on`, else a static
/// [`Color::LightMagenta`] bold; every other effort inherits the group color.
/// The concatenated span text is always exactly `effort`.
fn effort_spans(
    group: Option<&str>,
    effort: &str,
    frame: usize,
    effects_on: bool,
    g: GradientCfg,
) -> Vec<Span<'static>> {
    match effort {
        "xhigh" => vec![Span::styled(
            effort.to_string(),
            Style::new()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        )],
        // Config `tui_gradient.max_effort` swaps the rainbow for a solid
        // gradient on the chosen color (UI-8); default keeps the rainbow.
        "max" if effects_on => effort
            .chars()
            .enumerate()
            .map(|(i, c)| {
                let color = match g.max_effort {
                    Some(base) => gradient_solid(base, frame, i, g.speed),
                    None => gradient_rainbow(frame, i, g.speed),
                };
                Span::styled(
                    c.to_string(),
                    Style::new().fg(color).add_modifier(Modifier::BOLD),
                )
            })
            .collect(),
        "max" => vec![Span::styled(
            effort.to_string(),
            Style::new()
                .fg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD),
        )],
        _ => vec![Span::styled(effort.to_string(), group_color(group))],
    }
}

/// Display-cell width of `text` — ratatui's own column accounting
/// (`unicode-width`), NOT the char count: group/model/effort ride in from
/// request bodies, so wide/combining input must not shift the badge column.
fn cell_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// `…`-clip `text` to at most `max` display CELLS (the cell-aware sibling of
/// [`truncate_chars`]). `max == 0` yields the empty string. The bound is
/// enforced by WHOLE-STRING re-measurement with the same [`cell_width`] the
/// caller pads with — never by summing per-char widths, which disagrees with
/// string measurement on context-sensitive sequences (VS16/ZWJ): chars are
/// popped until the remainder plus the one-cell ellipsis fits (review R2).
fn truncate_cells(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if cell_width(text) <= max {
        return text.to_string();
    }
    let budget = max - 1; // room for the ellipsis
    let mut out = text.to_string();
    while !out.is_empty() && cell_width(&out) > budget {
        out.pop();
    }
    out.push('\u{2026}');
    out
}

/// Compose the UNPADDED `[model effort]` badge body for an activity line
/// (Z 2026-07-15): the group WORD is gone (the badge is already group-colored
/// and the group repeated on every row was noise — `claude opus-4-8[1m]` →
/// `opus-4-8[1m]`, `codex gpt-5.6-sol` → `gpt-5.6-sol`), the model shows for
/// EVERY group, the effort token is dropped when unknown (`None`, empty, or
/// `"-"`), and the `fast` token is dropped entirely (the expanded detail's
/// `model` line keeps it). Callers pad the result to the frame's shared
/// [`RowMetrics::meta_w`] so the columns line up at the widest visible badge.
fn activity_meta_body(group: Option<&str>, model: Option<&str>, effort: Option<&str>) -> String {
    // Treat "-"/empty effort as unknown (the fold stamps unknown as "none"/"-").
    let effort = effort.map(str::trim).filter(|e| !e.is_empty() && *e != "-");
    let model = model.map(|m| abbrev_model(group, m));
    let parts: Vec<&str> = [model, effort].into_iter().flatten().collect();
    if parts.is_empty() {
        return String::new();
    }
    // Belt-and-braces cap: model/effort ride in from request bodies, so a
    // hostile value must not blow the shared column past the panel.
    truncate_cells(&format!("[{}]", parts.join(" ")), META_W_MAX)
}

/// Styled multi-span form of [`activity_meta_body`] (TUI UI-6 items 6/7): the
/// SAME `[model effort]` text — same abbreviation, same effort filtering, same
/// [`META_W_MAX`] cap and `…`-clip — but the model and effort tokens carry
/// per-level styling (headline-model gradient, effort marquee) instead of one
/// flat group color. Callers pad the result with [`pad_spans`] to the frame's
/// shared `meta_w`. The concatenated span text is byte-identical to what
/// `activity_meta_body` (still the width-measuring SSOT) returns, so the two
/// never disagree on column widths; in the rare hostile-width case that trips
/// the cap the badge degrades to a single clipped span (text stays identical,
/// only the per-char styling is dropped).
fn activity_meta_spans(
    group: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
    frame: usize,
    effects_on: bool,
    g: GradientCfg,
) -> Vec<Span<'static>> {
    // Same filtering as activity_meta_body so the parts match exactly.
    let effort = effort.map(str::trim).filter(|e| !e.is_empty() && *e != "-");
    let model_slug = model.map(|m| abbrev_model(group, m));
    // Ordered inner parts — exactly the set activity_meta_body joins with a
    // single space; each may itself be several per-char spans.
    let mut parts: Vec<Vec<Span<'static>>> = Vec::new();
    if let Some(m) = model_slug {
        parts.push(model_name_spans(group, m, frame, effects_on, g));
    }
    if let Some(e) = effort {
        parts.push(effort_spans(group, e, frame, effects_on, g));
    }
    if parts.is_empty() {
        return Vec::new();
    }
    let base = group_color(group);
    let mut spans: Vec<Span<'static>> = vec![Span::styled("[", base)];
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" ", base));
        }
        spans.extend(part);
    }
    spans.push(Span::styled("]", base));
    // Enforce the identical cap by WHOLE-STRING re-measurement (matching
    // activity_meta_body): a hostile assembly collapses to the same single
    // `…`-clipped span so the text never diverges from the width SSOT.
    let raw: String = spans.iter().map(|s| s.content.as_ref()).collect();
    if cell_width(&raw) <= META_W_MAX {
        spans
    } else {
        vec![Span::styled(truncate_cells(&raw, META_W_MAX), base)]
    }
}

/// Right-pad a multi-span badge to `width` display CELLS by the SUM of its span
/// cell widths (never char count) — the multi-span analogue of [`pad_cells`],
/// so the styled `[model effort]` badge lines up with the plain-padded columns
/// around it. The badge text is capped at [`META_W_MAX`] and `width` is the
/// frame's max badge width, so a trailing blank span always fills the gap.
fn pad_spans(mut spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let used: usize = spans.iter().map(|s| cell_width(s.content.as_ref())).sum();
    let pad = width.saturating_sub(used);
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    spans
}

/// Bottom log console: the tail of the tracing ring, newest line on the
/// bottom row (auto-follow), level-colored prefix, no wrapping (long lines
/// truncate — the console is a glance surface, not a pager).
fn draw_logs(frame: &mut Frame, area: Rect, view: &DashboardView) {
    let block = Block::new().borders(Borders::TOP).title(" logs ");
    let capacity = area.height.saturating_sub(1) as usize; // top border
                                                           // `view.logs` is oldest→newest; take the newest `capacity` lines.
    let start = view.logs.len().saturating_sub(capacity);
    let lines: Vec<Line> = view.logs[start..]
        .iter()
        .map(|line| log_line(line, view.email_anonymous))
        .collect();
    // Bottom-align: pad above so the newest line hugs the bottom edge.
    let mut padded: Vec<Line> = Vec::with_capacity(capacity);
    padded.resize_with(capacity.saturating_sub(lines.len()), Line::default);
    padded.extend(lines);
    frame.render_widget(Paragraph::new(padded).block(block), area);
}

fn log_line(line: &LogLine, mask: bool) -> Line<'_> {
    use tracing::Level;

    let (label, style) = if line.level == Level::ERROR {
        ("ERROR", Style::new().fg(Color::Red))
    } else if line.level == Level::WARN {
        (" WARN", Style::new().fg(Color::Yellow))
    } else if line.level == Level::INFO {
        (" INFO", Style::new())
    } else if line.level == Level::DEBUG {
        ("DEBUG", dim())
    } else {
        ("TRACE", dim())
    };
    Line::from(vec![
        Span::styled(format!(" {label} "), style),
        // Tracing lines carry account emails (switch/refresh messages) — the
        // email-anonymous setting masks them at render time.
        Span::raw(masked_text(&line.text, mask)),
    ])
}

// ---------------------------------------------------------------------------
// Model usage (req1-20): compact strip + detailed table/drill-down.
// ---------------------------------------------------------------------------

/// Total tokens processed for one model row: fresh input + output + cache
/// reads + cache writes. The cached classes must be counted for the same
/// reason as [`crate::tui::TokenCounts::total`] — `tokens_in` is the FRESH
/// prompt only, so cache-heavy traffic (Claude Code) would otherwise show a
/// tiny `tok` while the `$` column ([`model_cost`]) still prices all four
/// classes. `None` cache counters (upstream never reported them) count 0.
fn model_total(m: &ModelUsageDoc) -> u64 {
    m.tokens_in
        .saturating_add(m.tokens_out)
        .saturating_add(m.cache_read.unwrap_or(0))
        .saturating_add(m.cache_creation.unwrap_or(0))
}

/// API-equivalent USD cost for one model row (item #4): the server-computed
/// [`ModelUsageDoc::cost_usd`] (issue #62 S1), which already reflects the
/// daemon's pricing overrides. Docs from daemons that predate the field carry
/// the serde default `0.0` — for those, when the row has tokens, fall back to
/// pricing the token parts locally so the `$` column stays useful during a
/// rolling upgrade. The fallback holds no config overrides (empty map = the
/// built-in default rate table); an unknown/zero-rate `(group, model)` still
/// yields `0.0`.
fn model_cost(m: &ModelUsageDoc) -> f64 {
    if m.cost_usd > 0.0 {
        return m.cost_usd;
    }
    if m.tokens_in.saturating_add(m.tokens_out) == 0 {
        return 0.0;
    }
    crate::pricing::cost_from_parts(
        &m.group,
        &m.model,
        m.tokens_in,
        m.tokens_out,
        m.cache_read,
        m.cache_creation,
        &std::collections::HashMap::new(),
    )
}

/// "—" when unavailable (the upstream never reported it), else a human count —
/// so the UI never implies a precise zero it does not have (req9).
fn opt_count(v: Option<u64>) -> String {
    match v {
        Some(n) => format::human_count(n),
        None => "—".to_string(),
    }
}

/// Compact "last used" age for the strip ("12s", "3m"); "—" for in-flight-only
/// rows that have no completed request yet.
fn model_age_compact(last_used_ms: u64, now: SystemTime) -> String {
    if last_used_ms == 0 {
        return "—".to_string();
    }
    let at = UNIX_EPOCH + Duration::from_millis(last_used_ms);
    now.duration_since(at)
        .map(select::compact_duration)
        .unwrap_or_else(|_| "now".to_string())
}

fn model_is_recent(last_used_ms: u64, now: SystemTime) -> bool {
    if last_used_ms == 0 {
        return false;
    }
    let at = UNIX_EPOCH + Duration::from_millis(last_used_ms);
    now.duration_since(at)
        .map(|age| age <= MODEL_RECENT_WINDOW)
        .unwrap_or(true)
}

/// Leading marker for a model row: a group-colored working spinner while it has
/// in-flight traffic (req11), a `●` when recently used (req15), else blank.
fn model_active_marker(m: &ModelUsageDoc, now: SystemTime, frame: usize) -> Span<'static> {
    // A LEADING space inside the 2-cell marker column for every variant (TUI
    // UI-6 item 2b): the glyph sits in the 2nd cell so the marker breathes one
    // cell right of the border while the column stays aligned.
    if m.in_flight > 0 {
        let glyph = if m.group == "codex" {
            anim::block_spin(frame)
        } else {
            anim::braille_spin(frame)
        };
        Span::styled(format!(" {glyph}"), group_color(Some(m.group.as_str())))
    } else if model_is_recent(m.last_used_ms, now) {
        Span::styled(" ●", Style::new().fg(Color::Green))
    } else {
        Span::raw("  ")
    }
}

/// A `GROUP model` label pair, group-colored, model bold when active. Headline
/// models (`fable-5*` / `gpt-5.6-sol*`) render the same animated group-family
/// gradient as the activity badge (TUI UI-6 item 7), gated on `effects_on`;
/// ordinary models keep the plain bold-when-active name.
fn model_name_cells(
    m: &ModelUsageDoc,
    active: bool,
    frame: usize,
    effects_on: bool,
    g: GradientCfg,
) -> (Cell<'static>, Cell<'static>) {
    let group = Cell::from(Span::styled(
        m.group.to_uppercase(),
        group_color(Some(m.group.as_str())).add_modifier(Modifier::BOLD),
    ));
    let name_cell = if model_gradient_base(Some(m.group.as_str()), &m.model, g).is_some() {
        Cell::from(Line::from(model_name_spans(
            Some(m.group.as_str()),
            &m.model,
            frame,
            effects_on,
            g,
        )))
    } else {
        let name_style = if active {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        Cell::from(Span::styled(m.model.clone(), name_style))
    };
    (group, name_cell)
}

/// Always-visible compact strip: the top models by total tokens, each with a
/// proportional mini-bar and req/tok/last-used (req12/28). Narrow terminals
/// drop the bar so the column set stays readable (req29).
fn draw_models_strip(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    now: SystemTime,
) {
    // Fill the pane: rows follow the AREA height (border + header take 2), so
    // a drag-resized strip (U8) shows more than the MODEL_STRIP_ROWS default
    // instead of 5 rows + dead space (UI-4 V2).
    let visible = (area.height.saturating_sub(2)) as usize;
    let rows_data: Vec<&ModelUsageDoc> = view.model_usage.iter().take(visible).collect();
    let shown = rows_data.len();
    let max_total = view
        .model_usage
        .iter()
        .map(model_total)
        .max()
        .unwrap_or(0)
        .max(1);
    let wide = area.width >= SIDE_BY_SIDE_AT;
    // The strip marker rides the SAME shared animation frame as the rest of the
    // board (TUI UI-6 item 2a) — it was frozen at 0 before, so in-flight strip
    // spinners never turned.
    let frame_n = ctx.frame;

    let rows = rows_data.into_iter().map(|m| {
        let active = m.in_flight > 0 || model_is_recent(m.last_used_ms, now);
        let (group_cell, name_cell) =
            model_name_cells(m, active, frame_n, view.tui_effects, view.gradient);
        let share = model_total(m) as f64 / max_total as f64;
        let mut cells = vec![
            Cell::from(model_active_marker(m, now, frame_n)),
            group_cell,
            name_cell,
        ];
        if wide {
            cells.push(Cell::from(Span::styled(
                format::gauge_bar(share, MODEL_BAR_WIDTH),
                group_color(Some(m.group.as_str())),
            )));
        }
        cells.push(Cell::from(format::human_count(m.requests)));
        cells.push(Cell::from(format::human_count(model_total(m))));
        // API-equivalent cost ($) cell, right after tok (item #4).
        cells.push(Cell::from(Span::styled(
            format_cost(model_cost(m)),
            Style::new().fg(Color::Green),
        )));
        let mut last = model_age_compact(m.last_used_ms, now);
        if m.in_flight > 0 {
            last = format!("{} in-flight", m.in_flight);
        }
        cells.push(Cell::from(Span::styled(last, dim())));
        Row::new(cells)
    });

    let (header, constraints): (Vec<&'static str>, Vec<Constraint>) = if wide {
        (
            vec!["", "group", "model", "share", "req", "tok", "$", "last"],
            vec![
                Constraint::Length(2),
                Constraint::Length(7),
                Constraint::Fill(1),
                Constraint::Length(MODEL_BAR_WIDTH as u16 + 1),
                Constraint::Length(7),
                Constraint::Length(8),
                Constraint::Length(9),
                Constraint::Length(12),
            ],
        )
    } else {
        (
            vec!["", "group", "model", "req", "tok", "$", "last"],
            vec![
                Constraint::Length(2),
                Constraint::Length(7),
                Constraint::Fill(1),
                Constraint::Length(7),
                Constraint::Length(8),
                Constraint::Length(9),
                Constraint::Length(12),
            ],
        )
    };
    // Scope qualifier (issue #62 S2, U22): wording comes from the document's
    // `data_quality` field (server-owned; canonical serde default for old
    // daemons) — same visible-qualifier contract as the heatmap's
    // "(best-effort)" title.
    let title = format!(
        " models — top {} of {} by tokens — {} (g: all) ",
        shown,
        view.model_usage.len(),
        view.data_quality.model_usage
    );
    let table = Table::new(rows, constraints)
        .header(Row::new(header).style(dim().add_modifier(Modifier::BOLD)))
        .block(Block::new().borders(Borders::TOP).title(title));
    frame.render_widget(table, area);
}

/// Detailed model view body: the full scrollable table beside (or above) the
/// drill-down panel for the cursored model row.
fn draw_models_full(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    chrome: &Chrome,
) {
    if view.model_usage.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "no model usage yet — send a request through the proxy",
            Style::new().fg(Color::Yellow),
        )))
        .block(Block::new().borders(Borders::TOP).title(" models "));
        frame.render_widget(empty, area);
        return;
    }
    if area.width >= SIDE_BY_SIDE_AT {
        let [table_area, detail_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(46)]).areas(area);
        draw_models_table(frame, table_area, view, ctx, chrome);
        draw_model_detail(frame, detail_area, view, ctx, chrome);
    } else {
        draw_models_table(frame, area, view, ctx, chrome);
    }
}

/// The full model table (all rows reachable via the cursor, req13). Columns
/// drop on narrow widths. The title shows the cursor position and total so it
/// is obvious more rows exist off-screen.
fn draw_models_table(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    chrome: &Chrome,
) {
    let now = ctx.now;
    let total = view.model_usage.len();
    let cursor = chrome.model_cursor.min(total.saturating_sub(1));
    let capacity = (area.height.saturating_sub(2) as usize).max(1); // border + header
    let start = if cursor >= capacity {
        cursor + 1 - capacity
    } else {
        0
    };
    let end = (start + capacity).min(total);
    let wide = area.width >= WIDE_TABLE_AT;

    let rows = view.model_usage[start..end]
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let idx = start + i;
            let active = m.in_flight > 0 || model_is_recent(m.last_used_ms, now);
            let (group_cell, name_cell) =
                model_name_cells(m, active, ctx.frame, view.tui_effects, view.gradient);
            let ok_err = Line::from(vec![
                Span::styled(format::human_count(m.ok), Style::new().fg(Color::Green)),
                Span::raw("/"),
                Span::styled(
                    format::human_count(m.errors),
                    if m.errors > 0 {
                        Style::new().fg(Color::Red)
                    } else {
                        dim()
                    },
                ),
            ]);
            let mut cells = vec![
                Cell::from(model_active_marker(m, now, ctx.frame)),
                group_cell,
                name_cell,
                Cell::from(format::human_count(m.requests)),
                Cell::from(ok_err),
                Cell::from(format::human_count(m.tokens_in)),
                Cell::from(format::human_count(m.tokens_out)),
                // API-equivalent cost ($) column, after out (item #4).
                Cell::from(Span::styled(
                    format_cost(model_cost(m)),
                    Style::new().fg(Color::Green),
                )),
            ];
            if wide {
                cells.push(Cell::from(Span::styled(opt_count(m.cache_read), dim())));
            }
            cells.push(Cell::from(Span::styled(
                model_age_compact(m.last_used_ms, now),
                dim(),
            )));
            cells.push(Cell::from(in_flight_span(m.in_flight)));
            let row = Row::new(cells);
            if idx == cursor {
                row.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                row
            }
        });

    let (header, constraints): (Vec<&'static str>, Vec<Constraint>) = if wide {
        (
            vec![
                "", "group", "model", "req", "ok/err", "in", "out", "$", "cache", "last", "if",
            ],
            vec![
                Constraint::Length(2),
                Constraint::Length(7),
                Constraint::Fill(1),
                Constraint::Length(7),
                Constraint::Length(9),
                Constraint::Length(8),
                Constraint::Length(8),
                Constraint::Length(9),
                Constraint::Length(8),
                Constraint::Length(8),
                Constraint::Length(3),
            ],
        )
    } else {
        (
            vec![
                "", "group", "model", "req", "ok/err", "in", "out", "$", "if",
            ],
            vec![
                Constraint::Length(2),
                Constraint::Length(7),
                Constraint::Fill(1),
                Constraint::Length(7),
                Constraint::Length(9),
                Constraint::Length(8),
                Constraint::Length(8),
                Constraint::Length(9),
                Constraint::Length(3),
            ],
        )
    };
    // Data-quality qualifiers (issue #62 S2) on the panel that owns the full
    // `$` column: the model-usage scope label (U22) and the cost qualifier
    // `$ ≈ …` (U20), both worded by the document's `data_quality` field
    // (server-owned; canonical serde default for old daemons) — same
    // visible-qualifier contract as the heatmap's "(best-effort)" title.
    let title = format!(
        " models — {} of {total} — {} — $ ≈ {} ",
        cursor + 1,
        view.data_quality.model_usage,
        view.data_quality.cost
    );
    let table = Table::new(rows, constraints)
        .header(Row::new(header).style(dim().add_modifier(Modifier::BOLD)))
        .block(Block::new().borders(Borders::TOP).title(title));
    frame.render_widget(table, area);
}

/// Drill-down panel for the cursored model row: token + cache split, account
/// breakdown (req19), effort (req18) and endpoint (req20) distributions.
fn draw_model_detail(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    ctx: &FrameCtx,
    chrome: &Chrome,
) {
    let now = ctx.now;
    let cursor = chrome
        .model_cursor
        .min(view.model_usage.len().saturating_sub(1));
    let Some(m) = view.model_usage.get(cursor) else {
        return;
    };
    let counts = |items: &[crate::dashboard::ModelCountDoc]| {
        if items.is_empty() {
            "—".to_string()
        } else {
            items
                .iter()
                .map(|c| format!("{}×{}", c.label, c.requests))
                .collect::<Vec<_>>()
                .join("  ")
        }
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} ", m.group.to_uppercase()),
            group_color(Some(m.group.as_str())).add_modifier(Modifier::BOLD),
        ),
        Span::styled(m.model.clone(), Style::new().add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" req   ", dim()),
        Span::raw(format!("{} (", format::human_count(m.requests))),
        Span::styled(
            format!("{} ok", format::human_count(m.ok)),
            Style::new().fg(Color::Green),
        ),
        Span::raw("/"),
        Span::styled(
            format!("{} err", format::human_count(m.errors)),
            if m.errors > 0 {
                Style::new().fg(Color::Red)
            } else {
                dim()
            },
        ),
        Span::raw(")"),
        Span::styled(
            if m.in_flight > 0 {
                format!(" · {} in-flight", m.in_flight)
            } else {
                String::new()
            },
            Style::new().fg(Color::Cyan),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" tok   ", dim()),
        Span::raw(format!(
            "in {} · out {}",
            format::human_count(m.tokens_in),
            format::human_count(m.tokens_out)
        )),
    ]));
    // Cache split — explicit "—" when the upstream did not report it (req9),
    // and a reminder that quota windows are account-level only (req27).
    lines.push(Line::from(vec![
        Span::styled(" cache ", dim()),
        Span::raw(format!(
            "read {} · creation {}",
            opt_count(m.cache_read),
            opt_count(m.cache_creation)
        )),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" last  ", dim()),
        Span::raw(model_age_compact(m.last_used_ms, now)),
        Span::styled(" ago", dim()),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" effort", dim()),
        Span::raw(format!(" {}", counts(&m.efforts))),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" route ", dim()),
        Span::raw(counts(&m.endpoints)),
    ]));
    lines.push(Line::from(Span::styled(" accounts", dim())));
    if m.accounts.is_empty() {
        lines.push(Line::from(Span::styled("   —", dim())));
    } else {
        for a in &m.accounts {
            lines.push(Line::from(vec![
                Span::raw(format!("   {} ", masked_name(&a.name, ctx.mask))),
                Span::styled(
                    format!(
                        "{} req · in {}/out {}",
                        format::human_count(a.requests),
                        format::human_count(a.tokens_in),
                        format::human_count(a.tokens_out),
                    ),
                    dim(),
                ),
            ]));
        }
    }
    // Quota windows are an account/provider fact, never per-model (req27) — make
    // that explicit so the per-account list above isn't read as a model limit.
    lines.push(Line::from(Span::styled(
        " quota is account-level (see accounts table)",
        dim(),
    )));

    let block = Block::new().borders(Borders::TOP).title(" model detail ");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The heatmap cells for `window`, or an empty slice when the doc carries no
/// slice for it (older daemon / no activity). Already sorted by tokens desc by
/// the document builder.
fn heatmap_cells(
    view: &DashboardView,
    window: super::activity::StatsWindow,
) -> &[crate::dashboard::WindowedCellDoc] {
    let label = window.label();
    view.windowed
        .iter()
        .find(|w| w.window == label)
        .map(|w| w.cells.as_slice())
        .unwrap_or(&[])
}

/// Windowed per-account/per-model token heatmap (issue #23). One row per
/// `(group, model, account)` cell over the selected trailing window, with a
/// token-intensity bar coloured by the cell's share of the busiest cell. The
/// numbers are a BEST-EFFORT sample — the activity event channel is lossy
/// (events are dropped on a full channel) — so the panel says so explicitly and
/// never presents them as an exact ledger.
fn draw_heatmap(
    frame: &mut Frame,
    area: Rect,
    view: &DashboardView,
    window: super::activity::StatsWindow,
) {
    if area.height == 0 {
        return;
    }
    let cells = heatmap_cells(view, window);
    let total = cells.len();
    let title = format!(" token heatmap — {} (best-effort) ", window.label());
    let block = Block::new().borders(Borders::TOP).title(title);

    let mut lines: Vec<Line> = Vec::new();
    // The accuracy contract (mandatory): a visible best-effort qualifier so the
    // windowed numbers are never read as exact accounting.
    lines.push(Line::from(Span::styled(
        " sampled from the activity feed — may undercount (lossy channel); w cycles 24h/72h",
        dim(),
    )));
    if total == 0 {
        lines.push(Line::from(Span::styled(
            " no windowed activity yet",
            Style::new().fg(Color::Yellow),
        )));
        frame.render_widget(Paragraph::new(lines).block(block), area);
        return;
    }

    // Column header.
    lines.push(Line::from(Span::styled(
        format!(
            " {:<7} {:<20} {:<14} {:>6} {:>8}  intensity",
            "group", "model", "account", "req", "tokens"
        ),
        dim().add_modifier(Modifier::BOLD),
    )));

    let max_tokens = cells.iter().map(|c| c.tokens).max().unwrap_or(0).max(1);
    let shown = total.min(HEATMAP_MAX_ROWS);
    for c in &cells[..shown] {
        let share = c.tokens as f64 / max_tokens as f64;
        let bar = format::gauge_bar(share, HEATMAP_BAR_WIDTH);
        let bar_color = level_color(format::gauge_level(share));
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:<7}", trunc(&c.group, 7)),
                group_color(Some(c.group.as_str())),
            ),
            Span::raw(format!(" {:<20}", trunc(&c.model, 20))),
            Span::styled(
                format!(
                    " {:<14}",
                    trunc(&masked_name(&c.account, view.email_anonymous), 14)
                ),
                dim(),
            ),
            Span::raw(format!(" {:>6}", format::human_count(c.requests))),
            Span::raw(format!(" {:>8}", format::human_count(c.tokens))),
            Span::raw("  "),
            Span::styled(bar, Style::new().fg(bar_color)),
        ]));
    }
    if total > shown {
        lines.push(Line::from(Span::styled(
            format!(" …{} more cell(s)", total - shown),
            dim(),
        )));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Truncate `s` to `width` display columns, appending `…` when clipped. Keeps
/// the heatmap columns aligned without depending on a unicode-width crate (the
/// model/account strings here are ASCII slugs/emails).
fn trunc(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else if width == 0 {
        String::new()
    } else {
        let keep: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{keep}…")
    }
}

fn draw_footer(frame: &mut Frame, area: Rect, chrome: &Chrome, mask: bool) {
    // Status messages quote account names ("switched to a@x.com") — mask any
    // embedded email under the email-anonymous setting.
    let status = Line::from(Span::styled(
        format!(
            " {}",
            masked_text(chrome.status_line.as_deref().unwrap_or(""), mask)
        ),
        Style::new().fg(Color::Yellow),
    ));
    let key = |k: &'static str| {
        Span::styled(k, Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD))
    };
    // Attach mode disables the config-mutation keys (a/r/R act on the server
    // host's config); the keybar shows what is actually available.
    let attached = chrome.attach.is_some();
    let keybar = match chrome.mode {
        // Config-editor prompts render their own inline hint line inside the
        // overlay; the keybar mirrors the essentials.
        Mode::ConfigEdit { .. } => Line::from(vec![
            Span::raw(" edit — "),
            key("Enter"),
            Span::raw(" apply  "),
            key("Esc"),
            Span::raw(" cancel"),
        ]),
        Mode::ConfigConfirm { .. } => Line::from(vec![
            Span::raw(" confirm — "),
            key("y"),
            Span::raw(" apply  "),
            key("n/Esc"),
            Span::raw(" cancel"),
        ]),
        // While a Mode interaction is pending it owns the keybar regardless of
        // which overlay summoned it (the interactions run within Accounts).
        Mode::Normal => match chrome.overlay {
            // MAIN: the tab bar owns surface navigation by click (UI-3 U6),
            // so the per-overlay summon keys are no longer advertised here
            // (UI-4 V4 — the `a`/`g`/`l`/`s`/`?`/`c` BINDINGS still work).
            // The `f`/`m`/`e` codex toggles are likewise covered by the
            // clickable group-settings bar (U9/U10), so their hint is gone
            // too (UI-4 V7).
            Overlay::None => {
                let mut spans = vec![Span::raw(" "), key("q"), Span::raw(" quit  ")];
                if attached {
                    spans.push(Span::styled("R disabled (attached)  ", dim()));
                } else {
                    spans.push(key("R"));
                    spans.push(Span::raw(" reload  "));
                }
                spans.extend([
                    key("u"),
                    Span::raw(" used/left  "),
                    key("t"),
                    Span::raw(" eta/utc  "),
                    key("S"),
                    Span::raw(" sched  "),
                    key("↑↓"),
                    Span::raw(" scroll"),
                ]);
                Line::from(spans)
            }
            // Accounts overlay: the issue #3/#4 affordances. a (add) and r
            // (remove) act on the DAEMON via the control endpoints, so they are
            // live in attach mode too.
            Overlay::Accounts => Line::from(vec![
                Span::raw(" accounts — "),
                key("s"),
                Span::raw(" switch  "),
                key("a"),
                Span::raw(" add  "),
                key("n"),
                Span::raw(" login  "),
                key("r"),
                Span::raw(" remove  "),
                key("u"),
                Span::raw(" used/left  "),
                key("t"),
                Span::raw(" eta/utc  "),
                key("Esc"),
                Span::raw(" back  "),
                key("q"),
                Span::raw(" quit"),
            ]),
            // Stats overlay: navigation + window cycle + back, regardless of
            // attach mode. `w` toggles the heatmap window (issue #23).
            Overlay::Stats => Line::from(vec![
                Span::raw(" stats — "),
                key("g/Esc"),
                Span::raw(" back  "),
                key("↑/k ↓/j"),
                Span::raw(" model  "),
                key("PgUp/PgDn"),
                Span::raw(" page  "),
                key("w"),
                Span::raw(" window  "),
                key("q"),
                Span::raw(" quit"),
            ]),
            // Usage overlay (usage-stats): granularity cycle + scroll + back.
            Overlay::Usage => Line::from(vec![
                Span::raw(" usage — "),
                key("g"),
                Span::raw(" hour/day/month  "),
                key("↑/k ↓/j"),
                Span::raw(" bucket  "),
                key("U/Esc"),
                Span::raw(" back  "),
                key("q"),
                Span::raw(" quit"),
            ]),
            // Logs overlay: full-screen tail; l/Esc back.
            Overlay::Logs => Line::from(vec![
                Span::raw(" logs — "),
                key("l/Esc"),
                Span::raw(" back  "),
                key("q"),
                Span::raw(" quit"),
            ]),
            Overlay::Perf => Line::from(vec![
                Span::raw(" perf — "),
                key("d"),
                Span::raw(" span  "),
                key("←/h →/l"),
                Span::raw(" day  "),
                key("↑/k ↓/j"),
                Span::raw(" series  "),
                key("p/Esc"),
                Span::raw(" back  "),
                key("q"),
                Span::raw(" quit"),
            ]),
            Overlay::Misc => Line::from(vec![
                Span::raw(" misc — "),
                key("?/Esc"),
                Span::raw(" back  "),
                key("q"),
                Span::raw(" quit"),
            ]),
            Overlay::Config => Line::from(vec![
                Span::raw(" config — "),
                key("c/Esc"),
                Span::raw(" back  "),
                key("q"),
                Span::raw(" quit"),
            ]),
            // Sessions overlay (issue #34): navigation + back.
            Overlay::Sessions => Line::from(vec![
                Span::raw(" sessions — "),
                key("s/Esc"),
                Span::raw(" back  "),
                key("↑/k ↓/j"),
                Span::raw(" session  "),
                key("PgUp/PgDn"),
                Span::raw(" page  "),
                key("q"),
                Span::raw(" quit"),
            ]),
        },
        Mode::Select { .. } => Line::from(vec![
            Span::raw(" "),
            key("↑/k ↓/j"),
            Span::raw(" move  "),
            key("Enter"),
            Span::raw(" switch  "),
            key("p"),
            Span::raw(" pause  "),
            key("L"),
            Span::raw(" limits  "),
            key("n"),
            Span::raw(" new login  "),
            key("Esc"),
            Span::raw(" cancel"),
        ]),
        // The typed key is shown ONLY as a masked width — never the raw
        // characters (AGENTS.md credential rule).
        Mode::EditLimits { .. } => Line::from(vec![
            Span::raw(" limits (5h,7d,fbl %): "),
            Span::styled(
                format!("{}▏", chrome.limits_input),
                Style::new().fg(Color::Cyan),
            ),
            Span::raw("  "),
            key("Enter"),
            Span::raw(" apply  "),
            key("Esc"),
            Span::raw(" cancel"),
        ]),
        Mode::AddKey => Line::from(vec![
            Span::raw(" add account — key: "),
            Span::styled(
                "•".repeat(chrome.add_input_len),
                Style::new().fg(Color::Cyan),
            ),
            Span::raw("  "),
            key("Enter"),
            Span::raw(" add  "),
            key("Esc"),
            Span::raw(" cancel"),
        ]),
        Mode::ContextMenu { .. } => Line::from(vec![
            Span::raw(" "),
            key("↑/k ↓/j"),
            Span::raw(" move  "),
            key("Enter/click"),
            Span::raw(" run  "),
            key("Esc"),
            Span::raw(" close"),
        ]),
        Mode::ConfirmRemove { .. } => Line::from(vec![
            Span::raw(" "),
            key("↑/k ↓/j"),
            Span::raw(" pick  "),
            Span::styled("remove selected? ", Style::new().fg(Color::Red)),
            key("y"),
            Span::raw(" confirm  "),
            key("Esc/n"),
            Span::raw(" cancel"),
        ]),
        Mode::NewLogin { idx } => {
            // Provider picker: the cursor row is shown highlighted; Enter
            // opens the browser for that provider.
            let mut spans = vec![Span::raw(" new login — ")];
            for (i, kind) in super::LoginKind::ALL.iter().enumerate() {
                let label = kind.label();
                if i == idx {
                    spans.push(Span::styled(
                        format!("[{label}]"),
                        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::styled(format!(" {label} "), dim()));
                }
                spans.push(Span::raw(" "));
            }
            spans.push(Span::raw(" "));
            spans.push(key("↑↓"));
            spans.push(Span::raw(" pick  "));
            spans.push(key("Enter"));
            spans.push(Span::raw(" open  "));
            spans.push(key("Esc"));
            spans.push(Span::raw(" cancel"));
            Line::from(spans)
        }
    };
    frame.render_widget(Paragraph::new(vec![status, keybar]), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::PoolSnapshot;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::{BTreeMap, HashMap};

    fn model_row(group: &str, model: &str, tokens_in: u64, tokens_out: u64) -> ModelUsageDoc {
        ModelUsageDoc {
            group: group.into(),
            model: model.into(),
            requests: 3,
            ok: 3,
            errors: 0,
            tokens_in,
            tokens_out,
            cache_read: Some(40_000),
            cache_creation: None,
            last_used_ms: 0,
            in_flight: 0,
            accounts: Vec::new(),
            efforts: Vec::new(),
            endpoints: Vec::new(),
            // Old-daemon default: tests exercise the local pricing fallback.
            cost_usd: 0.0,
        }
    }

    fn view_with(model_usage: Vec<ModelUsageDoc>) -> DashboardView {
        DashboardView {
            version: "llmux 0.0 (test)".into(),
            grok: Default::default(),
            daily_usage: Vec::new(),
            daily_perf: Vec::new(),
            config_facts: Default::default(),
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
                current: BTreeMap::new(),
                fable_current: BTreeMap::new(),
                manual_pin: Default::default(),
            },
            last_switch: None,
            poll_health: HashMap::new(),
            session_totals: HashMap::new(),
            global_totals: super::super::activity::Totals::default(),
            rpm_5m: 0.0,
            in_flight: Vec::new(),
            completed: Vec::new(),
            logs: Vec::new(),
            model_usage,
            client_usage: Vec::new(),
            windowed: Vec::new(),
            codex: crate::dashboard::CodexSettingsDoc::default(),
            email_anonymous: false,
            tui_effects: true,
            gradient: GradientCfg::default(),
            show_fable_weekly: true,
            domain_abbrev: crate::config::default_domain_abbrev(),
            quota_display: crate::config::QuotaDisplay::default(),
            data_quality: crate::dashboard::DataQualityDoc::default(),
            events: Vec::new(),
        }
    }

    /// An event banner active for a wide window around `now`, so a test only
    /// varies the field under scrutiny. `to` is compact local time.
    #[cfg(test)]
    fn banner(id: &str, from: &str, to: &str, content: &str) -> crate::config::EventBanner {
        crate::config::EventBanner {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            content: content.into(),
        }
    }

    #[test]
    fn event_banner_line_shows_only_active_events() {
        // Anchor `now` to a fixed instant so the compact (local-time) windows
        // resolve deterministically relative to it.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_780_000_000);
        // Absolute UTC bounds via RFC3339 so the test does not depend on the
        // machine's zone.
        let before = "2020-01-01T00:00:00Z";
        let mid_from = "2020-01-01T00:00:00Z";
        let far_to = "2100-01-01T00:00:00Z";

        // No events → no banner.
        assert!(event_banner_line(&[], now).is_none());

        // Before `from` (window entirely in the future) → hidden.
        let future = banner(
            "f",
            "2099-01-01T00:00:00Z",
            "2099-02-01T00:00:00Z",
            "future",
        );
        assert!(event_banner_line(std::slice::from_ref(&future), now).is_none());

        // After `to` (window entirely in the past) → hidden.
        let stale = banner("s", before, "2020-02-01T00:00:00Z", "stale");
        assert!(event_banner_line(std::slice::from_ref(&stale), now).is_none());

        // Active (`from <= now < to`) → shown, content in the line.
        let active = banner("a", mid_from, far_to, "Fable 5 Available until 7/12");
        let line = event_banner_line(std::slice::from_ref(&active), now).expect("active shown");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("Fable 5 Available until 7/12"),
            "content rendered: {text}"
        );
        assert!(text.contains("until "), "deadline label present: {text}");
        assert!(text.contains("left"), "countdown present: {text}");
    }

    #[test]
    fn event_banner_line_picks_earliest_active_to() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_780_000_000);
        let from = "2020-01-01T00:00:00Z";
        // Two active events; the one with the EARLIER `to` wins.
        let later = banner("late", from, "2100-01-01T00:00:00Z", "later end");
        let sooner = banner("soon", from, "2099-01-01T00:00:00Z", "sooner end");
        let line = event_banner_line(&[later, sooner], now).expect("one active");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("sooner end"), "earliest-to wins: {text}");
        assert!(!text.contains("later end"), "other event hidden: {text}");
    }

    /// Z 2026-07-16: with no active banner the top chrome is 2 rows (header +
    /// tabs), but the overlay reserve was a fixed 3 — MAIN's accounts-table
    /// border (` accounts ────`) leaked through as a STALE separator right
    /// under the tab bar on every overlay tab. The overlay must own the row
    /// directly below the tabs.
    #[test]
    fn overlay_owns_the_row_under_the_tab_bar_when_no_banner_is_active() {
        let view = view_with(Vec::new());
        let rows = render_rows(&view, &chrome_overlay(Overlay::Usage), 160, 30);
        assert!(
            rows[1].contains("dashboard"),
            "tab bar on row 1 with no banner:\n{}",
            rows[1]
        );
        assert!(
            !rows[2].contains("accounts"),
            "MAIN's accounts border must not leak under the tab bar:\n{}",
            rows[2]
        );
        assert!(
            rows[2].contains("usage"),
            "the usage overlay's own titled border owns the row under the tabs:\n{}",
            rows[2]
        );
    }

    /// Counterpart: while a banner IS active it stays visible above the
    /// header, and the overlay starts right under the (shifted) tab bar.
    #[test]
    fn overlay_reserves_the_banner_row_only_while_one_is_active() {
        let mut view = view_with(Vec::new());
        view.events = vec![banner(
            "evt",
            "2020-01-01T00:00:00Z",
            "2100-01-01T00:00:00Z",
            "maintenance window",
        )];
        let rows = render_rows(&view, &chrome_overlay(Overlay::Usage), 160, 30);
        assert!(
            rows[0].contains("maintenance window"),
            "banner pinned on top:\n{}",
            rows[0]
        );
        assert!(rows[2].contains("dashboard"), "tab bar row:\n{}", rows[2]);
        assert!(
            rows[3].contains("usage") && !rows[3].contains("accounts"),
            "overlay starts right under the tabs:\n{}",
            rows[3]
        );
    }

    /// Z 2026-07-16: the accounts overlay used to stack THREE "accounts" rows
    /// (leaked MAIN border + its own bold header line + the table's titled
    /// border). The table's titled border is the one and only separator.
    #[test]
    fn accounts_overlay_renders_exactly_one_accounts_separator() {
        let view = view_with(Vec::new());
        let rows = render_rows(&view, &chrome_overlay(Overlay::Accounts), 160, 30);
        // A separator row is the titled top border (` accounts ────`) — the
        // label followed by the border line. Hint TEXT mentioning the word
        // ("no accounts — run `llmux login`…") is not a separator.
        let hits: Vec<usize> = (2..rows.len() - 2)
            .filter(|&y| rows[y].contains("accounts ─"))
            .collect();
        assert_eq!(
            hits,
            vec![2],
            "one accounts separator, directly under the tabs:\n{}",
            rows[..6].join("\n")
        );
    }

    #[test]
    fn row_account_name_strips_group_prefix_and_abbreviates_domain() {
        let abbrev = crate::config::default_domain_abbrev();
        assert_eq!(
            row_account_name("claude:ai3@insightquest.io", false, &abbrev),
            "ai3@iq.io"
        );
        assert_eq!(
            row_account_name("codex:ai@insightquest.io", false, &abbrev),
            "ai@iq.io"
        );
        // Unmapped domain: prefix still stripped, domain untouched.
        assert_eq!(
            row_account_name("claude:x@example.com", false, &abbrev),
            "x@example.com"
        );
        // No prefix / no email shape: passes through.
        assert_eq!(row_account_name("plainname", false, &abbrev), "plainname");
    }

    #[test]
    fn quota_bar_line_is_cell_width_and_splits_at_fill_boundary() {
        let line = quota_bar_line(0.5, Color::Green, "7d 10h", 2, 16, None);
        let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(total, 16, "cell is exactly bar-width wide");
        // First 8 columns reversed (the fill), the rest plain.
        let mut col = 0usize;
        for span in &line.spans {
            let rev = span.style.add_modifier.contains(Modifier::REVERSED);
            for _ in span.content.chars() {
                assert_eq!(rev, col < 8, "column {col} reversal");
                col += 1;
            }
        }
    }

    #[test]
    fn quota_bar_time_position_is_fixed_regardless_of_marker() {
        // Z 2026-07-15: the freshness marker (○/◑) used to be APPENDED to the
        // centered time, shifting the time sideways whenever the marker came
        // and went. Now the time centers identically and the marker owns the
        // fixed last column.
        let flat = |line: ratatui::text::Line| -> String {
            line.spans.iter().map(|s| s.content.as_ref()).collect()
        };
        let without = flat(quota_bar_line(0.5, Color::Green, "7d 10h", 2, 16, None));
        let with = flat(quota_bar_line(
            0.5,
            Color::Green,
            "7d 10h",
            2,
            16,
            Some('○'),
        ));
        assert_eq!(
            without.find("7d 10h"),
            with.find("7d 10h"),
            "time column must not move when the marker appears"
        );
        assert_eq!(
            with.chars().last(),
            Some('○'),
            "marker pinned to last column"
        );
        assert_eq!(with.chars().count(), 16);
    }

    #[test]
    fn quota_bar_line_empty_and_full_fill() {
        for (fill, want_rev) in [(0.0, 0usize), (1.0, 16usize)] {
            let line = quota_bar_line(fill, Color::Red, "17m 35s", 3, 16, None);
            let rev: usize = line
                .spans
                .iter()
                .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
                .map(|s| s.content.chars().count())
                .sum();
            assert_eq!(rev, want_rev, "fill {fill}");
        }
    }

    #[test]
    fn quota_bar_line_bolds_only_the_leading_unit() {
        let line = quota_bar_line(0.0, Color::Green, "7d 10h", 2, 16, None);
        let bold: String = line
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(bold, "7d", "emphasis covers the larger unit only");
    }

    /// Chrome with a given overlay active and `Mode::Normal` (issue #5). The
    /// old `chrome(show_models)` builder mapped `true`→Stats; tests now name the
    /// overlay explicitly.
    fn chrome_overlay(overlay: Overlay) -> Chrome {
        Chrome {
            pane_heights: Default::default(),
            menu_anchor: None,
            menu_account: None,
            chart_days: 14,
            perf_days: 14,
            perf_cursor: 0,
            perf_day_off: None,
            usage_gran: Default::default(),
            usage_scroll: 0,
            input_modal: None,
            raw_modal: None,
            frame: 0,
            mode: Mode::Normal,
            overlay,
            status_line: None,
            activity_scroll: 0,
            expanded_activity: None,
            expanded_run: None,
            model_cursor: 0,
            stats_window: super::super::activity::StatsWindow::default(),
            sessions: Vec::new(),
            sessions_loading: false,
            sessions_pct: 100,
            session_cursor: 0,
            session_sort: Default::default(),
            config_cursor: 0,
            config_input: String::new(),
            config_saved: Default::default(),
            add_input_len: 0,
            quota_display_override: None,
            reset_absolute: false,
            limits_input: String::new(),
            attach: None,
        }
    }

    fn render(view: &DashboardView, chrome: &Chrome, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        let mut hits = None;
        terminal
            .draw(|f| draw(f, Some(view), chrome, &mut hits))
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// Render to a vector of screen rows (one `String` per terminal line), so a
    /// test can compare body rows while ignoring the rows that legitimately
    /// differ between local and attach mode (the header attach banner + the
    /// footer keybar). Used by the issue #5 local-vs-attach parity test.
    fn render_rows(view: &DashboardView, chrome: &Chrome, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        let mut hits = None;
        terminal
            .draw(|f| draw(f, Some(view), chrome, &mut hits))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect()
    }

    /// Attach-mode chrome (issue #5 parity): same as [`chrome_overlay`] but with
    /// an attach banner, so a test can prove the data render path is NOT forked
    /// by where the document came from.
    fn chrome_attach(overlay: Overlay) -> Chrome {
        Chrome {
            attach: Some(super::super::Attach {
                pid: Some(61282),
                connected: true,
            }),
            ..chrome_overlay(overlay)
        }
    }

    #[test]
    fn compact_strip_shows_top_model_and_keybar_advertises_view() {
        let view = view_with(vec![model_row("codex", "gpt-5.5", 700, 300)]);
        // MAIN (overlay=None): the compact strip is part of MAIN and the keybar
        // advertises the stats overlay shortcut (req12/req30, adapted to #5).
        let text = render(&view, &chrome_overlay(Overlay::None), 160, 30);
        assert!(
            text.contains("stats"),
            "keybar advertises the stats overlay"
        );
        assert!(text.contains("gpt-5.5"), "strip shows the top model");
    }

    #[test]
    fn detailed_view_lists_all_model_rows_and_drilldown() {
        let view = view_with(vec![
            model_row("codex", "gpt-5.5", 700, 300),
            model_row("claude", "claude-sonnet-4-5", 100, 50),
        ]);
        // The Stats overlay (was the `show_models` full view) still lists all
        // model rows + the drill-down (req13).
        let text = render(&view, &chrome_overlay(Overlay::Stats), 160, 30);
        assert!(text.contains("gpt-5.5"));
        assert!(
            text.contains("claude-sonnet-4-5"),
            "lower rows reachable (req13)"
        );
        assert!(text.contains("model detail"), "drill-down panel present");
    }

    // --- issue #5: MAIN-always + summoned overlays -------------------------

    /// MAIN (overlay=None) shows in-flight + account quota + the model strip,
    /// with NO navigation/overlay surface drawn.
    #[test]
    fn main_shows_inflight_quota_and_strip_without_overlay() {
        let mut view = view_with(vec![model_row("codex", "gpt-5.5", 700, 300)]);
        view.in_flight = vec![super::super::activity::InFlight {
            id: 7,
            method: "POST".into(),
            path: "/v1/messages".into(),
            account: Some("claude:me@example.com".into()),
            group: Some("claude".into()),
            model: Some("claude-opus-4-8".into()),
            effort: None,
            fast: false,
            kind: None,
            started_at: std::time::SystemTime::UNIX_EPOCH,
        }];
        let text = render(&view, &chrome_overlay(Overlay::None), 160, 30);
        assert!(
            text.contains("opus-4-8"),
            "MAIN shows the in-flight session"
        );
        assert!(text.contains("gpt-5.5"), "MAIN shows the model strip");
        // No overlay chrome on MAIN: the Stats drill-down panel is absent.
        assert!(
            !text.contains("model detail"),
            "MAIN draws no overlay surface"
        );
    }

    /// The Stats overlay still renders MAIN underneath (the model strip stays
    /// visible), proving MAIN is drawn first every frame.
    #[test]
    fn stats_overlay_keeps_main_underneath() {
        let view = view_with(vec![model_row("codex", "gpt-5.5", 700, 300)]);
        let text = render(&view, &chrome_overlay(Overlay::Stats), 160, 30);
        assert!(text.contains("model detail"), "stats overlay drawn on top");
        assert!(
            text.contains("gpt-5.5"),
            "MAIN model data still visible underneath the overlay"
        );
    }

    /// One usage row for the Usage-overlay render tests (usage-stats).
    #[allow(clippy::too_many_arguments)]
    fn usage_row(
        gran: &str,
        bucket: u64,
        label: &str,
        group: &str,
        model: &str,
        tokens_in: u64,
        tokens_out: u64,
        cost_usd: f64,
    ) -> crate::dashboard::UsageStatDoc {
        crate::dashboard::UsageStatDoc {
            gran: gran.into(),
            bucket,
            label: label.into(),
            group: group.into(),
            model: model.into(),
            requests: 3,
            tokens_in,
            tokens_out,
            cache_read: 40,
            cache_creation: 4,
            cost_usd,
            priced: true,
        }
    }

    /// The Usage overlay (usage-stats) renders the selected granularity's
    /// buckets: a bold per-bucket total row (label + "N models" + summed
    /// counters + summed cost) above per-model rows, and the period totals in
    /// the title. Rows of OTHER granularities never leak into the table.
    #[test]
    fn usage_overlay_renders_buckets_models_and_costs() {
        let mut view = view_with(vec![]);
        view.usage_stats = vec![
            usage_row(
                "day",
                20_000,
                "2024-10-04",
                "claude",
                "opus-x",
                1_000,
                200,
                1.25,
            ),
            usage_row(
                "day",
                20_000,
                "2024-10-04",
                "codex",
                "gpt-5.5",
                500,
                100,
                0.50,
            ),
            usage_row(
                "day",
                19_999,
                "2024-10-03",
                "claude",
                "opus-x",
                900,
                90,
                0.75,
            ),
            // An hourly row that must NOT appear in the daily table.
            usage_row("hour", 480_000, "10-04 09h", "claude", "opus-x", 7, 7, 0.01),
            // An UNPRICED row (review R1 MUST-FIX 3): its zero cost must
            // render as `—`, and both its bucket row and the title carry a
            // marker so the totals can't read as "all traffic priced".
            {
                let mut r = usage_row("day", 19_999, "2024-10-03", "misc", "mystery", 11, 2, 0.0);
                r.priced = false;
                r
            },
            // A thousands-magnitude bucket (LAST so day rows stay bucket-
            // adjacent, as the document guarantees): the ledger column must
            // render a comma separator AND keep its decimal point on the
            // same column as the small amounts (usage-cost ledger polish).
            usage_row(
                "day",
                19_998,
                "2024-10-02",
                "claude",
                "opus-x",
                10,
                10,
                1_234.499,
            ),
        ];
        let text = render(&view, &chrome_overlay(Overlay::Usage), 160, 30);
        assert!(text.contains("usage — daily"), "title shows granularity");
        assert!(text.contains("3 buckets"), "title counts day buckets only");
        assert!(text.contains("$1237.00"), "title sums the day costs");
        assert!(
            text.contains("(+unpriced)"),
            "title qualifies unpriced usage"
        );
        assert!(text.contains("2024-10-04"), "bucket label rendered");
        assert!(text.contains("2 models"), "bucket summary row rendered");
        assert!(text.contains("claude/opus-x"), "model row rendered");
        assert!(text.contains("codex/gpt-5.5"), "second model row rendered");
        assert!(text.contains("$1.25"), "per-model cost rendered");
        assert!(
            text.contains("$0.7500+?"),
            "bucket with unpriced rows marked"
        );
        assert!(text.contains("misc/mystery"), "unpriced model row rendered");
        assert!(!text.contains("10-04 09h"), "hourly rows stay out of daily");

        // Ledger polish (usage-cost): thousands separator + the decimal
        // point anchored to ONE column for every amount, whatever its
        // magnitude — and the unpriced dash sits in the same column.
        assert!(text.contains("$1,234.50"), "thousands separator rendered");
        let dot_col = |needle: &str| {
            let line = text
                .lines()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("line with {needle:?}"));
            line.char_indices()
                .filter(|(_, c)| *c == '.')
                .map(|(i, _)| i)
                .next_back()
                .unwrap_or_else(|| panic!("cost dot in {needle:?} line"))
        };
        let small = dot_col("codex/gpt-5.5"); // $0.50 detail row
        assert_eq!(dot_col("$1.25"), small, "detail amounts align");
        assert_eq!(dot_col("$1,234.50"), small, "comma amount aligns");
        assert_eq!(dot_col("$0.7500+?"), small, "marker doesn't shift the dot");
    }

    /// Ledger number splitting (usage-cost polish): carry-safe rounding,
    /// the sub-dollar 4-digit fraction, and thousands grouping.
    #[test]
    fn usage_cost_parts_carry_grouping_and_subdollar_precision() {
        assert_eq!(usage_cost_parts(0.99999), ("1".into(), ".00".into()));
        assert_eq!(usage_cost_parts(0.0076), ("0".into(), ".0076".into()));
        assert_eq!(usage_cost_parts(0.0), ("0".into(), ".0000".into()));
        assert_eq!(usage_cost_parts(1.25), ("1".into(), ".25".into()));
        assert_eq!(usage_cost_parts(1_234.499), ("1,234".into(), ".50".into()));
        assert_eq!(
            usage_cost_parts(1_234_567.891),
            ("1,234,567".into(), ".89".into())
        );
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000), "1,000");
    }

    /// An invalid amount (negative rate via a pathological config `pricing`
    /// override, or non-finite) must render the honesty dash, never a
    /// credible-looking $0 — and in a MIXED bucket it must not reduce the
    /// bucket/title totals, which instead carry the `+?` / `(+unpriced)`
    /// qualifiers (review R2 MUST-FIX: validated per component, before
    /// aggregation).
    #[test]
    fn usage_overlay_invalid_cost_renders_dash_and_qualifies_totals() {
        let mut view = view_with(vec![]);
        let mut neg = usage_row("day", 20_000, "2024-10-04", "claude", "neg-x", 10, 5, -0.42);
        neg.priced = true;
        view.usage_stats = vec![
            usage_row("day", 20_000, "2024-10-04", "claude", "ok-x", 20, 10, 1.0),
            neg,
        ];
        let text = render(&view, &chrome_overlay(Overlay::Usage), 160, 30);
        assert!(text.contains("claude/neg-x"), "invalid row rendered");
        assert!(
            !text.contains("$0.0000") && !text.contains("$0.58"),
            "negative component neither dresses up as free nor nets the sum"
        );
        assert!(
            text.contains("$1.00+?"),
            "bucket total = valid components only, qualified"
        );
        assert!(
            text.contains("(+unpriced)"),
            "title total qualified by the invalid component"
        );
        let model_line = text
            .lines()
            .find(|l| l.contains("claude/neg-x"))
            .expect("model line");
        assert!(model_line.contains('—'), "invalid cost renders the dash");
    }

    /// The ledger style tiers survive rendering (review R2 nice-to-have —
    /// the one-time ANSI capture can't catch regressions): `$` at the
    /// quietest tier on both rows; total-row integer digits at full
    /// strength with a DarkGray fraction; detail-row integer digits
    /// DarkGray with a DIM fraction.
    #[test]
    fn usage_cost_cells_carry_style_tiers() {
        let mut view = view_with(vec![]);
        view.usage_stats = vec![usage_row(
            "day",
            20_000,
            "2024-10-04",
            "claude",
            "opus-x",
            1_000,
            200,
            994.99,
        )];
        let chrome = chrome_overlay(Overlay::Usage);
        let mut terminal = Terminal::new(TestBackend::new(160, 30)).expect("terminal");
        let mut hits = None;
        terminal
            .draw(|f| draw(f, Some(&view), &chrome, &mut hits))
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        let w = 160usize;
        let text_row = |y: usize| {
            (0..w)
                .map(|x| buf.content()[y * w + x].symbol())
                .collect::<String>()
        };
        let style_at = |x: usize, y: usize| buf.content()[y * w + x].style();
        let mut total_y = None;
        let mut detail_y = None;
        for y in 0..30 {
            let t = text_row(y);
            if t.contains("1 models") {
                total_y = Some(y);
            }
            if t.contains("claude/opus-x") {
                detail_y = Some(y);
            }
        }
        let (total_y, detail_y) = (total_y.expect("total row"), detail_y.expect("detail row"));
        // These rows are pure ASCII, so byte columns == cell columns.
        let dollar = |y: usize| text_row(y).find('$').expect("dollar column");
        let dot = |y: usize| text_row(y).rfind('.').expect("cost dot");

        for y in [total_y, detail_y] {
            let s = style_at(dollar(y), y);
            assert_eq!(s.fg, Some(Color::DarkGray), "$ color, row {y}");
            assert!(s.add_modifier.contains(Modifier::DIM), "$ dim, row {y}");
        }
        let t_int = style_at(dollar(total_y) + 1, total_y);
        assert_ne!(
            t_int.fg,
            Some(Color::DarkGray),
            "total integer digits at full strength"
        );
        let t_frac = style_at(dot(total_y) + 1, total_y);
        assert_eq!(t_frac.fg, Some(Color::DarkGray), "total fraction darker");
        assert!(!t_frac.add_modifier.contains(Modifier::DIM));
        let d_int = style_at(dollar(detail_y) + 1, detail_y);
        assert_eq!(
            d_int.fg,
            Some(Color::DarkGray),
            "detail integer a tier down"
        );
        assert!(!d_int.add_modifier.contains(Modifier::DIM));
        let d_frac = style_at(dot(detail_y) + 1, detail_y);
        assert_eq!(d_frac.fg, Some(Color::DarkGray), "detail fraction darkest");
        assert!(
            d_frac.add_modifier.contains(Modifier::DIM),
            "detail fraction dim"
        );
    }

    /// With no usage rows for the selected granularity the overlay shows the
    /// empty hint (also the shape an OLDER daemon's document renders as).
    #[test]
    fn usage_overlay_empty_shows_hint() {
        let view = view_with(vec![]);
        let text = render(&view, &chrome_overlay(Overlay::Usage), 160, 30);
        assert!(text.contains("no usage history yet"));
    }

    /// When only the SELECTED granularity is empty (idle daemon: hourly
    /// window drained, daily/monthly still populated) the hint must say so —
    /// not claim the whole history is missing (review CR).
    #[test]
    fn usage_overlay_gran_empty_hints_granularity_switch() {
        let mut view = view_with(vec![]);
        view.usage_stats = vec![usage_row(
            "day",
            20_000,
            "2024-10-04",
            "claude",
            "opus-x",
            1_000,
            200,
            1.25,
        )];
        let mut chrome = chrome_overlay(Overlay::Usage);
        chrome.usage_gran = crate::tui::activity::UsageGran::Hour;
        let text = render(&view, &chrome, 160, 30);
        assert!(text.contains("no buckets in this granularity"));
        assert!(!text.contains("no usage history yet"));
    }

    /// The Stats overlay's windowed heatmap (issue #23) renders the selected
    /// window's cells AND a visible best-effort qualifier (accuracy contract).
    #[test]
    fn stats_overlay_renders_windowed_heatmap_with_best_effort_label() {
        let mut view = view_with(vec![model_row("codex", "gpt-5.5", 700, 300)]);
        view.windowed = vec![
            crate::dashboard::WindowedStatsDoc {
                window: "24h".into(),
                window_secs: 86_400,
                cells: vec![crate::dashboard::WindowedCellDoc {
                    group: "codex".into(),
                    model: "gpt-5.5".into(),
                    account: "user@example.com".into(),
                    requests: 12,
                    ok: 11,
                    errors: 1,
                    tokens_in: 700,
                    tokens_out: 300,
                    cache_read: 120,
                    cache_creation: 0,
                    tokens: 1_120,
                }],
            },
            crate::dashboard::WindowedStatsDoc {
                window: "72h".into(),
                window_secs: 259_200,
                cells: Vec::new(),
            },
        ];
        let text = render(&view, &chrome_overlay(Overlay::Stats), 160, 40);
        assert!(text.contains("heatmap"), "heatmap panel titled");
        assert!(
            text.contains("best-effort"),
            "accuracy contract: best-effort qualifier visible"
        );
        // The account column is truncated via `trunc(&c.account, 14)`, so the
        // 16-char `user@example.com` renders as `user@example.…`; assert on the
        // visible prefix rather than the full address.
        assert!(text.contains("user@example"), "per-account axis rendered");
        // The keybar advertises the window-cycle key.
        assert!(text.contains("window"), "footer advertises w window cycle");
    }

    /// The models surfaces render the data-quality qualifiers from the doc's
    /// `data_quality` field (issue #62 S2): the model-usage scope label on
    /// the strip and full-table titles (U22) and the `$ ≈ …` cost qualifier
    /// on the panel owning the full `$` column (U20) — the same
    /// visible-qualifier contract as the heatmap's best-effort title above.
    #[test]
    fn models_surfaces_render_data_quality_scope_and_cost_labels() {
        let view = view_with(vec![model_row("codex", "gpt-5.5", 700, 300)]);
        // MAIN (overlay=None): the always-visible strip title carries the
        // scope label.
        let text = render(&view, &chrome_overlay(Overlay::None), 160, 30);
        assert!(
            text.contains("hydrated activity/runtime"),
            "strip title carries the model-usage scope label (U22)"
        );
        // Stats overlay: the full models table title carries scope + cost.
        let text = render(&view, &chrome_overlay(Overlay::Stats), 160, 40);
        assert!(
            text.contains("hydrated activity/runtime"),
            "models table title carries the scope label (U22)"
        );
        assert!(
            text.contains("$ ≈ API-equivalent estimate"),
            "cost qualifier rendered on the $-column panel title (U20)"
        );
    }

    /// The Logs overlay shows the log tail.
    #[test]
    fn logs_overlay_shows_the_log_tail() {
        let mut view = view_with(Vec::new());
        view.logs = vec![crate::logging::LogLine {
            level: tracing::Level::INFO,
            text: "proxy started on :3456".into(),
        }];
        let text = render(&view, &chrome_overlay(Overlay::Logs), 160, 30);
        assert!(text.contains("logs"), "logs overlay titled");
        assert!(
            text.contains("proxy started on :3456"),
            "logs overlay shows the tail"
        );
    }

    /// Sessions overlay (issue #34): renders the folded session list with the
    /// confidence label, the user_id, and the per-session aggregates.
    #[test]
    fn sessions_overlay_shows_session_rows_with_confidence_label() {
        use crate::session::{Confidence, Session};
        let view = view_with(Vec::new());
        let mut chrome = chrome_overlay(Overlay::Sessions);
        chrome.sessions = vec![
            Session {
                user_id: Some("u-active".into()),
                requests: 12,
                tokens_in: 3400,
                tokens_out: 1200,
                models: vec!["claude-sonnet-4".into(), "claude-opus-4".into()],
                accounts: vec!["acct-a".into(), "acct-b".into()],
                account_rotations: 3,
                first_ms: 1_000_000,
                last_ms: 1_600_000,
                duration_ms_sum: 0,
                timed_requests: 0,
                tokens_out_timed: 0,
                confidence: Confidence::High,
            },
            Session {
                user_id: None,
                requests: 1,
                tokens_in: 0,
                tokens_out: 0,
                models: vec![],
                accounts: vec!["acct-c".into()],
                account_rotations: 0,
                first_ms: 2_000_000,
                last_ms: 2_000_000,
                duration_ms_sum: 0,
                timed_requests: 0,
                tokens_out_timed: 0,
                confidence: Confidence::Low,
            },
        ];
        let text = render(&view, &chrome, 160, 30);
        assert!(text.contains("sessions"), "sessions overlay titled");
        assert!(text.contains("u-active"), "shows the user_id grouping key");
        assert!(text.contains("high"), "shows the High confidence label");
        assert!(
            text.contains("low") || text.contains("(ungrouped)"),
            "shows the ungrouped Low bucket"
        );
    }

    /// An empty timeline (no captured raw-io) renders the hint, not a crash.
    #[test]
    fn sessions_overlay_empty_shows_hint() {
        let view = view_with(Vec::new());
        let chrome = chrome_overlay(Overlay::Sessions);
        let text = render(&view, &chrome, 160, 30);
        assert!(text.contains("no sessions yet"), "empty hint shown");
    }

    /// The full-screen spinner shows ONLY while loading AND no partial has
    /// arrived yet (empty `sessions`) — never the empty "no sessions yet" hint —
    /// so the user sees progress instead of a frozen/empty screen. Once partials
    /// land the table takes over (see the two tests below).
    #[test]
    fn sessions_overlay_loading_shows_spinner_only_when_empty() {
        let view = view_with(Vec::new());
        let mut chrome = chrome_overlay(Overlay::Sessions);
        chrome.sessions_loading = true;
        let text = render(&view, &chrome, 160, 30);
        assert!(text.contains("loading sessions"), "loading indicator shown");
        assert!(
            !text.contains("no sessions yet"),
            "empty hint suppressed while loading"
        );
    }

    /// A single non-empty session for the progressive-load render tests.
    fn one_session() -> crate::session::Session {
        crate::session::Session {
            user_id: Some("u-active".into()),
            requests: 12,
            tokens_in: 3400,
            tokens_out: 1200,
            models: vec!["claude-sonnet-4".into()],
            accounts: vec!["acct-a".into()],
            account_rotations: 0,
            first_ms: 1_000_000,
            last_ms: 1_600_000,
            duration_ms_sum: 0,
            timed_requests: 0,
            tokens_out_timed: 0,
            confidence: crate::session::Confidence::High,
        }
    }

    /// A progressive partial (loading is still true but `sessions` is non-empty)
    /// renders the TABLE — not the spinner — with the `loading… N%` progress in
    /// the title, so the user watches the timeline fill in.
    #[test]
    fn sessions_overlay_partial_renders_table_with_loading_title() {
        let view = view_with(Vec::new());
        let mut chrome = chrome_overlay(Overlay::Sessions);
        chrome.sessions_loading = true;
        chrome.sessions_pct = 42;
        chrome.sessions = vec![one_session()];
        let text = render(&view, &chrome, 160, 30);
        assert!(
            text.contains("loading… 42%"),
            "table title carries the read progress"
        );
        assert!(text.contains("u-active"), "table (not spinner) is rendered");
        assert!(
            !text.contains("loading sessions"),
            "full-screen spinner suppressed once a partial has arrived"
        );
    }

    /// The final delivery clears the loading state: the table renders WITHOUT the
    /// `loading…` title suffix.
    #[test]
    fn sessions_overlay_final_delivery_clears_loading_title() {
        let view = view_with(Vec::new());
        let mut chrome = chrome_overlay(Overlay::Sessions);
        chrome.sessions_loading = false;
        chrome.sessions = vec![one_session()];
        let text = render(&view, &chrome, 160, 30);
        assert!(text.contains("u-active"), "table rendered");
        assert!(
            !text.contains("loading…"),
            "loading title gone once the load is done"
        );
    }

    /// Issue #5 acceptance: local and attach render IDENTICALLY from the same
    /// `DashboardDoc`. The view-model both modes feed `draw` is built by the one
    /// `DashboardView::from_doc` contract (local: from an in-process doc;
    /// attach: from the fetched JSON), so the body — account table, model strip,
    /// in-flight/activity, AND the summoned overlay drawn over MAIN — must match
    /// byte-for-byte. Only the header attach banner and the footer keybar
    /// legitimately differ (deliberate chrome, not a fork of the data render),
    /// so those rows are excluded from the comparison.
    ///
    /// Asserting parity for BOTH `Overlay::None` and an open overlay proves the
    /// overlay layer is also drawn from the shared view, not re-derived per
    /// backend.
    fn parity_doc() -> crate::dashboard::DashboardDoc {
        serde_json::from_value(serde_json::json!({
            "version": "llmux 0.1.0 (dev dev)",
            "pid": 61282,
            "uptime_secs": 7980,
            "port": 3456,
            "current": "a",
            "upstream": "https://api.anthropic.com",
            "config_path": "/home/u/.config/llmux/llmux.json",
            "select_params": { "five_hour_max": 0.90, "seven_day_max": 0.99, "usage_max_age_secs": 600 },
            "refresh_ahead_secs": 25200,
            "evaluate_tick_secs": 60,
            "accounts": [
                {
                    "name": "a", "type": "oauth", "status": "active", "order": 1,
                    "blocked": null, "healthy": true,
                    "five_hour": { "utilization": 0.42, "resets_at": 1_003_600u64,
                                   "resets_in_secs": 3600, "fetched_at_ms": 1_000_000_000u64,
                                   "source": "headers" },
                    "seven_day": null,
                    "cooldown_until": null, "cooldown_source": null,
                    "in_flight": 0,
                    "token_expires_at_ms": 1_003_600_000u64, "last_refresh_ms": 999_820_000u64,
                    "totals": { "requests": 3, "input_tokens": 100, "output_tokens": 50 },
                    "session": { "requests": 3, "ok": 2, "errors": 1, "tokens_in": 100, "tokens_out": 50 },
                },
            ],
            "scheduler": {
                "last_switch": { "from": null, "to": "a", "reason": "initial selection",
                                 "at_ms": 999_910_000u64 },
                "next_in_line": null,
                "next_eval_in_secs": 42,
            },
            "poller": [],
            "totals": { "requests": 3, "ok": 2, "errors": 1, "tokens_in": 100,
                        "tokens_out": 50, "rpm_5m": 0.6, "in_flight": 0 },
            "model_usage": [
                { "group": "codex", "model": "gpt-5.5", "requests": 3,
                  "ok": 3, "errors": 0, "tokens_in": 700, "tokens_out": 300,
                  "cache_read": 4000, "last_used_ms": 999_940_000u64, "in_flight": 0,
                  "accounts": [], "efforts": [], "endpoints": [] },
            ],
            "activity": {
                "in_flight": [],
                "completed": [
                    { "kind": "note", "at_ms": 999_910_000u64,
                      "text": "switch (none) → a (initial selection)", "error": false },
                ],
            },
            "logs": [
                { "level": "INFO", "text": "proxy: proxy listening" },
            ],
        }))
        .expect("parse parity doc")
    }

    /// The rows that legitimately differ between local and attach: the header
    /// (row 0, attach banner) and the two footer rows (keybar shows
    /// `R disabled (attached)`). Everything else is the shared data render.
    fn body_rows(rows: &[String], h: usize) -> &[String] {
        &rows[1..h - 2]
    }

    #[test]
    fn local_and_attach_render_identically_from_the_same_doc() {
        const W: u16 = 160;
        const H: u16 = 30;
        let doc = parity_doc();
        // BOTH backends build the view through the single from_doc contract.
        let view = DashboardView::from_doc(&doc);

        for overlay in [
            Overlay::None,
            Overlay::Stats,
            Overlay::Logs,
            Overlay::Accounts,
        ] {
            let local = render_rows(&view, &chrome_overlay(overlay), W, H);
            let attach = render_rows(&view, &chrome_attach(overlay), W, H);
            assert_eq!(
                body_rows(&local, H as usize),
                body_rows(&attach, H as usize),
                "MAIN+overlay body must render identically for {overlay:?} \
                 regardless of local vs attach (single DashboardDoc, unforked renderer)"
            );
        }
    }

    /// Issue #5 acceptance (render side): the every-frame compose is
    /// MAIN-then-overlay-then-footer. MAIN's frame is built and its data drawn
    /// every tick (so MAIN "keeps updating"); a summoned overlay then covers the
    /// body rect (all but the footer) and the MAIN-frame footer keybar stays
    /// drawn over everything. So MAIN-only shows the model strip; under an
    /// overlay the body is the overlay's surface while the footer keybar — proof
    /// the MAIN compose ran this frame — remains. The deterministic "MAIN state
    /// keeps updating underneath" guarantee is in the `mod.rs` state-machine
    /// test (`open_overlay_preserves_main_state_then_esc_returns_to_main`); this
    /// pins the draw-order contract.
    #[test]
    fn main_is_composed_every_frame_then_the_overlay_is_drawn_over_it() {
        const W: u16 = 160;
        const H: u16 = 30;
        let doc = parity_doc();
        let view = DashboardView::from_doc(&doc);

        // MAIN only: the compact model strip (part of MAIN) is visible.
        let main_only = render(&view, &chrome_overlay(Overlay::None), W, H);
        assert!(main_only.contains("gpt-5.5"), "MAIN shows the model strip");
        assert!(
            main_only.contains("logs"),
            "MAIN footer keybar advertises the logs shortcut"
        );

        // Logs overlay: the overlay body covers MAIN's strip, but the footer
        // keybar (drawn last, from the same MAIN compose) is still present,
        // proving the overlay is layered ON the MAIN frame rather than replacing
        // the render path.
        let with_logs = render(&view, &chrome_overlay(Overlay::Logs), W, H);
        assert!(
            with_logs.contains("proxy: proxy listening"),
            "Logs overlay drawn on top of MAIN"
        );
        assert!(
            with_logs.contains("logs"),
            "MAIN footer keybar still drawn over the overlay (MAIN compose ran)"
        );
    }

    #[test]
    fn activity_meta_body_drops_group_word_and_vendor_prefix(/* Z 2026-07-15 */) {
        // Claude models drop the redundant `claude-` prefix AND the group
        // word: `claude opus-4-8[1m]` → `opus-4-8[1m]`.
        assert_eq!(
            activity_meta_body(Some("claude"), Some("claude-opus-4-8[1m]"), None),
            "[opus-4-8[1m]]"
        );
        // Codex/grok rows now SHOW their served model, minus the group word.
        assert_eq!(
            activity_meta_body(Some("codex"), Some("gpt-5.6-sol"), Some("high")),
            "[gpt-5.6-sol high]"
        );
        assert_eq!(
            activity_meta_body(Some("grok"), Some("grok-4.5"), Some("high")),
            "[grok-4.5 high]"
        );
        // No group → no claude- stripping (the id is the client-requested one).
        assert_eq!(
            activity_meta_body(None, Some("claude-haiku-4-5"), None),
            "[claude-haiku-4-5]"
        );
        // Nothing known → empty body (the caller pads the shared slot).
        assert_eq!(activity_meta_body(None, None, None), "");
    }

    #[test]
    fn activity_meta_body_caps_hostile_width_and_pads_via_pad_cells() {
        // Belt-and-braces: a hostile model id cannot exceed the cap…
        let hostile = activity_meta_body(Some("claude"), Some(&"x".repeat(100)), Some("high"));
        assert!(cell_width(&hostile) <= META_W_MAX);
        // …including wide (CJK) + context-sensitive sequences (VS16/ZWJ) —
        // the contract is display CELLS, not chars.
        for meta in [
            activity_meta_body(Some("claude"), Some("모델-한글-이름-아주-긴-경우"), Some("high")),
            activity_meta_body(
                Some("claude"),
                Some("☂\u{fe0f}-model-\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}-\u{1f44d}\u{1f3fd}-very-long-tail"),
                Some("high"),
            ),
        ] {
            assert!(cell_width(&meta) <= META_W_MAX, "badge {meta:?} within cap");
        }
        // And the caller-side padding lands every body on the shared width.
        for body in ["", "[opus-4-8 low]", "[gpt-5.6-sol max]"] {
            assert_eq!(cell_width(&pad_cells(body, 20)), 20);
        }
    }

    #[test]
    fn activity_meta_body_effort_rules(/* fast token removed, Z 2026-07-15 */) {
        // Effort renders as a bare space-separated token; `fast` never shows.
        assert_eq!(
            activity_meta_body(Some("codex"), Some("gpt-5.6-sol"), Some("max")),
            "[gpt-5.6-sol max]"
        );
        assert_eq!(
            activity_meta_body(Some("claude"), Some("fable-5"), Some("low")),
            "[fable-5 low]"
        );
        // Unknown effort ("-"/empty) is omitted — no trailing token.
        assert_eq!(
            activity_meta_body(Some("codex"), Some("gpt-5.6-sol"), Some("-")),
            "[gpt-5.6-sol]"
        );
        assert_eq!(
            activity_meta_body(Some("codex"), Some("gpt-5.6-sol"), Some("")),
            "[gpt-5.6-sol]"
        );
    }

    #[test]
    fn strip_marker_animates_and_carries_the_leading_space(/* UI-6 item 2 */) {
        let now = SystemTime::now();
        let mut m = model_row("claude", "claude-opus-4-8", 100, 50);
        m.in_flight = 1;
        // 2a: the in-flight marker rides the shared frame — frozen at 0 before.
        let f0 = model_active_marker(&m, now, 0).content.into_owned();
        let f3 = model_active_marker(&m, now, 3).content.into_owned();
        assert_ne!(f0, f3, "in-flight strip marker must animate with the frame");
        // 2b: every variant reserves a LEADING space so the glyph sits one cell
        // right of the border while the 2-cell column stays aligned.
        assert!(
            f0.starts_with(' ') && f0.chars().count() == 2,
            "spinner: {f0:?}"
        );
        let mut codex = model_row("codex", "gpt-5.6-sol", 100, 50);
        codex.in_flight = 2;
        let cx = model_active_marker(&codex, now, 1).content.into_owned();
        assert!(
            cx.starts_with(' ') && cx.chars().count() == 2,
            "codex spin: {cx:?}"
        );
        // Idle (never used) marker is the 2-cell blank, still leading-spaced.
        let idle = model_active_marker(&model_row("claude", "x", 1, 1), now, 0)
            .content
            .into_owned();
        assert_eq!(idle, "  ", "idle marker fills the 2-cell column");
    }

    #[test]
    fn max_effort_badge_spans_cycle_when_on_and_are_static_when_off(/* UI-6 item 6 */) {
        let fg = |spans: &[Span]| spans.iter().map(|s| s.style.fg).collect::<Vec<_>>();
        let text = |spans: &[Span]| spans.iter().map(|s| s.content.as_ref()).collect::<String>();
        // A plain (non-headline) model isolates the effort token from the name
        // gradient, so a color change can only come from the `max` marquee.
        // Effects ON: `max` is one span per char and the palette slides with
        // the frame, so consecutive frames differ.
        let f0 = activity_meta_spans(
            Some("codex"),
            Some("gpt-5.5"),
            Some("max"),
            0,
            true,
            GradientCfg::default(),
        );
        let f1 = activity_meta_spans(
            Some("codex"),
            Some("gpt-5.5"),
            Some("max"),
            1,
            true,
            GradientCfg::default(),
        );
        assert_ne!(fg(&f0), fg(&f1), "max marquee must cycle across frames");
        // The assembled TEXT stays byte-identical to the width-measuring SSOT.
        assert_eq!(
            text(&f0),
            activity_meta_body(Some("codex"), Some("gpt-5.5"), Some("max"))
        );
        // Effects OFF: static — every frame renders the same distinct color.
        let off0 = activity_meta_spans(
            Some("codex"),
            Some("gpt-5.5"),
            Some("max"),
            0,
            false,
            GradientCfg::default(),
        );
        let off9 = activity_meta_spans(
            Some("codex"),
            Some("gpt-5.5"),
            Some("max"),
            9,
            false,
            GradientCfg::default(),
        );
        assert_eq!(fg(&off0), fg(&off9), "effects off ⇒ static color");
        assert!(
            off0.iter().any(|s| s.style.fg == Some(Color::LightMagenta)),
            "off-state max is the distinct static LightMagenta"
        );
        // xhigh is a static distinct color regardless of the frame (a plain
        // non-headline model isolates the effort token from the name gradient).
        let xh_on = activity_meta_spans(
            Some("codex"),
            Some("gpt-5.5"),
            Some("xhigh"),
            0,
            true,
            GradientCfg::default(),
        );
        let xh_off = activity_meta_spans(
            Some("codex"),
            Some("gpt-5.5"),
            Some("xhigh"),
            5,
            true,
            GradientCfg::default(),
        );
        assert_eq!(fg(&xh_on), fg(&xh_off), "xhigh never animates");
        assert!(xh_on.iter().any(|s| s.style.fg == Some(Color::LightRed)));
    }

    #[test]
    fn fable5_name_spans_use_the_group_gradient_when_on(/* UI-6 item 7 */) {
        let fg = |spans: &[Span]| spans.iter().map(|s| s.style.fg).collect::<Vec<_>>();
        // A headline model splits into per-char spans, all from the claude
        // (magenta) family, and the palette slides with the frame.
        let on0 = model_name_spans(Some("claude"), "fable-5", 0, true, GradientCfg::default());
        let on1 = model_name_spans(Some("claude"), "fable-5", 8, true, GradientCfg::default());
        assert!(on0.len() > 1, "gradient splits the name per char");
        assert!(
            on0.iter()
                .enumerate()
                .all(|(i, s)| s.style.fg == Some(gradient_solid(CLAUDE_GRADIENT_BASE, 0, i, 1.0))),
            "fable-5 uses the solid claude/magenta gradient (herdr 단색 mode)"
        );
        assert_ne!(fg(&on0), fg(&on1), "gradient drifts with the frame");
        // Solid mode NEVER changes hue — every span keeps the base color's
        // channel ordering (r > b > g for the magenta base), only luma moves.
        for s in on0.iter().chain(on1.iter()) {
            let Some(Color::Rgb(r, g, b)) = s.style.fg else {
                panic!("solid gradient renders truecolor, got {:?}", s.style.fg);
            };
            assert!(r >= b && b >= g, "magenta hue preserved, got ({r},{g},{b})");
        }
        // Codex headline models use the cyan family base.
        let codex = model_name_spans(
            Some("codex"),
            "gpt-5.6-sol",
            0,
            true,
            GradientCfg::default(),
        );
        assert!(codex
            .iter()
            .enumerate()
            .all(|(i, s)| s.style.fg == Some(gradient_solid(CODEX_GRADIENT_BASE, 0, i, 1.0))));
        // Effects OFF: a single static bold group-colored span.
        let off = model_name_spans(Some("claude"), "fable-5", 0, false, GradientCfg::default());
        assert_eq!(off.len(), 1);
        assert_eq!(off[0].style.fg, Some(Color::Magenta));
        assert!(off[0].style.add_modifier.contains(Modifier::BOLD));
        // Ordinary models stay a single plain group span (no gradient) even on.
        let plain = model_name_spans(Some("claude"), "opus-4-8", 0, true, GradientCfg::default());
        assert_eq!(plain.len(), 1);
    }

    #[test]
    fn gradient_cfg_parses_hex_and_sanitizes_speed(/* UI-8 */) {
        // Well-formed config: colors parse, speed passes through.
        let cfg = GradientCfg::from_config(&crate::config::TuiGradient {
            speed: 2.5,
            claude: "#102030".into(),
            codex: "#a0B0c0".into(),
            max_effort: Some("#ffffff".into()),
        });
        assert_eq!(cfg.speed, 2.5);
        assert_eq!(cfg.claude, (0x10, 0x20, 0x30));
        assert_eq!(cfg.codex, (0xa0, 0xb0, 0xc0), "hex is case-insensitive");
        assert_eq!(cfg.max_effort, Some((255, 255, 255)));
        // Garbage falls back instead of guessing: colors to the built-in
        // anchors, speed to 1.0 — a bad config must never freeze or panic.
        for bad_speed in [0.0, -3.0, f32::NAN, f32::INFINITY] {
            let cfg = GradientCfg::from_config(&crate::config::TuiGradient {
                speed: bad_speed,
                claude: "not-a-color".into(),
                codex: "#12345".into(),
                max_effort: Some("".into()),
            });
            assert_eq!(cfg.speed, 1.0, "speed {bad_speed} sanitized");
            assert_eq!(cfg.claude, CLAUDE_GRADIENT_BASE);
            assert_eq!(cfg.codex, CODEX_GRADIENT_BASE);
            assert_eq!(cfg.max_effort, None);
        }
        // Defaults: speed 1.0, the built-in anchors, rainbow kept.
        let def = GradientCfg::default();
        assert_eq!(
            (def.speed, def.claude, def.codex, def.max_effort),
            (1.0, CLAUDE_GRADIENT_BASE, CODEX_GRADIENT_BASE, None)
        );
    }

    #[test]
    fn gradient_speed_scales_the_temporal_drift(/* UI-8 */) {
        // Same frame, double speed ⇒ double the temporal phase advance.
        let base = gradient_phase(10, 0, 1.0) - gradient_phase(0, 0, 1.0);
        let fast = gradient_phase(10, 0, 2.0) - gradient_phase(0, 0, 2.0);
        assert!((fast - 2.0 * base).abs() < 1e-4);
        // And the configured base colors drive the solid gradient directly:
        // at any phase the scaled channels keep the base's ordering.
        let custom = GradientCfg::from_config(&crate::config::TuiGradient {
            speed: 1.0,
            claude: "#804020".into(),
            codex: "#56dcdc".into(),
            max_effort: None,
        });
        let spans = model_name_spans(Some("claude"), "fable-5", 3, true, custom);
        for s in &spans {
            let Some(Color::Rgb(r, g, b)) = s.style.fg else {
                panic!("truecolor expected");
            };
            assert!(r >= g && g >= b, "configured hue preserved ({r},{g},{b})");
        }
    }

    #[test]
    fn max_effort_override_swaps_rainbow_for_solid(/* UI-8 */) {
        let solid = GradientCfg {
            max_effort: Some((200, 100, 50)),
            ..GradientCfg::default()
        };
        let spans = effort_spans(Some("codex"), "max", 0, true, solid);
        for (i, s) in spans.iter().enumerate() {
            assert_eq!(
                s.style.fg,
                Some(gradient_solid((200, 100, 50), 0, i, 1.0)),
                "max token breathes the configured color instead of the rainbow"
            );
        }
        // Default keeps the rainbow (distinct from any solid scaling).
        let rainbow = effort_spans(Some("codex"), "max", 0, true, GradientCfg::default());
        assert_eq!(
            rainbow[0].style.fg,
            Some(gradient_rainbow(0, 0, 1.0)),
            "no override → 3-phase sine rainbow"
        );
    }

    #[test]
    fn curl_command_replays_the_exchange_shell_safely(/* UI-8 */) {
        let curl = curl_command(
            "POST",
            "http://localhost:3456/v1/messages",
            Some(&[
                ("content-type".to_string(), "application/json".to_string()),
                ("x-api-key".to_string(), "•••redacted".to_string()),
                ("content-length".to_string(), "42".to_string()),
                ("host".to_string(), "localhost:3456".to_string()),
            ]),
            r#"{"model":"m","note":"it's quoted"}"#,
        );
        assert!(curl.starts_with("curl -X 'POST' 'http://localhost:3456/v1/messages'"));
        // A method carrying shell metacharacters (valid RFC 7230 tchar tokens)
        // is quoted, not interpolated raw — no command substitution on paste.
        let evil = curl_command("`id`", "http://x/y", None, "");
        assert!(
            evil.starts_with("curl -X '`id`' 'http://x/y'"),
            "method shell-quoted: {evil}"
        );
        assert!(curl.contains("-H 'content-type: application/json'"));
        assert!(
            curl.contains("-H 'x-api-key: •••redacted'"),
            "redacted value kept verbatim for the user to substitute"
        );
        assert!(
            !curl.contains("-H 'content-length") && !curl.contains("-H 'host"),
            "curl-managed headers dropped so the command replays cleanly: {curl}"
        );
        assert!(
            curl.contains(concat!(
                r#"--data-raw '{"model":"m","note":"it'"#,
                r#"\''s quoted"}'"#
            )),
            "single quotes shell-escaped: {curl}"
        );
    }

    #[test]
    fn raw_content_builds_four_tabs_for_translated_exchanges(/* UI-8 */) {
        let general = || RawGeneral {
            lines: vec![Line::from("general")],
            method: "POST".into(),
            path: "/v1/messages".into(),
            base_url: "http://localhost:3456".into(),
        };
        let mut record = crate::proxy::raw_io::RawIoRecord::new(
            7,
            0,
            Some("codex".into()),
            None,
            None,
            Some(200),
            br#"{"model":"claude"}"#,
            b"event: message_start\n\n",
            1 << 20,
            Some(vec![("content-type".into(), "application/json".into())]),
            None,
            None,
        );
        // No upstream half → the classic 2 tabs, both carrying the client curl.
        let two = raw_content_from_record(general(), &record);
        assert_eq!(
            two.tabs.iter().map(|t| t.label).collect::<Vec<_>>(),
            vec!["Request", "Response"]
        );
        assert!(two.tabs[0]
            .curl
            .contains("curl -X 'POST' 'http://localhost:3456/v1/messages'"));
        assert_eq!(two.tabs[0].body_text, r#"{"model":"claude"}"#);

        // Upstream half present → 4 tabs in wire order, the upstream pair
        // carrying the REWRITTEN request's curl against the provider.
        record.upstream = Some(crate::proxy::raw_io::UpstreamRaw {
            url: Some("https://api.example.com/responses".into()),
            request_body: Some(r#"{"input":[]}"#.into()),
            request_headers: Some(vec![("authorization".into(), "•••redacted".into())]),
            response_body: Some("event: response.completed\n\n".into()),
            response_headers: Some(vec![("x-request-id".into(), "req_9".into())]),
        });
        let four = raw_content_from_record(general(), &record);
        assert_eq!(
            four.tabs.iter().map(|t| t.label).collect::<Vec<_>>(),
            vec!["Request", "Upstream Req", "Upstream Resp", "Response"],
            "wire order: client req → upstream req → upstream resp → client resp"
        );
        assert_eq!(four.tabs[1].body_text, r#"{"input":[]}"#);
        assert!(four.tabs[1]
            .curl
            .contains("curl -X 'POST' 'https://api.example.com/responses'"));
        assert!(four.tabs[2].body_text.contains("response.completed"));
        assert!(
            four.tabs[2].curl.contains("api.example.com"),
            "a response tab replays its side's request"
        );
        // The bulk payloads are prebuilt for the buttons.
        assert!(four.all_text.contains("── Upstream Req ──"));
        let parsed: serde_json::Value =
            serde_json::from_str(&four.record_json).expect("save-all payload is valid JSON");
        assert_eq!(parsed["id"].as_u64(), Some(7));
        // Every tab measured its widest line for the horizontal scroll bound.
        assert!(four.tabs.iter().all(|t| t.width > 0));
    }

    #[test]
    fn raw_request_line_and_hit_open_the_raw_viewer(/* UI-7 */) {
        let entry = completed_request(1_000, Some("claude"), Some("opus"), 10, 5, 200);
        let key = entry.activity_key().expect("request key");
        // The first detail line carries the clickable magnifier + method/path.
        let lines = completed_detail_lines(&entry, false, &Default::default());
        let first: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            first.contains("🔍 request"),
            "magnifier sits left of request: {first:?}"
        );
        assert!(first.contains("POST /v1/messages"));
        // The hit registers on the request line's row (first detail line) and
        // resolves to OpenRaw.
        let mut hits = Vec::new();
        push_raw_line_hit(&mut hits, &key, &entry, 5, 4);
        // The hit carries the entry's activity id so the opener can pin the
        // exact row under a same-ms ActivityKey collision (MUST-FIX).
        let eid = match &entry.body {
            CompletedBody::Request { id, .. } => *id,
            _ => unreachable!("seeded a request"),
        };
        assert_eq!(hits[0].kind, ActivityHitKind::RawLine { id: eid });
        assert_eq!((hits[0].y_start, hits[0].height), (6, 1));
        let chrome = ActivityChrome {
            area: Rect::new(0, 0, 80, 20),
            hits,
        };
        assert_eq!(
            hit_test_activity(&chrome, 10, 6),
            Some(ActivityClick::OpenRaw(key.clone(), eid))
        );
        // A collapsed entry (height 1: no detail lines rendered) registers none.
        let mut none = Vec::new();
        push_raw_line_hit(&mut none, &key, &entry, 5, 1);
        assert!(none.is_empty());
    }

    #[test]
    fn raw_body_lines_pretty_print_and_highlight_json(/* UI-7 */) {
        let lines = raw_body_lines(r#"{"model":"opus","n":42,"ok":true,"nil":null}"#);
        assert!(lines.len() > 4, "pretty print splits onto multiple lines");
        let all: Vec<(String, Option<Color>)> = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| (s.content.to_string(), s.style.fg)))
            .collect();
        let fg_of = |needle: &str| {
            all.iter()
                .find(|(c, _)| c.contains(needle))
                .and_then(|(_, f)| *f)
        };
        assert_eq!(fg_of("\"model\""), Some(Color::Cyan), "keys cyan");
        assert_eq!(fg_of("\"opus\""), Some(Color::Green), "string values green");
        assert_eq!(fg_of("42"), Some(Color::Yellow), "numbers yellow");
        assert_eq!(fg_of("true"), Some(Color::Magenta), "booleans magenta");
        assert_eq!(fg_of("null"), Some(Color::Magenta), "null magenta");
        let text: String = all.iter().map(|(c, _)| c.as_str()).collect::<String>();
        assert!(
            text.contains("\"model\": \"opus\""),
            "content survives: {text}"
        );
    }

    #[test]
    fn raw_body_lines_highlight_sse_data_payloads_inline(/* UI-7 */) {
        let body = "event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: ping\ndata: not-json";
        let lines = raw_body_lines(body);
        assert_eq!(lines.len(), 5, "SSE keeps its own lines");
        assert!(
            lines[1].spans.len() > 2,
            "a json `data:` payload splits into styled spans"
        );
        assert_eq!(
            lines[4].spans.len(),
            1,
            "a non-json `data:` line stays one raw span"
        );
    }

    #[test]
    fn wrap_raw_line_bounds_monster_lines_on_char_boundaries(/* UI-7 */) {
        let big = "x".repeat(RAW_LINE_WRAP * 2 + 10);
        let chunks = wrap_raw_line(&big);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.len() <= RAW_LINE_WRAP));
        // Multi-byte chars are never split mid-scalar.
        let uni = "€".repeat(RAW_LINE_WRAP);
        for chunk in wrap_raw_line(&uni) {
            assert!(chunk.len() <= RAW_LINE_WRAP);
            assert!(chunk.chars().all(|c| c == '€'), "no torn scalars");
        }
    }

    #[test]
    fn truncate_cells_zero_budget_yields_empty(/* UI-4 R3 nice 1/2 */) {
        assert_eq!(truncate_cells("some-model", 0), "");
        assert_eq!(truncate_cells("", 0), "");
        // Zero-width (combining-only) text also honors the exact contract.
        assert_eq!(truncate_cells("\u{200d}", 0), "");
    }

    #[test]
    fn expanded_detail_keeps_the_full_served_backend_model(/* UI-4 V6 */) {
        // The compact row hides the codex/grok served id; the expanded
        // detail is the fidelity surface and MUST keep it.
        let entry = completed_request(1_000, Some("codex"), Some("gpt-5.6-sol"), 10, 5, 200);
        let lines = completed_detail_lines(&entry, false, &Default::default());
        let text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("codex gpt-5.6-sol"),
            "expanded detail keeps the full served id, got:\n{text}"
        );
    }

    #[test]
    fn models_strip_fills_its_pane_height(/* UI-4 V1/V2 */) {
        let docs: Vec<ModelUsageDoc> = (0..7)
            .map(|i| model_row("claude", &format!("m-{i}"), 1_000 - i as u64, 10))
            .collect();
        let view = view_with(docs);
        // Assertions pin the actual rendered rows (last visible + first
        // hidden model id), not just the title's self-reported count.
        // Default auto height shows MODEL_STRIP_ROWS rows.
        let text = render(&view, &chrome_overlay(Overlay::None), 220, 50);
        assert!(
            text.contains(" models — top 5 of 7 by tokens"),
            "default strip shows 5 rows"
        );
        assert!(text.contains("m-4"), "5th row rendered at default height");
        assert!(!text.contains("m-5"), "6th row hidden at default height");
        // Drag-expanded pane (border + header + 7): every row fits.
        let mut chrome = chrome_overlay(Overlay::None);
        chrome.pane_heights.strip = Some(9);
        let text = render(&view, &chrome, 220, 50);
        assert!(
            text.contains(" models — top 7 of 7 by tokens"),
            "a taller pane reveals more rows"
        );
        assert!(
            text.contains("m-6"),
            "last row rendered when dragged taller"
        );
        // Shrunk to the drag minimum (border + header + 1).
        let mut chrome = chrome_overlay(Overlay::None);
        chrome.pane_heights.strip = Some(PANE_MIN_HEIGHT);
        let text = render(&view, &chrome, 220, 50);
        assert!(
            text.contains(" models — top 1 of 7 by tokens"),
            "the minimum pane still renders one row"
        );
        assert!(text.contains("m-0"), "first row rendered at minimum height");
        assert!(!text.contains("m-1"), "second row hidden at minimum height");
    }

    #[test]
    fn in_flight_row_shows_abbreviated_model_badge(/* issue #2, 2a */) {
        // No effort / fast off: the badge is just [group model] — no stray
        // separators or trailing spaces.
        let mut view = view_with(Vec::new());
        view.in_flight = vec![super::super::activity::InFlight {
            id: 1,
            method: "POST".into(),
            path: "/v1/messages".into(),
            account: Some("claude:me@example.com".into()),
            group: Some("claude".into()),
            model: Some("claude-opus-4-8".into()),
            effort: None,
            fast: false,
            kind: None,
            started_at: std::time::SystemTime::UNIX_EPOCH,
        }];
        let text = render(&view, &chrome_overlay(Overlay::None), 160, 30);
        assert!(
            text.contains("[opus-4-8]"),
            "in-flight badge without effort is [model] only — no group word (Z 2026-07-15)"
        );
        assert!(
            !text.contains("claude-opus-4-8"),
            "model label is abbreviated, not the raw claude- id (2b)"
        );
    }

    #[test]
    fn in_flight_row_shows_effort_and_fast_like_a_completed_row() {
        // Routed effort/fast render on the RUNNING row with the same bracket
        // tag format as its eventual completed entry.
        let mut view = view_with(Vec::new());
        view.in_flight = vec![super::super::activity::InFlight {
            id: 1,
            method: "POST".into(),
            path: "/v1/messages".into(),
            account: Some("codex:me@example.com".into()),
            group: Some("codex".into()),
            model: Some("gpt-5.6-sol".into()),
            effort: Some("max".into()),
            fast: true,
            kind: None,
            started_at: std::time::SystemTime::UNIX_EPOCH,
        }];
        let text = render(&view, &chrome_overlay(Overlay::None), 160, 30);
        // Z 2026-07-15: the served model now SHOWS on activity rows (group
        // word dropped instead), and the `fast` token is gone from the badge.
        assert!(
            text.contains("[gpt-5.6-sol max]"),
            "in-flight badge is [model effort], no group word, no fast token"
        );
        assert!(
            !text.contains("[codex max fast]") && !text.contains("max fast]"),
            "the fast token no longer rides the badge"
        );
    }

    #[test]
    fn in_flight_row_shows_the_kind_column(/* UI-6 item 1 */) {
        // The running row carries the same `kind` column as a completed row so
        // the meta/email columns line up across both — `kind` before the badge.
        let mut view = view_with(Vec::new());
        view.in_flight = vec![super::super::activity::InFlight {
            id: 3,
            method: "POST".into(),
            path: "/v1/messages".into(),
            account: Some("claude:me@example.com".into()),
            group: Some("claude".into()),
            model: Some("claude-opus-4-8".into()),
            effort: None,
            fast: false,
            kind: Some("compact".into()),
            started_at: std::time::SystemTime::UNIX_EPOCH,
        }];
        let rows = render_rows(&view, &chrome_overlay(Overlay::None), 160, 30);
        let row = rows
            .iter()
            .find(|l| l.contains("[opus-4-8]"))
            .expect("in-flight row rendered");
        assert!(
            row.find("compact").unwrap_or(usize::MAX)
                < row.find("[opus-4-8]").unwrap_or(usize::MAX),
            "kind → badge order on the in-flight row: {row}"
        );
    }

    #[test]
    fn activity_cost_at_or_above_one_dollar_is_yellow_bold(/* UI-6 item 5 */) {
        // A row costing ≥ $1 shouts (Yellow + BOLD); below $1 stays plain. The
        // color boundary matches `format_cost`'s 2dp boundary.
        let row = |out: u64| Completed {
            at: UNIX_EPOCH + Duration::from_millis(1_000),
            body: CompletedBody::Request {
                id: 1,
                method: "POST".into(),
                path: "/v1/messages".into(),
                account: Some("codex:me@example.com".into()),
                status: 200,
                duration: Duration::from_millis(10),
                tokens: Some(super::super::TokenCounts {
                    input: 0,
                    output: out,
                    cache_read: None,
                    cache_creation: None,
                }),
                group: Some("codex".into()),
                model: Some("gpt-5.5".into()),
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
        let labels = BTreeMap::new();
        let abbrev = BTreeMap::new();
        let cost_span = |entry: &Completed| {
            let m = RowMetrics::measure(200, &[], &[entry]);
            completed_line(
                entry,
                false,
                false,
                &labels,
                &abbrev,
                &m,
                0,
                true,
                GradientCfg::default(),
            )
            .spans
            .into_iter()
            .find(|s| s.content.contains('$'))
            .expect("a cost span with a $ amount")
        };

        // 1M output tokens on gpt-5.5 = $30.00 ≥ $1 → Yellow + BOLD.
        let pricey = cost_span(&row(1_000_000));
        assert_eq!(pricey.style.fg, Some(Color::Yellow));
        assert!(pricey.style.add_modifier.contains(Modifier::BOLD));

        // 1k output tokens = $0.03 < $1 → plain.
        let cheap = cost_span(&row(1_000));
        assert_ne!(cheap.style.fg, Some(Color::Yellow));
        assert!(!cheap.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn completed_row_layout_puts_excerpt_last_at_full_width(/* Z 2026-07-15 */) {
        // Row contract: time · kind · [model effort] · email(10) → status
        // dur tok $ … "excerpt", with the excerpt LAST and spending the rest
        // of the panel width.
        let mut view = view_with(Vec::new());
        view.completed = vec![Completed {
            at: UNIX_EPOCH + Duration::from_millis(1_000),
            body: CompletedBody::Request {
                id: 1,
                method: "POST".into(),
                path: "/v1/messages?beta=true".into(),
                account: Some("claude:someone@example.com".into()),
                status: 200,
                duration: Duration::from_millis(3_100),
                tokens: Some(super::super::TokenCounts {
                    input: 100,
                    output: 169,
                    cache_read: None,
                    cache_creation: None,
                }),
                group: Some("claude".into()),
                model: Some("claude-opus-4-8".into()),
                effort: Some("high".into()),
                fast: Some(false),
                ttfb_ms: None,
                ttft_ms: None,
                gen_ms: None,
                aborted: false,
                user_id: None,
                kind: Some("user".into()),
                excerpt: Some("고쳐줘 빨리 제발 이거 진짜 마지막이다".into()),
            },
        }];
        let rows = render_rows(&view, &chrome_overlay(Overlay::None), 200, 30);
        let row = rows
            .iter()
            .find(|l| l.contains("[opus-4-8 high]"))
            .expect("activity row rendered with the [model effort] badge");
        // Column order: kind before badge, badge before email, email before
        // status block, excerpt last.
        let pos = |needle: &str| row.find(needle).unwrap_or(usize::MAX);
        assert!(pos("user") < pos("[opus-4-8 high]"), "kind → badge: {row}");
        assert!(
            pos("[opus-4-8 high]") < pos("someone@e"),
            "badge → email: {row}"
        );
        // The email column clips to 10 cells (`someone@e…`).
        assert!(pos("someone@e") < pos("200"), "email → status: {row}");
        // The excerpt (opening quote) comes after the status block. Wide CJK
        // chars render with buffer filler cells ("고 쳐 줘"), so compare the
        // space-stripped row for content checks.
        assert!(
            pos("200") < pos("\u{201c}"),
            "status block → excerpt last: {row}"
        );
        assert!(row.contains("269tok"), "token column: {row}");
        assert!(row.contains('$'), "cost column: {row}");
        let flat: String = row.chars().filter(|c| *c != ' ').collect();
        assert!(
            flat.contains("마지막이다"),
            "excerpt not clipped at the old 12-char cap: {row}"
        );
    }

    // --- Feature A: cost display -------------------------------------------

    #[test]
    fn format_cost_decimal_scheme() {
        // Exactly zero → fixed 4-decimal sentinel.
        assert_eq!(format_cost(0.0), "$0.0000");
        // Sub-dollar → 4 decimals so small per-request costs stay legible.
        assert_eq!(format_cost(0.0123), "$0.0123");
        assert_eq!(format_cost(0.999_94), "$0.9999");
        // ≥ $1 → 2 decimals.
        assert_eq!(format_cost(1.0), "$1.00");
        assert_eq!(format_cost(3.775), "$3.77"); // round-half-to-even (banker's)
        assert_eq!(format_cost(12.5), "$12.50");
    }

    #[test]
    fn model_cost_matches_pricing_table() {
        // opus: 5/25/0.5/6.25 per 1e6 → 200k in (1.0) + 100k out (2.5)
        //   + 40k cache_read (0.02) = 3.52.
        let mut m = model_row("claude", "claude-opus-4-8", 200_000, 100_000);
        m.cache_read = Some(40_000);
        m.cache_creation = None;
        let cost = model_cost(&m);
        assert!((cost - (1.0 + 2.5 + 0.02)).abs() < 1e-9, "got {cost}");
        assert_eq!(format_cost(cost), "$3.52");
    }

    #[test]
    fn model_cost_prefers_server_value_and_falls_back_for_old_docs() {
        // Server-computed cost (issue #62 S1) wins — it already reflects the
        // daemon's pricing overrides, which the render path cannot see.
        let mut m = model_row("claude", "claude-opus-4-8", 200_000, 100_000);
        m.cache_read = Some(40_000);
        m.cost_usd = 9.99;
        assert!((model_cost(&m) - 9.99).abs() < 1e-12);
        // Old-daemon doc (serde default 0.0) with tokens → local pricing
        // fallback (same math as `model_cost_matches_pricing_table`).
        m.cost_usd = 0.0;
        assert!((model_cost(&m) - 3.52).abs() < 1e-9);
        // No tokens at all (e.g. an in-flight-only row) → $0, no lookup.
        let mut idle = model_row("claude", "claude-opus-4-8", 0, 0);
        idle.cache_read = None;
        assert!(model_cost(&idle).abs() < 1e-12);
    }

    #[test]
    fn model_total_counts_all_four_token_classes() {
        // Distinct per-class values so a dropped class is caught: the strip's
        // `tok` must use the same denominators as its `$` (`model_cost`).
        let mut m = model_row("claude", "claude-opus-4-8", 1_000, 20_000);
        m.cache_read = Some(300_000);
        m.cache_creation = Some(4_000_000);
        assert_eq!(model_total(&m), 1_000 + 20_000 + 300_000 + 4_000_000);
        // Absent cache counters (upstream never reported them) count 0.
        m.cache_read = None;
        m.cache_creation = None;
        assert_eq!(model_total(&m), 21_000);
    }

    fn completed_request(
        at_ms: u64,
        group: Option<&str>,
        model: Option<&str>,
        input: u64,
        output: u64,
        status: u16,
    ) -> Completed {
        Completed {
            at: UNIX_EPOCH + Duration::from_millis(at_ms),
            body: CompletedBody::Request {
                id: 1,
                method: "POST".into(),
                path: "/v1/messages".into(),
                account: Some("a@x.com".into()),
                status,
                duration: Duration::from_millis(1_400),
                tokens: Some(crate::tui::TokenCounts {
                    input,
                    output,
                    ..Default::default()
                }),
                group: group.map(str::to_string),
                model: model.map(str::to_string),
                effort: None,
                fast: Some(false),
                ttfb_ms: None,
                ttft_ms: None,
                gen_ms: None,
                aborted: false,
                user_id: None,
                kind: None,
                excerpt: None,
            },
        }
    }

    /// A completed request carrying `kind` + `excerpt`, for the UI-6 item-3
    /// input-modal tests.
    fn completed_with_excerpt(kind: Option<&str>, excerpt: &str) -> Completed {
        let mut entry =
            completed_request(1_000, Some("claude"), Some("claude-opus-4-8"), 10, 5, 200);
        if let CompletedBody::Request {
            kind: k,
            excerpt: x,
            ..
        } = &mut entry.body
        {
            *k = kind.map(str::to_string);
            *x = Some(excerpt.to_string());
        }
        entry
    }

    #[test]
    fn input_line_offset_tracks_kind_presence(/* UI-6 item 3 */) {
        // request, kind, input → the input line is the 3rd detail row (offset 2).
        assert_eq!(
            completed_input_line_offset(&completed_with_excerpt(Some("user"), "hi")),
            Some(2)
        );
        // request, input → offset 1 when no kind line precedes it.
        assert_eq!(
            completed_input_line_offset(&completed_with_excerpt(None, "hi")),
            Some(1)
        );
        // No excerpt → the entry has no clickable input line.
        assert_eq!(
            completed_input_line_offset(&completed_request(
                1,
                Some("claude"),
                Some("claude-opus-4-8"),
                1,
                1,
                200
            )),
            None
        );
    }

    #[test]
    fn wrapped_line_count_wraps_words_and_hard_splits(/* UI-6 item 3 */) {
        // Three short words fit one 12-cell line.
        assert_eq!(wrapped_line_count("aa bb cc", 12), 1);
        // Two 6-cell words + a short one spill to a second line.
        assert_eq!(wrapped_line_count("aaaaaa bbbbbb cc", 12), 2);
        // Explicit newlines always break.
        assert_eq!(wrapped_line_count("line1\nline2", 40), 2);
        // A word wider than the line hard-splits across rows.
        assert_eq!(wrapped_line_count(&"x".repeat(25), 10), 3);
    }

    #[test]
    fn wrapped_line_count_charges_leading_whitespace(/* UI-6 item 3 MUST-FIX */) {
        // Leading indentation occupies cells: "    " fills the 4-cell row, so the
        // "x" wraps to a second row (ratatui `Wrap { trim: false }` renders the
        // spaces). The old estimator dropped leading spaces and returned 1.
        assert_eq!(wrapped_line_count("    x", 4), 2);
        // A multi-line indented block = per-line manual cell math:
        //   "    x"  → "    " (row) + "x" (row)        = 2 rows
        //   "  yy"   → "  yy" is 4 cells, fits one row  = 1 row
        assert_eq!(wrapped_line_count("    x\n  yy", 4), 3);
    }

    #[test]
    fn wrapped_line_count_accounts_wide_chars(/* UI-6 item 3 MUST-FIX */) {
        // 가 = 2 cells; at width 4 only two fit per row, so 5 of them need 3
        // rows. Integer cell-width division would have said 2 (undercount).
        assert_eq!(wrapped_line_count(&"가".repeat(5), 4), 3);
        // Odd width where a 2-cell glyph cannot straddle the edge: width 3 holds
        // exactly one 가 per row (2+2 > 3), so 3 가 = 3 rows.
        assert_eq!(wrapped_line_count(&"가".repeat(3), 3), 3);
    }

    #[test]
    fn wrapped_line_count_measures_multi_scalar_graphemes(/* UI-6 item 3 R2 MUST-FIX */) {
        // "❤\u{FE0F}" (heart + VS16) is ONE grapheme that ratatui renders 2 cells
        // wide; summing scalar widths (1 + 0) would undercount it as 1. Measured
        // per cluster, 5 of them at width 4 = 2 clusters/row → 3 rows.
        let heart = "\u{2764}\u{FE0F}".repeat(5);
        assert_eq!(cell_width("\u{2764}\u{FE0F}"), 2, "VS16 cluster is 2 cells");
        assert_eq!(wrapped_line_count(&heart, 4), 3);
    }

    #[test]
    fn input_modal_tail_reachable_at_max_scroll(/* UI-6 item 3 MUST-FIX */) {
        // Operator contract "전체 input을 볼 수 있도록": scrolling to the reported
        // max must bring the LAST line of an indented, multi-line prompt on
        // screen. A too-small max (leading-space undercount) would strand it.
        let mut lines: Vec<String> = (0..40).map(|i| format!("    indented line {i}")).collect();
        lines.push("    TAILMARKER_LAST".to_string());
        let excerpt = lines.join("\n");
        let entry = completed_with_excerpt(Some("user"), &excerpt);
        let key = key_of(&entry);
        let mut view = view_with(Vec::new());
        view.completed = vec![entry];

        // First render: read back the reported max scroll for this modal size.
        let mut chrome = chrome_overlay(Overlay::None);
        chrome.input_modal = Some(InputModal {
            key: key.clone(),
            scroll: 0,
        });
        let mut hits = None;
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).expect("terminal");
        terminal
            .draw(|f| draw(f, Some(&view), &chrome, &mut hits))
            .expect("draw");
        let max = hits
            .expect("layout")
            .input_modal_max_scroll
            .expect("modal drawn");

        // Scroll to the reported max and re-render: the tail line is on screen.
        chrome.input_modal = Some(InputModal { key, scroll: max });
        let text = render(&view, &chrome, 60, 24);
        assert!(
            text.contains("TAILMARKER_LAST"),
            "the last prompt line must be reachable at max scroll (max={max})"
        );
    }

    #[test]
    fn clicking_input_line_opens_modal_showing_full_excerpt(/* UI-6 item 3 */) {
        // A long excerpt that wraps well past one row proves the modal shows the
        // FULL stored text, not the width-clipped activity row.
        let excerpt = "가".repeat(600);
        let entry = completed_with_excerpt(Some("user"), &excerpt);
        let key = key_of(&entry);
        let mut view = view_with(Vec::new());
        view.completed = vec![entry];

        // Expand the entry and capture the hit layout: the input detail line is
        // its OWN hit that resolves to OpenInput (not Entry).
        let mut chrome = chrome_overlay(Overlay::None);
        chrome.expanded_activity = Some(key.clone());
        let mut hits = None;
        let mut terminal = Terminal::new(TestBackend::new(200, 40)).expect("terminal");
        terminal
            .draw(|f| draw(f, Some(&view), &chrome, &mut hits))
            .expect("draw");
        let layout = hits.expect("layout recorded").activity;
        let input_hit = layout
            .hits
            .iter()
            .find(|h| h.kind == ActivityHitKind::InputLine)
            .expect("the 🔍 input line is its own hit target");
        assert_eq!(
            hit_test_activity(&layout, layout.area.x + 8, input_hit.y_start),
            Some(ActivityClick::OpenInput(key.clone())),
            "clicking the input line opens the modal, never collapses the row"
        );

        // Open the modal and render: the box, its scroll/esc footer, and the
        // excerpt text are all on screen.
        chrome.input_modal = Some(InputModal { key, scroll: 0 });
        let text = render(&view, &chrome, 200, 40);
        assert!(
            text.contains("scroll"),
            "modal footer shows the scroll hint"
        );
        assert!(
            text.contains("esc close"),
            "modal footer shows the close hint"
        );
        assert!(text.contains('가'), "modal renders the excerpt text");
    }

    #[test]
    fn input_modal_signals_close_when_entry_aged_out(/* UI-6 item 3 */) {
        // A modal keyed to an entry no longer in `view.completed` draws nothing
        // and reports `None` max-scroll → the runtime closes it gracefully.
        let missing = key_of(&completed_with_excerpt(Some("user"), "gone"));
        let view = view_with(Vec::new()); // empty ring
        let mut chrome = chrome_overlay(Overlay::None);
        chrome.input_modal = Some(InputModal {
            key: missing,
            scroll: 0,
        });
        let mut hits = None;
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).expect("terminal");
        terminal
            .draw(|f| draw(f, Some(&view), &chrome, &mut hits))
            .expect("draw");
        assert_eq!(
            hits.expect("layout").input_modal_max_scroll,
            None,
            "aged-out entry yields the close signal"
        );
    }

    #[test]
    fn activity_row_shows_cost() {
        let mut view = view_with(Vec::new());
        // opus: 1M input = $5.00; rendered inline after the tok count.
        view.completed = vec![completed_request(
            1_000,
            Some("claude"),
            Some("claude-opus-4-8"),
            1_000_000,
            0,
            200,
        )];
        let text = render(&view, &chrome_overlay(Overlay::None), 200, 40);
        assert!(text.contains("$5.00"), "activity row shows the $ cost");
    }

    #[test]
    fn models_strip_and_table_show_cost_column() {
        // No cache tokens so the cost is exactly the input rate (gpt-5.5: $5/1M).
        let mut row = model_row("codex", "gpt-5.5", 1_000_000, 0);
        row.cache_read = None;
        let view = view_with(vec![row]);
        // gpt-5.5 input = $5.00, in the MAIN compact strip.
        let main = render(&view, &chrome_overlay(Overlay::None), 200, 40);
        assert!(main.contains("$5.00"), "compact strip shows the $ cost");
        // And in the full table (Stats overlay).
        let stats = render(&view, &chrome_overlay(Overlay::Stats), 200, 40);
        assert!(
            stats.contains("$5.00"),
            "full models table shows the $ cost"
        );
    }

    // --- Feature B: hit-testing + expand -----------------------------------

    fn key_of(entry: &Completed) -> ActivityKey {
        entry.activity_key().expect("request entry has a key")
    }

    #[test]
    fn hit_test_activity_maps_row_to_entry_and_ignores_outside() {
        let area = Rect {
            x: 0,
            y: 10,
            width: 80,
            height: 10,
        };
        let k1 = ActivityKey {
            at_ms: 1,
            method: "POST".into(),
            path: "/a".into(),
            status: 200,
        };
        let k2 = ActivityKey {
            at_ms: 2,
            method: "POST".into(),
            path: "/b".into(),
            status: 200,
        };
        // Entry 1 occupies rows 11..14 (expanded: 3 rows), entry 2 is a
        // folded-run HEADER on row 14.
        let chrome = ActivityChrome {
            area,
            hits: vec![
                ActivityHit {
                    key: k1.clone(),
                    y_start: 11,
                    height: 3,
                    kind: ActivityHitKind::Entry,
                },
                ActivityHit {
                    key: k2.clone(),
                    y_start: 14,
                    height: 1,
                    kind: ActivityHitKind::RunHeader { expanded: false },
                },
            ],
        };
        // Clicks within entry 1's row span (any of 11,12,13) map to k1.
        assert_eq!(
            hit_test_activity(&chrome, 5, 11),
            Some(ActivityClick::Entry(k1.clone()))
        );
        assert_eq!(
            hit_test_activity(&chrome, 5, 13),
            Some(ActivityClick::Entry(k1))
        );
        // Run header row 14: the marker zone toggles, the body expands.
        assert_eq!(
            hit_test_activity(&chrome, RUN_MARKER_ZONE - 1, 14),
            Some(ActivityClick::RunToggle(k2.clone()))
        );
        assert_eq!(
            hit_test_activity(&chrome, RUN_MARKER_ZONE, 14),
            Some(ActivityClick::RunExpand(k2))
        );
        // The title/border row (y=10) and below the last entry map to nothing.
        assert_eq!(hit_test_activity(&chrome, 5, 10), None);
        assert_eq!(hit_test_activity(&chrome, 5, 15), None);
        // Outside the panel horizontally / vertically → None.
        assert_eq!(hit_test_activity(&chrome, 99, 12), None);
        assert_eq!(hit_test_activity(&chrome, 5, 0), None);
    }

    #[test]
    fn tab_bar_records_seven_hit_targets_and_hit_test_maps_labels() {
        let view = view_with(Vec::new());
        let mut hits = None;
        let mut terminal = Terminal::new(TestBackend::new(200, 40)).expect("terminal");
        let chrome = chrome_overlay(Overlay::None);
        terminal
            .draw(|f| draw(f, Some(&view), &chrome, &mut hits))
            .expect("draw");
        let tabs = hits.expect("layout").tabs;
        assert_eq!(tabs.len(), TABS.len(), "one hit target per tab");
        for (hit, (label, overlay)) in tabs.iter().zip(TABS) {
            assert_eq!(hit.overlay, *overlay);
            assert_eq!(hit.area.width as usize, label.chars().count());
            // A click anywhere on the label maps to its overlay.
            assert_eq!(
                hit_test_tabs(&tabs, hit.area.x, hit.area.y),
                Some(*overlay),
                "{label}"
            );
            assert_eq!(
                hit_test_tabs(&tabs, hit.area.right() - 1, hit.area.y),
                Some(*overlay),
                "{label} right edge"
            );
        }
        // The separator between the first two labels maps to nothing.
        let gap_x = tabs[0].area.right() + 1;
        assert_eq!(hit_test_tabs(&tabs, gap_x, tabs[0].area.y), None);
        // A different row maps to nothing.
        assert_eq!(
            hit_test_tabs(&tabs, tabs[0].area.x, tabs[0].area.y + 1),
            None
        );
    }

    #[test]
    fn daily_tokens_chart_renders_title_legend_and_series() {
        use crate::dashboard::DailyUsageDoc;
        let mut view = view_with(Vec::new());
        let today = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            / 86_400;
        view.daily_usage = (0..7)
            .flat_map(|d| {
                vec![
                    DailyUsageDoc {
                        day: today - d,
                        group: "claude".into(),
                        model: "claude-fable-5".into(),
                        tokens_in: 100_000 * (d + 1),
                        tokens_out: 50_000,
                        cache_read: 0,
                        cache_creation: 0,
                    },
                    DailyUsageDoc {
                        day: today - d,
                        group: "codex".into(),
                        model: "gpt-5.6-sol".into(),
                        tokens_in: 40_000,
                        tokens_out: 10_000,
                        cache_read: 0,
                        cache_creation: 0,
                    },
                ]
            })
            .collect();
        let text = render(&view, &chrome_overlay(Overlay::Stats), 200, 50);
        assert!(text.contains("tokens per day"), "chart title visible");
        assert!(text.contains("fable-5"), "legend names the top model");
        assert!(
            text.contains("gpt-5.6-sol"),
            "legend names the second model"
        );
        assert!(text.contains('%'), "legend carries shares");
        // Empty data (or a short terminal) renders no chart, no panic.
        view.daily_usage.clear();
        let text = render(&view, &chrome_overlay(Overlay::Stats), 200, 50);
        assert!(!text.contains("tokens per day"));
    }

    #[test]
    fn daily_chart_ignores_future_days_and_never_panics() {
        use crate::dashboard::DailyUsageDoc;
        let mut view = view_with(Vec::new());
        let today = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            / 86_400;
        // One normal row + one FUTURE-dated row (clock skew in a replayed
        // log). Rendering must not panic and must still draw the chart.
        view.daily_usage = vec![
            DailyUsageDoc {
                day: today,
                group: "claude".into(),
                model: "claude-fable-5".into(),
                tokens_in: 10_000,
                tokens_out: 1_000,
                cache_read: 0,
                cache_creation: 0,
            },
            DailyUsageDoc {
                day: today + 30,
                group: "claude".into(),
                model: "claude-fable-5".into(),
                tokens_in: 99_000,
                tokens_out: 9_000,
                cache_read: 0,
                cache_creation: 0,
            },
        ];
        let text = render(&view, &chrome_overlay(Overlay::Stats), 200, 50);
        assert!(text.contains("tokens per day"), "chart still renders");
    }

    #[test]
    fn tab_row_stays_visible_under_every_overlay() {
        // Review R1 MUST-FIX 3: overlays used to paint over the tab strip
        // while its click targets stayed armed. The overlay rect now starts
        // below the tab row, so the labels must be visible from every
        // surface.
        let view = view_with(Vec::new());
        for (_, overlay) in TABS {
            let text = render(&view, &chrome_overlay(*overlay), 200, 40);
            assert!(
                text.contains("dashboard │ accounts"),
                "tab strip visible under {overlay:?}"
            );
        }
    }

    #[test]
    fn group_settings_bar_renders_and_records_clickable_segments() {
        let mut view = view_with(Vec::new());
        view.codex.available = true;
        view.codex.model = "gpt-5.6-sol".into();
        view.codex.effort = None;
        view.grok.available = true;
        view.grok.effort = Some("high".into());
        let mut hits = None;
        let mut terminal = Terminal::new(TestBackend::new(200, 40)).expect("terminal");
        let chrome = chrome_overlay(Overlay::None);
        terminal
            .draw(|f| draw(f, Some(&view), &chrome, &mut hits))
            .expect("draw");
        let settings = hits.expect("layout").settings;
        let actions: Vec<SettingAction> = settings.iter().map(|h| h.action).collect();
        assert!(actions.contains(&SettingAction::SchedMode));
        assert!(actions.contains(&SettingAction::CodexModel));
        assert!(actions.contains(&SettingAction::CodexEffort));
        assert!(actions.contains(&SettingAction::CodexFast));
        assert!(actions.contains(&SettingAction::GrokEffort));
        let text = render(&view, &chrome_overlay(Overlay::None), 200, 40);
        assert!(text.contains("sched"), "scheduler segment visible");
        assert!(text.contains("effort:bypass"), "codex bypass label visible");
        assert!(text.contains("effort:high"), "grok effort value visible");
    }

    #[test]
    fn context_menu_renders_from_pinned_identity_not_row_occupant() {
        use crate::routing::BackendGroup;
        use crate::scheduler::{AccountId, AccountSnapshot};
        let acct = |name: &str, paused: bool| AccountSnapshot {
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
            paused,
            limits: crate::config::AccountLimits::default(),
        };
        let mut view = view_with(Vec::new());
        view.snapshot.accounts = vec![acct("claude:a@x.com", false), acct("claude:b@x.com", true)];
        let mut chrome = chrome_overlay(Overlay::None);
        chrome.mode = Mode::ContextMenu { idx: 0, item: 0 };
        chrome.menu_anchor = Some((10, 6));
        // Pin B while the display index points at row 0: the menu must name B
        // and show B's paused state (resume), not the row occupant's.
        chrome.menu_account = Some("claude:b@x.com".into());
        let text = render(&view, &chrome, 200, 40);
        assert!(text.contains("b@x.com"), "menu titled from the pinned id");
        assert!(
            text.contains("resume"),
            "pause/resume label from the pinned id"
        );
        // A vanished pin renders as gone — never the row occupant.
        chrome.menu_account = Some("claude:gone@x.com".into());
        let text = render(&view, &chrome, 200, 40);
        assert!(
            text.contains("gone@x.com — gone"),
            "vanished pin marked gone"
        );
        assert!(
            !text.contains(" a@x.com — gone") && text.contains("gone@x.com"),
            "no fallback to the row occupant"
        );
    }

    #[test]
    fn perf_overlay_renders_series_health_and_honest_gaps() {
        let mut view = view_with(Vec::new());
        let today = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("now")
            .as_secs()
            / 86_400;
        let row = |model: &str, fast: Option<bool>| crate::dashboard::DailyPerfDoc {
            day: today,
            group: "codex".into(),
            model: model.into(),
            fast,
            requests: 10,
            ok: 9,
            errors: 1,
            tps_n: 8,
            output_tokens: 4_000,
            e2e_ms: 20_000,
            measured_n: 6,
            measured_output: 3_000,
            post_ttft_ms: 10_000,
            ttfb_n: 8,
            ttfb_ms_sum: 1_600,
        };
        view.daily_perf = vec![
            row("gpt-5.5", Some(true)),
            row("gpt-5.5", Some(false)),
            // Legacy pre-field history: unknown fast, its own series.
            row("gpt-5.5", None),
            // A series with NO measured samples: est column must be `—`.
            crate::dashboard::DailyPerfDoc {
                measured_n: 0,
                measured_output: 0,
                post_ttft_ms: 0,
                model: "opus".into(),
                group: "claude".into(),
                ..row("opus", Some(false))
            },
        ];
        let text = render(&view, &chrome_overlay(Overlay::Perf), 200, 50);
        assert!(text.contains("gpt-5.5⚡"), "fast=on series marked: {text}");
        assert!(text.contains("gpt-5.5?"), "unknown-fast series marked");
        assert!(
            text.contains("timing since"),
            "collection-start labeled from first TIMING day"
        );
        assert!(text.contains("est t/s"), "estimated column labeled est");
        assert!(
            text.contains("claude opus"),
            "claude series present in table"
        );
        // est for the measured series = 3000 tokens / 10s = 300t/s; e2e =
        // 4000/20s = 200t/s. The no-measured series renders `—` in est.
        assert!(text.contains("300t/s"), "estimated post-delta rate shown");
        assert!(text.contains("200t/s"), "e2e rate shown");
        // Provider health matrix shows both groups.
        assert!(
            text.contains("codex: n err% ttfb e2e est"),
            "health columns"
        );
        assert!(
            text.contains("claude: n err% ttfb e2e est"),
            "health columns"
        );

        // Empty state: honest hint, no fabricated zeros.
        view.daily_perf.clear();
        let text = render(&view, &chrome_overlay(Overlay::Perf), 200, 50);
        assert!(
            text.contains("no perf data yet"),
            "empty perf tab explains collection"
        );
    }

    #[test]
    fn config_editor_covers_every_schema_leaf() {
        // Trinity contract C6: the acceptance denominator is the WHOLE
        // schema. Authoritative reconciliation — flatten a default Config's
        // JSON into leaf paths and demand every one maps to an inventory
        // row (by covering prefix). A new schema field fails this test until
        // it is classified in `config_rows`.
        fn leaves(prefix: &str, v: &serde_json::Value, out: &mut Vec<String>) {
            match v {
                serde_json::Value::Object(map) => {
                    for (k, val) in map {
                        let path = if prefix.is_empty() {
                            k.clone()
                        } else {
                            format!("{prefix}.{k}")
                        };
                        leaves(&path, val, out);
                    }
                }
                _ => out.push(prefix.to_string()),
            }
        }
        // MAXIMAL fixture: every Option filled and every map/list non-empty,
        // so `skip_serializing_if` fields (api key, remote, client models,
        // events, pricing, limits…) appear as leaves and must be classified.
        let mut config = crate::config::Config::default();
        config.proxy.api_key = Some("lm-test".into());
        config.codex.client_model = Some("gpt-5.5".into());
        config.grok.client_model = Some("grok-4".into());
        config.tui_gradient.max_effort = Some("#ffffff".into());
        config.remote.host = Some("example.com".into());
        config.remote.port = Some(3456);
        config.remote.api_key = Some("lm-remote".into());
        config.pricing.insert(
            "gpt-5.5".into(),
            crate::pricing::ModelPrice {
                input: 1.0,
                output: 2.0,
                cache_read: 0.1,
                cache_creation: 1.25,
            },
        );
        config.paused_accounts.insert("a@x.com".into());
        config.account_limits.insert(
            "a@x.com".into(),
            crate::config::AccountLimits {
                five_hour_max: Some(0.5),
                seven_day_max: Some(0.5),
                fable_weekly_max: Some(0.5),
            },
        );
        config.events.push(crate::config::EventBanner {
            id: "e1".into(),
            from: "2026-01-01".into(),
            to: "2026-01-02".into(),
            content: "banner".into(),
        });
        let config = serde_json::to_value(&config).expect("json");
        let mut paths = Vec::new();
        leaves("", &config, &mut paths);
        assert!(
            paths.iter().any(|p| p == "proxy.api_key"),
            "maximal fixture must surface skip-serialized leaves"
        );
        // Covering prefixes, each tied to an inventory row (or the dedicated
        // surface the row names). Keep in sync with `config_rows`.
        const COVERED: &[&str] = &[
            "version",
            "proxy.port",
            "proxy.max_request_bytes",
            "proxy.api_key",
            "proxy.forward_idle_timeout_secs",
            "proxy.idle_probe",
            "upstream",
            "codex.",
            "grok.",
            "scheduler.",
            "routing.",
            "pricing",
            "raw_io.",
            "email_anonymous",
            "tui_effects",
            "tui_gradient.",
            "show_fable_weekly",
            "domain_abbrev",
            "quota_display",
            "paused_accounts",
            "account_limits",
            "events",
            "remote.",
            "accounts",
        ];
        for path in &paths {
            assert!(
                COVERED.iter().any(|c| path == c || path.starts_with(c)),
                "schema leaf {path:?} is not classified in the config editor \
                 inventory — add a row (or covering entry) for it"
            );
        }
        // Bidirectional: a COVERED entry matching no leaf is stale coverage
        // (it would silently swallow future leaves under a dead prefix).
        for c in COVERED {
            assert!(
                paths.iter().any(|p| p == c || p.starts_with(c)),
                "coverage entry {c:?} matches no schema leaf — remove or fix it"
            );
        }
        // And the inventory renders with honest labels.
        let view = view_with(Vec::new());
        let chrome = chrome_overlay(Overlay::Config);
        let rows = config_rows(&view, &chrome);
        for section in [
            "scheduler",
            "codex",
            "grok",
            "display",
            "routing",
            "raw-io",
            "daemon",
        ] {
            assert!(
                rows.iter().any(|r| r.section == section),
                "{section} section present"
            );
        }
        assert!(rows.iter().any(|r| r.note.contains("secret")));
        let text = render(&view, &chrome, 200, 50);
        assert!(text.contains("live"), "live state label rendered");
        assert!(text.contains("restart"), "restart state label rendered");
    }

    #[test]
    fn config_editor_click_outside_value_cells_changes_nothing() {
        // Contract C6: only the value cell is a control. A click elsewhere
        // must map to no action.
        let view = view_with(Vec::new());
        let chrome = chrome_overlay(Overlay::Config);
        let mut terminal = Terminal::new(TestBackend::new(200, 50)).expect("terminal");
        let mut hits = None;
        terminal
            .draw(|f| draw(f, Some(&view), &chrome, &mut hits))
            .expect("draw");
        let hits = hits.expect("main chrome").config_rows;
        assert!(!hits.is_empty(), "editable value cells recorded");
        for h in &hits {
            assert!(h.area.x > 2, "value cells start after the label column");
        }
        // A label-column click (x=1) hits no control on any row.
        assert!(
            !hits.iter().any(|h| 1 >= h.area.x && 1 < h.area.right()),
            "a label-column click hits no control"
        );
    }
    #[test]
    fn tab_bar_renders_all_labels_and_marks_the_active_surface() {
        let view = view_with(Vec::new());
        for (label, overlay) in TABS {
            let text = render(&view, &chrome_overlay(*overlay), 200, 40);
            assert!(text.contains(label), "{label} visible");
        }
        // Misc/config overlays render their surfaces.
        let text = render(&view, &chrome_overlay(Overlay::Misc), 200, 40);
        assert!(text.contains("keys"), "misc shows keybindings");
        let text = render(&view, &chrome_overlay(Overlay::Config), 200, 40);
        assert!(text.contains("scheduler"), "config shows scheduler block");
        assert!(text.contains("quota fill"), "config shows display block");
    }

    #[test]
    fn click_expand_recorded_layout_round_trips_to_detail() {
        // Render once to capture the hit layout, find the row a click lands on,
        // set that key expanded, and re-render: the detail lines appear.
        let entry = completed_request(
            7_000,
            Some("claude"),
            Some("claude-opus-4-8"),
            200_000,
            100_000,
            200,
        );
        let key = key_of(&entry);
        let mut view = view_with(Vec::new());
        view.completed = vec![entry];

        // Capture layout (collapsed).
        let mut hits = None;
        let mut terminal = Terminal::new(TestBackend::new(200, 40)).expect("terminal");
        let chrome = chrome_overlay(Overlay::None);
        terminal
            .draw(|f| draw(f, Some(&view), &chrome, &mut hits))
            .expect("draw");
        let layout = hits.expect("activity layout recorded").activity;
        assert!(!layout.hits.is_empty(), "the request row is a hit target");
        let hit = &layout.hits[0];
        // A click on the row's first line maps back to the same key.
        assert_eq!(
            hit_test_activity(&layout, layout.area.x + 1, hit.y_start),
            Some(ActivityClick::Entry(key.clone()))
        );

        // Now render expanded and confirm the detail lines show.
        let mut expanded_chrome = chrome_overlay(Overlay::None);
        expanded_chrome.expanded_activity = Some(key);
        let text = render(&view, &expanded_chrome, 200, 40);
        assert!(text.contains("cache_read"), "expanded detail shows tokens");
        assert!(
            text.contains("$"),
            "expanded detail shows per-component cost"
        );
        assert!(text.contains('▾'), "expanded row shows the open marker");
    }

    #[test]
    fn notes_are_not_expandable_hit_targets() {
        let mut view = view_with(Vec::new());
        view.completed = vec![Completed {
            at: UNIX_EPOCH + Duration::from_millis(1),
            body: CompletedBody::Note {
                text: "switch a → b".into(),
                error: false,
            },
        }];
        let mut hits = None;
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).expect("terminal");
        let chrome = chrome_overlay(Overlay::None);
        terminal
            .draw(|f| draw(f, Some(&view), &chrome, &mut hits))
            .expect("draw");
        let layout = hits.expect("layout").activity;
        assert!(
            layout.hits.is_empty(),
            "a note line is not a clickable hit target"
        );
    }

    // --- fable-usage U9a: Fbl gauge column, opt-in toggle (default ON) -------

    /// An account carrying a Fable weekly window (critical/active) plus a 5h
    /// window, used to prove the Fbl gauge appears with the toggle ON and is
    /// fully absent (columns unchanged) with it OFF.
    fn fable_account() -> crate::scheduler::AccountSnapshot {
        use crate::routing::BackendGroup;
        use crate::scheduler::window::{
            LimitSeverity, QuotaWindow, ScopedQuotaWindow, WindowSource,
        };
        use crate::scheduler::{AccountId, AccountSnapshot};
        let now = SystemTime::now();
        AccountSnapshot {
            id: AccountId("claude:me@example.com".into()),
            healthy: true,
            credential_kind: "oauth",
            group: BackendGroup::Claude,
            five_hour: Some(QuotaWindow {
                utilization: 0.42,
                resets_at: now + Duration::from_secs(3_600),
                fetched_at: now,
                source: WindowSource::UsagePoll,
            }),
            seven_day: None,
            // Test helpers default `scoped_limits` empty; this one carries a
            // live, engaged Fable weekly bucket at 97%.
            scoped_limits: vec![ScopedQuotaWindow {
                scope_label: "Fable".into(),
                window: QuotaWindow {
                    utilization: 0.97,
                    resets_at: now + Duration::from_secs(80_000),
                    fetched_at: now,
                    source: WindowSource::UsagePoll,
                },
                severity: LimitSeverity::Critical,
                is_active: true,
            }],
            scoped_cooldowns: Vec::new(),
            cooldown_until: None,
            cooldown_source: None,
            in_flight: 0,
            token_expires_at_ms: None,
            last_refresh_ms: None,
            paused: false,
            limits: crate::config::AccountLimits::default(),
        }
    }

    /// U9a acceptance: with the toggle ON the wide accounts table renders the
    /// `Fbl` gauge column + the Fable percent; with it OFF neither appears and
    /// the pre-W3 5h column is unchanged. One view, both toggle states, so the
    /// OFF assertions can't pass vacuously (the ON render proves the data is
    /// present at this width).
    #[test]
    fn fable_gauge_renders_when_enabled_and_vanishes_when_disabled() {
        use crate::routing::BackendGroup;
        use crate::scheduler::AccountId;

        let mut view = view_with(Vec::new());
        view.snapshot.accounts = vec![fable_account()];
        view.snapshot.current.insert(
            BackendGroup::Claude,
            AccountId("claude:me@example.com".into()),
        );

        // Toggle ON (wide layout, width ≥ WIDE_TABLE_AT): Fbl header + the
        // in-bar reset countdown (80_000s out → "22h 13m", critical `!`).
        view.show_fable_weekly = true;
        let on = render(&view, &chrome_overlay(Overlay::None), 200, 20);
        assert!(
            on.contains("Fbl"),
            "toggle ON: wide table shows the Fbl gauge column header:\n{on}"
        );
        assert!(
            on.contains("22h 13m"),
            "toggle ON: Fable weekly in-bar countdown rendered:\n{on}"
        );
        assert!(
            on.contains("3%!"),
            "toggle ON: Fable remaining-percent label carries the critical `!`:\n{on}"
        );
        // The 5h gauge is still present alongside it (+3600s reads "59m …" by
        // the time the render clock ticks past the helper's `now`) with its
        // percent label restored on the right.
        assert!(on.contains("59m"), "toggle ON: 5h gauge still rendered");
        assert!(
            on.contains("58%"),
            "toggle ON: 5h remaining-percent label rendered (42% used)"
        );

        // Toggle OFF: no Fbl column, no Fable countdown, 5h column unchanged.
        view.show_fable_weekly = false;
        let off = render(&view, &chrome_overlay(Overlay::None), 200, 20);
        assert!(
            !off.contains("Fbl"),
            "toggle OFF: no Fbl column header:\n{off}"
        );
        assert!(
            !off.contains("22h 13m"),
            "toggle OFF: no Fable weekly countdown:\n{off}"
        );
        assert!(
            off.contains("59m"),
            "toggle OFF: 5h gauge unchanged:\n{off}"
        );
    }

    /// Cap receipt (Z 2026-07-13): the account name column is a fixed
    /// `Length` clamped to `NAME_COL_MAX` (20). A longer name renders its
    /// prefix in the table row but is clipped at the cap — leftover terminal
    /// width no longer widens the column, superseding the 2026-07-09
    /// leftover-allocation behavior.
    #[test]
    fn account_name_column_clips_at_name_col_max() {
        use crate::scheduler::AccountId;

        let mut view = view_with(Vec::new());
        let mut a = fable_account();
        a.id = AccountId("claude:really.long.account.name.overflow@example.com".into());
        view.snapshot.accounts = vec![a];

        // Wide layout so leftover width is maximal — under the old `Min`
        // behavior the whole name would have fit; under the `Length` cap it
        // does not.
        let rows = render_rows(&view, &chrome_overlay(Overlay::None), 200, 40);

        let name_row = rows
            .iter()
            .find(|line| line.contains("really.long.account"))
            .unwrap_or_else(|| {
                panic!(
                    "expected a table row rendering the name prefix:\n{}",
                    rows.join("\n")
                )
            });
        assert!(
            !name_row.contains("name.overflow"),
            "name column clipped at NAME_COL_MAX (chars past the 20-col cap dropped):\n{name_row}"
        );
        assert!(
            rows.iter().any(|line| line.contains("account")),
            "the accounts table renders its `account` header word:\n{}",
            rows.join("\n")
        );
    }

    /// Leftover-width receipt (Z 2026-07-13): with every column a fixed
    /// `Length`, the leftover terminal width is poured into the quota gauge
    /// BARS instead of dying as dead space on the right (~col 120) — the
    /// follow-up to the NAME_COL_MAX cap. A wide (200-col) render reaches near
    /// the right edge; the same account at 120 cols stays within the terminal.
    #[test]
    fn accounts_gauges_absorb_leftover_width() {
        let mut view = view_with(Vec::new());
        view.snapshot.accounts = vec![fable_account()];
        view.show_fable_weekly = true;

        // Wide: leftover width flows into the 3 gauge bars, so the row's content
        // reaches near the 200-col right edge (was dying at ~120 dead-space).
        let wide = render_rows(&view, &chrome_overlay(Overlay::None), 200, 40);
        let wide_row = wide
            .iter()
            .find(|line| line.contains("me@"))
            .unwrap_or_else(|| {
                panic!(
                    "expected a table row with the account name:\n{}",
                    wide.join("\n")
                )
            });
        assert!(
            wide_row.trim_end().chars().count() >= 170,
            "leftover width goes to the gauge bars — the row reaches near the \
             200-col right edge instead of dying at ~120:\n{wide_row}"
        );

        // Narrow: the same row still fits inside a 120-col terminal (the bars
        // only absorb what is actually left over).
        let narrow = render_rows(&view, &chrome_overlay(Overlay::None), 120, 40);
        let narrow_row = narrow
            .iter()
            .find(|line| line.contains("me@"))
            .unwrap_or_else(|| {
                panic!(
                    "expected a table row with the account name:\n{}",
                    narrow.join("\n")
                )
            });
        assert!(
            narrow_row.trim_end().chars().count() <= 120,
            "at 120 cols the same row stays within the terminal width:\n{narrow_row}"
        );
    }

    /// Z 2026-07-13 screenshots — a 1-col layout flap at 149/150: the old fixed
    /// `WIDE_TABLE_AT=150` predated the NAME_COL_MAX cap, so at ~149 cols the
    /// wide column set (req/tok) was hidden and the width poured into fat bars.
    /// The wide set must engage as soon as it actually fits.
    #[test]
    fn accounts_wide_set_fits_before_150() {
        let mut view = view_with(Vec::new());
        view.snapshot.accounts = vec![fable_account()];
        view.show_fable_weekly = true;

        // 149 cols: wide set already fits → header carries req + tok (under the
        // old 150 threshold this row would be the narrow set, no req/tok).
        let at_149 = render_rows(&view, &chrome_overlay(Overlay::None), 149, 40);
        let header_149 = at_149
            .iter()
            .find(|line| line.contains("account") && line.contains("5h"))
            .unwrap_or_else(|| panic!("expected the accounts header row:\n{}", at_149.join("\n")));
        assert!(
            header_149.contains("req") && header_149.contains("tok"),
            "at 149 cols the wide set fits — header shows req + tok:\n{header_149}"
        );

        // 110 cols: too narrow for the wide set → header has 5h but not req.
        let at_110 = render_rows(&view, &chrome_overlay(Overlay::None), 110, 40);
        let header_110 = at_110
            .iter()
            .find(|line| line.contains("account") && line.contains("5h"))
            .unwrap_or_else(|| panic!("expected the accounts header row:\n{}", at_110.join("\n")));
        assert!(
            !header_110.contains("req"),
            "at 110 cols the narrow set still exists — no req column:\n{header_110}"
        );
    }

    /// The narrow layout compresses the gauge to an inline `F 97%` marker
    /// (no third gauge+reset pair) when the toggle is ON, and drops it when OFF.
    #[test]
    fn fable_gauge_narrow_uses_compact_marker_gated_by_toggle() {
        use crate::routing::BackendGroup;
        use crate::scheduler::AccountId;

        let mut view = view_with(Vec::new());
        view.snapshot.accounts = vec![fable_account()];
        view.snapshot.current.insert(
            BackendGroup::Claude,
            AccountId("claude:me@example.com".into()),
        );

        // Width < WIDE_TABLE_AT → narrow layout. The compact marker carries
        // the top countdown unit + the critical `!` (80_000s out → "F 22h!").
        view.show_fable_weekly = true;
        let on = render(&view, &chrome_overlay(Overlay::None), 120, 20);
        assert!(
            on.contains("F 22h!"),
            "narrow toggle ON: compact `F 22h!` marker rendered:\n{on}"
        );

        view.show_fable_weekly = false;
        let off = render(&view, &chrome_overlay(Overlay::None), 120, 20);
        assert!(
            !off.contains("F 22h"),
            "narrow toggle OFF: no Fable marker:\n{off}"
        );
    }

    /// An account with NO Fable scope renders the neutral cold state in the Fbl
    /// slot (never a crash/blank), and does not disturb the 5h/7d columns.
    #[test]
    fn fable_gauge_cold_state_for_account_without_fable_scope() {
        use crate::routing::BackendGroup;
        use crate::scheduler::window::{QuotaWindow, WindowSource};
        use crate::scheduler::{AccountId, AccountSnapshot};

        let now = SystemTime::now();
        let mut view = view_with(Vec::new());
        view.snapshot.accounts = vec![AccountSnapshot {
            id: AccountId("claude:cold@example.com".into()),
            healthy: true,
            credential_kind: "oauth",
            group: BackendGroup::Claude,
            // Both 5h and 7d populated, so the only cold cell in the row is the
            // absent Fable one — the assertion below is then attributable to it.
            five_hour: Some(QuotaWindow {
                utilization: 0.10,
                resets_at: now + Duration::from_secs(3_600),
                fetched_at: now,
                source: WindowSource::UsagePoll,
            }),
            seven_day: Some(QuotaWindow {
                utilization: 0.20,
                resets_at: now + Duration::from_secs(600_000),
                fetched_at: now,
                source: WindowSource::UsagePoll,
            }),
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
        view.snapshot.current.insert(
            BackendGroup::Claude,
            AccountId("claude:cold@example.com".into()),
        );
        view.show_fable_weekly = true;
        // Renders without panicking and the Fbl column header is still present.
        let text = render(&view, &chrome_overlay(Overlay::None), 200, 20);
        assert!(
            text.contains("Fbl"),
            "cold account still gets the Fbl column"
        );
        assert!(
            text.contains("cold"),
            "absent Fable scope reads as cold state"
        );
    }

    /// Regression (W2 `is_active` misread): a Fable weekly at 76% /
    /// `warning` / `is_active: true` has real headroom — `is_active` marks the
    /// representative limit, NOT exhaustion — so the gauge must render its normal
    /// utilization hue with NO forced-red `!` marker (contrast `fable_account`'s
    /// 97%/critical row, which legitimately gets the `!`).
    #[test]
    fn fable_gauge_with_headroom_is_not_forced_red() {
        use crate::routing::BackendGroup;
        use crate::scheduler::window::{
            LimitSeverity, QuotaWindow, ScopedQuotaWindow, WindowSource,
        };
        use crate::scheduler::{AccountId, AccountSnapshot};

        let now = SystemTime::now();
        let mut view = view_with(Vec::new());
        view.snapshot.accounts = vec![AccountSnapshot {
            id: AccountId("claude:icedac@example.com".into()),
            healthy: true,
            credential_kind: "oauth",
            group: BackendGroup::Claude,
            five_hour: Some(QuotaWindow {
                utilization: 0.42,
                resets_at: now + Duration::from_secs(3_600),
                fetched_at: now,
                source: WindowSource::UsagePoll,
            }),
            seven_day: None,
            // 76% util, warning severity, is_active=true → the real headroom
            // case that must NOT read red.
            scoped_limits: vec![ScopedQuotaWindow {
                scope_label: "Fable".into(),
                window: QuotaWindow {
                    utilization: 0.76,
                    resets_at: now + Duration::from_secs(80_000),
                    fetched_at: now,
                    source: WindowSource::UsagePoll,
                },
                severity: LimitSeverity::Warning,
                is_active: true,
            }],
            scoped_cooldowns: Vec::new(),
            cooldown_until: None,
            cooldown_source: None,
            in_flight: 0,
            token_expires_at_ms: None,
            last_refresh_ms: None,
            paused: false,
            limits: crate::config::AccountLimits::default(),
        }];
        view.snapshot.current.insert(
            BackendGroup::Claude,
            AccountId("claude:icedac@example.com".into()),
        );
        view.show_fable_weekly = true;

        // Narrow marker: the countdown (`F 22h`) with no critical `!`
        // (is_active must not force the over-threshold marker at 76%).
        let narrow = render(&view, &chrome_overlay(Overlay::None), 120, 20);
        assert!(
            narrow.contains("F 22h"),
            "76%/warning/is_active renders its normal countdown marker:\n{narrow}"
        );
        assert!(
            !narrow.contains("22h!"),
            "is_active alone must NOT force the red-critical `!` marker:\n{narrow}"
        );
    }

    /// Regression (fable-usage reset race): right after a weekly reset the
    /// window is expired (util → 0) but its `severity` field can still be a
    /// stale `Critical` until the next usage poll. The gauge must key off the
    /// reset-aware `is_constraining` (which short-circuits on `is_expired`), so
    /// a just-reset window renders its honest full-quota `F 100%` (remaining
    /// mode) with NO forced-red `!` — not the old red critical flash.
    #[test]
    fn fable_gauge_reset_window_is_not_forced_red() {
        use crate::routing::BackendGroup;
        use crate::scheduler::window::{
            LimitSeverity, QuotaWindow, ScopedQuotaWindow, WindowSource,
        };
        use crate::scheduler::{AccountId, AccountSnapshot};

        let now = SystemTime::now();
        let mut view = view_with(Vec::new());
        view.snapshot.accounts = vec![AccountSnapshot {
            id: AccountId("claude:icedac@example.com".into()),
            healthy: true,
            credential_kind: "oauth",
            group: BackendGroup::Claude,
            five_hour: Some(QuotaWindow {
                utilization: 0.42,
                resets_at: now + Duration::from_secs(3_600),
                fetched_at: now,
                source: WindowSource::UsagePoll,
            }),
            seven_day: None,
            // EXPIRED Fable weekly (resets_at in the past) whose `severity` is
            // still a stale `Critical`: util is 0 after reset but severity has
            // not been refreshed by a poll yet. Must NOT read red.
            scoped_limits: vec![ScopedQuotaWindow {
                scope_label: "Fable".into(),
                window: QuotaWindow {
                    utilization: 0.97,
                    resets_at: now - Duration::from_secs(60),
                    fetched_at: now,
                    source: WindowSource::UsagePoll,
                },
                severity: LimitSeverity::Critical,
                is_active: true,
            }],
            scoped_cooldowns: Vec::new(),
            cooldown_until: None,
            cooldown_source: None,
            in_flight: 0,
            token_expires_at_ms: None,
            last_refresh_ms: None,
            paused: false,
            limits: crate::config::AccountLimits::default(),
        }];
        view.snapshot.current.insert(
            BackendGroup::Claude,
            AccountId("claude:icedac@example.com".into()),
        );
        view.show_fable_weekly = true;

        // Expired → effective utilization 0 → remaining-mode label `F 100%`
        // (full quota is back), and because `is_constraining` short-circuits
        // on the expired window the stale `Critical` severity does NOT force
        // the red-critical `!` marker.
        let narrow = render(&view, &chrome_overlay(Overlay::None), 120, 20);
        assert!(
            narrow.contains("F 100%"),
            "expired/reset Fable window renders its honest full-quota 100%:\n{narrow}"
        );
        assert!(
            !narrow.contains("100%!"),
            "stale-critical severity on an expired window must NOT force the red `!`:\n{narrow}"
        );
    }

    // --- email_anonymous: render-layer masking (SSOT E4) --------------------

    /// The email that must never survive an anonymous render, planted on every
    /// email-bearing surface: table, detail, current/next, poller backoff,
    /// in-flight + completed + note activity lines, logs, model accounts,
    /// heatmap cells, sessions, footer status.
    const LEAK: &str = "me@leak-domain.com";

    fn email_everywhere_view() -> DashboardView {
        use super::super::activity::{Completed, CompletedBody, InFlight, Totals};
        use super::super::{LastSwitch, PollHealth};
        use crate::routing::BackendGroup;
        use crate::scheduler::{AccountId, AccountSnapshot};

        let mut view = view_with(vec![crate::dashboard::ModelUsageDoc {
            group: "claude".into(),
            model: "claude-opus-4-8".into(),
            requests: 1,
            ok: 1,
            errors: 0,
            tokens_in: 10,
            tokens_out: 5,
            cache_read: None,
            cache_creation: None,
            last_used_ms: 1,
            in_flight: 0,
            accounts: vec![crate::dashboard::ModelAccountDoc {
                name: LEAK.into(),
                requests: 1,
                ok: 1,
                errors: 0,
                tokens_in: 10,
                tokens_out: 5,
            }],
            efforts: Vec::new(),
            endpoints: Vec::new(),
            cost_usd: 0.0,
        }]);
        view.email_anonymous = true;
        view.snapshot.accounts = vec![AccountSnapshot {
            id: AccountId(LEAK.into()),
            healthy: true,
            credential_kind: "oauth",
            group: BackendGroup::Claude,
            five_hour: None,
            seven_day: None,
            scoped_limits: Vec::new(),
            scoped_cooldowns: Vec::new(),
            cooldown_until: None,
            cooldown_source: None,
            in_flight: 1,
            token_expires_at_ms: None,
            last_refresh_ms: None,
            paused: false,
            limits: crate::config::AccountLimits::default(),
        }];
        view.snapshot
            .current
            .insert(BackendGroup::Claude, AccountId(LEAK.into()));
        view.last_switch = Some(LastSwitch {
            from: Some(LEAK.into()),
            to: LEAK.into(),
            reason: Some("manual".into()),
            at: UNIX_EPOCH,
        });
        view.poll_health.insert(
            LEAK.into(),
            PollHealth {
                last_ok: Some(UNIX_EPOCH),
                consecutive_failures: 2,
                next_at: UNIX_EPOCH,
            },
        );
        view.session_totals.insert(LEAK.into(), Totals::default());
        view.in_flight = vec![InFlight {
            id: 7,
            method: "POST".into(),
            path: "/v1/messages".into(),
            account: Some(LEAK.into()),
            group: Some("claude".into()),
            model: Some("claude-opus-4-8".into()),
            effort: None,
            fast: false,
            kind: None,
            started_at: UNIX_EPOCH,
        }];
        view.completed = vec![
            Completed {
                at: UNIX_EPOCH + Duration::from_millis(2),
                body: CompletedBody::Request {
                    id: 1,
                    method: "POST".into(),
                    path: "/v1/messages".into(),
                    account: Some(LEAK.into()),
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
                    kind: None,
                    excerpt: None,
                },
            },
            Completed {
                at: UNIX_EPOCH + Duration::from_millis(1),
                body: CompletedBody::Note {
                    text: format!("switch {LEAK} → {LEAK} (manual)"),
                    error: false,
                },
            },
        ];
        view.logs = vec![LogLine {
            level: tracing::Level::INFO,
            text: format!("proxy: account {LEAK} refreshed"),
        }];
        view.windowed = vec![crate::dashboard::WindowedStatsDoc {
            window: "24h".into(),
            window_secs: 86_400,
            cells: vec![crate::dashboard::WindowedCellDoc {
                group: "claude".into(),
                model: "claude-opus-4-8".into(),
                account: LEAK.into(),
                requests: 1,
                ok: 1,
                errors: 0,
                tokens_in: 10,
                tokens_out: 5,
                cache_read: 0,
                cache_creation: 0,
                tokens: 15,
            }],
        }];
        view
    }

    fn email_everywhere_chrome(overlay: Overlay) -> Chrome {
        let mut chrome = chrome_overlay(overlay);
        chrome.status_line = Some(format!("switched to {LEAK} (manual)"));
        chrome.sessions = vec![crate::session::Session {
            user_id: Some("acct_x".into()),
            requests: 1,
            tokens_in: 10,
            tokens_out: 5,
            models: vec!["claude-opus-4-8".into()],
            accounts: vec![LEAK.into()],
            account_rotations: 0,
            first_ms: 1,
            last_ms: 2,
            duration_ms_sum: 0,
            timed_requests: 0,
            tokens_out_timed: 0,
            confidence: crate::session::Confidence::High,
        }];
        chrome
    }

    /// E4: with the setting ON, no surface (MAIN or any overlay) renders the
    /// raw email; the deterministic alias appears instead. The control render
    /// (setting OFF) proves the raw email WOULD be visible at this size, so
    /// the masked assertion can't pass vacuously through truncation.
    #[test]
    fn email_anonymous_masks_every_render_surface() {
        for overlay in [
            Overlay::None,
            Overlay::Accounts,
            Overlay::Stats,
            Overlay::Logs,
            Overlay::Sessions,
        ] {
            let mut view = email_everywhere_view();
            let chrome = email_everywhere_chrome(overlay);

            view.email_anonymous = false;
            let control = render(&view, &chrome, 220, 50);
            assert!(
                control.contains("leak-domain"),
                "control ({overlay:?}): raw email must be visible when masking is off"
            );

            view.email_anonymous = true;
            let masked = render(&view, &chrome, 220, 50);
            assert!(
                !masked.contains("leak-domain"),
                "masked ({overlay:?}): raw email leaked:\n{masked}"
            );
            assert!(
                masked.contains(&crate::demo::alias_always(LEAK)),
                "masked ({overlay:?}): alias not rendered"
            );
        }
    }

    /// Both ON: demo mode aliases at config load (names arrive pre-aliased in
    /// the doc), and render-layer masking aliases again — the result is still
    /// a pool alias, never a real email (SSOT T2).
    #[test]
    fn email_anonymous_composes_with_demo_aliases() {
        let alias_of_alias = crate::demo::alias_always(&crate::demo::alias_always(LEAK));
        assert!(
            alias_of_alias.ends_with("@example.com"),
            "still a fake-pool email: {alias_of_alias}"
        );
        assert!(!alias_of_alias.contains("leak-domain"));
    }

    // ---- issue #70: compressed accounts row, info relocated to detail ----

    /// The wide accounts row keeps ≤8 clusters (gauges + traffic); `auth`,
    /// the two `reset` columns, and the token cluster left the row — they
    /// live in the always-on detail pane instead, so nothing is unreachable.
    #[test]
    fn accounts_row_drops_auth_reset_token_and_detail_carries_them() {
        use crate::routing::BackendGroup;
        use crate::scheduler::AccountId;

        let mut view = view_with(Vec::new());
        view.snapshot.accounts = vec![fable_account()];
        view.snapshot.current.insert(
            BackendGroup::Claude,
            AccountId("claude:me@example.com".into()),
        );
        view.show_fable_weekly = true;

        let rows = render_rows(&view, &chrome_overlay(Overlay::None), 200, 30);
        let header = rows
            .iter()
            .find(|r| r.contains(" account ") && r.contains("status"))
            .expect("accounts header row");
        for kept in [
            "group", "account", "status", "5h", "7d", "7d Fbl", "if", "req", "tok",
        ] {
            assert!(header.contains(kept), "header keeps `{kept}`:\n{header}");
        }
        for gone in ["auth", "reset", "token"] {
            assert!(
                !header.contains(gone),
                "header must not carry `{gone}` anymore:\n{header}"
            );
        }
        let text = rows.join("\n");
        assert!(
            text.contains(" token "),
            "detail pane still surfaces the token line:\n{text}"
        );
        assert!(
            text.contains("resets"),
            "detail pane still surfaces window resets:\n{text}"
        );
        assert!(
            text.contains("oauth"),
            "detail pane still surfaces the auth kind:\n{text}"
        );
    }

    /// Narrow (<80col) keeps the same reduced column set and renders without
    /// panic — the compressed mode may clip, never crash (issue #70).
    #[test]
    fn accounts_row_narrow_uses_reduced_columns_without_panic() {
        use crate::routing::BackendGroup;
        use crate::scheduler::AccountId;

        let mut view = view_with(Vec::new());
        view.snapshot.accounts = vec![fable_account()];
        view.snapshot.current.insert(
            BackendGroup::Claude,
            AccountId("claude:me@example.com".into()),
        );
        let rows = render_rows(&view, &chrome_overlay(Overlay::None), 70, 30);
        let header = rows
            .iter()
            .find(|r| r.contains("5h") && r.contains("7d"))
            .expect("narrow accounts header row");
        for gone in ["auth", "reset", "token"] {
            assert!(
                !header.contains(gone),
                "narrow header must not carry `{gone}`:\n{header}"
            );
        }
    }
}
