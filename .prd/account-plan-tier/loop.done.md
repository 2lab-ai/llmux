# Account plan tier — done

## R1 — closed

Shipped v0.2.26 (PR #219). Every acceptance row GREEN.

### Closed rows

## Round 1 — 2026-10-26

| WU | Scope (files) | Gate | Verify |
| --- | --- | --- | --- |
| 1 | `src/auth/plan.rs` (new), `src/auth/profile.rs`, `src/proxy/plans.rs` (new), `src/proxy/server.rs`, `src/dashboard.rs`, `src/tui/view.rs`, `src/tui/ui.rs`, `src/cli/status.rs`, `tests/mock_upstream.rs`, docs | GREEN 2026-10-26: `just check` = fmt clean, clippy -D warnings clean, `cargo test` 1620 lib / 31 cli / 127 e2e (1 ignored) / 5 / 31 / 7 / 5 / 36, 0 failed. First run: 21 e2e failures — the plan pass's start-up profile read landed in the mock's `seen()`; fixed by serving `/api/oauth/profile` on its own mock route (like `/api/oauth/usage`). One `cli` drain-deadline failure under full-suite load did not reproduce (3/3 alone, 2/2 binary, then full gate green) | unit: `auth::plan` 4, `proxy::plans` 3, `parse_profile_reads_the_organization_rate_limit_tier`, `status_json_reports_each_accounts_plan`, `account_lines_show_the_plan_beside_the_weekly_quota_left`, `accounts_detail_pane_names_the_plan`; live: pending release |

## Review r1 — 2026-10-26 (code-reviewer)

- P1 (fixed): profile reads went out with expired tokens and a missing plan waited 6 h. Now an
  expired/unknown-expiry token reads `NotNow` (`plan_source(credential, now_ms)`), and the task
  re-runs every `PLAN_RETRY` (10 min) while any plan-capable account has no plan. Tests:
  `an_expired_claude_token_is_not_used_for_a_profile_read`, `missing_counts_plan_capable_accounts_without_a_plan`.
- P2 (fixed): the 0.2.24 note names its older stable; the release-availability row lands with the
  v0.2.26 tag this PR is released as (release runbook step 1: patch bump by default).

## Gap matrix

| Acceptance | Status | Observation |
| --- | --- | --- |
| status JSON plan fields | GREEN (live 2026-10-26) | both deployed daemons: `plan`/`rate_limit_tier`/`plan_type` per account; unit `status_json_reports_each_accounts_plan` |
| `llmux status` plan + 7d left | GREEN (live 2026-10-26) | `1. claude:ai8@…  active  max20 · 7d 75% left`; `15. codex:ai5@…  ready  promax · 7d 100% left`; `18. codex:ai@…  ready  pro · 7d 68% left` |
| TUI detail plan | GREEN (unit) | `accounts_detail_pane_names_the_plan` (` · codex · promax`) |
| release + dev hosts | GREEN 2026-10-26 | tag v0.2.26 → Release workflow success (6 assets); Preview success (preview-2026-10-26-1105-b8e490ffdfd3, tap bumped); both dev daemons `brew upgrade llmux-preview` + restart → `0.2.26 (preview …)`, client/server match. Stable formula follows the tap's 6-hourly scheduled bump |
| soma-work without the map | GREEN (live 2026-10-26) | soma-work #445 (Codex `plan_type`) deployed; a dev host with `SOMA_LLMUX_ACCOUNT_TIERS` removed: Claude 14/15 counted on llmux `max20`, 1 (`default_raven`) shown as `티어 미지정 1(x20 가정)`; Codex 4/4 counted (`pro`/`promax`), total 2.4 → 2.8 |
