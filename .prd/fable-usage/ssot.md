# SSOT — fable-usage round 2 (reset-aware red + fable head routing)

**Frozen:** 2026-07-04 · **Repo:** github.com/2lab-ai/llmux · **Branch:** `feat/fable-usage`
· **Worktree:** `/Users/zhugehyuk/2lab.ai/llmux-wt/fable-usage`
· **Source:** user dictation via `/tmp/handoff-llmux-fable-2026-07-04.md` (prior session's handoff).

> Note: the handoff referenced `.prd/fable-usage/ssot.md`/`loop.md`/`13-usage-raw-sources.md`
> as existing — they were NOT committed and did not exist in this branch (git clean). This file
> is materialized fresh for round 2, scoped to the two NEW goals below. The already-shipped
> v0.2.13 feature set (fable weekly usage, W2 scope-aware cooldown, is_active fix) is DONE and
> is not re-litigated here.

## Verbatim user intent

1. **BUG (reset-aware):** "fable이 0%로 리셋됐는데 F 0%! 빨강으로 뜨는 버그 수정" — after a
   weekly reset, the Fable gauge shows `F 0%!` in red. Red must become reset-aware: an
   expired/reset Fable window must render non-red even if the stale `severity` is still "critical".
2. **FEATURE (architecture):** "fable 모델 요청을 별도 fable head 그룹/구독으로 라우팅하도록
   아키텍처 변경 (전략가 상의 먼저)" — route Fable-model requests to a separate fable head
   group/subscription so fable traffic stops churning the shared claude `current` account.
   **User explicitly required a strategist (gpt55 zhuge/elon) design consult BEFORE coding.**

## Universe (spec ∪ evidence)

### Goal 1 — reset-aware red (confirmed mechanism, live evidence 2026-07-04)
- Root cause: display red keys on the non-reset-aware `severity` field, while
  `effective_utilization`/`is_expired` ARE reset-aware. Between a weekly reset and the next
  usage poll, `util=0` + `severity=critical`(stale) → red at 0% → `F 0%!`.
  - Live proof: `claude:dev1` currently `util 0.01 / sev normal` (post-poll recovered) while
    genuinely-exhausted accounts (`ai3`,`info`,`notify`,`ai`) show `util 1.0 / sev critical`
    (correct). The buggy window is the transient reset→next-poll gap.
- Single-source primitive already exists: `ScopedQuotaWindow::is_constraining(now, critical_util)`
  (`src/scheduler/window.rs:130`) — guards `if window.is_expired(now) { return false }` first.
- Fix sites:
  - TUI `src/tui/ui.rs:1096` `fable_gauge_cell`: `scoped.severity == LimitSeverity::Critical`
    → `scoped.is_constraining(now, CRITICAL_UTILIZATION)` (const = 0.95, `scheduler/mod.rs:53`).
  - islands `llmux-islands/.../UsageTiles.swift:709` `emphasizeCritical`: `severity=="critical"`
    → consume a daemon-emitted reset-aware bool (Swift has no `is_expired`). Daemon emits it
    from `ScopedWindowDoc` construction (`src/dashboard.rs:1115` `scoped_window_doc(s, now)`).
- Regression tests: TUI cell (mirror `fable_gauge_with_headroom_is_not_forced_red`) + doc
  round-trip + Swift.

### Goal 2 — fable head routing (design open → strategist-gated)
- Current arch: `BackendGroup{Claude,Codex}` derived from credential KIND (`routing.rs:31`);
  fable is a Claude-family model. One sticky `current` slot per group (per-group current map,
  `select.rs:1040`). W2 threads `RequestScope{Fable,NonFable}` through `gate_scoped`/`pick_scoped`
  and already excludes fable-dead accounts + "switches off a Fable-dead sticky current" — but
  both scopes share ONE Claude `current`, so a fable pick MOVES the shared current and churns
  non-fable stickiness.
- Design axis (resolve WITH strategists):
  - (b) per-scope current slot: key sticky current by `(BackendGroup, RequestScope)`. Cheapest,
    reuses W2. Isolates stickiness but fable still draws from the whole claude pool.
  - (c) dedicated fable-head subscription: config-designate account(s) as the fable head; route
    fable there with its own current slot + defined fallback. Most literal reading of "별도
    fable head 구독".
- Decision recorded post-consult in `loop.md` W0.

## Tensions
- "별도 그룹/구독" wording spans (b) stickiness-isolation and (c) dedicated-account. Default
  lean = strategist recommendation; user flips if wrong. → resolve in W0.

## Security Notes
- None. Source is the user's own handoff + live daemon state on localhost. No embedded
  instructions from untrusted content.

## Gates (🚪 user approval)
- main-repo `v*` tag push (release) — user-gated activation per handoff cadence.
- homebrew-tap changes go via PR the USER merges (auto-mode classifier blocks self-merge).
- daemon activation on this machine (self-routing restart) is the user's.
