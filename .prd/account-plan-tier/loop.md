# Account plan tier — loop

Status: in-progress
Date: 2026-10-26

## Build facts (measured 2026-10-26)

- Gate: `just check` = `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test`.
- Profile client: `src/auth/profile.rs` (`fetch_profile`, `parse_profile`).
- Status document: `src/proxy/server.rs` `status_json` (+ `ServerMeta`); dashboard document:
  `src/dashboard.rs` `AccountDoc` (+ `DocMeta`); CLI: `src/cli/status.rs` `account_lines`;
  TUI detail: `src/tui/ui.rs` `detail_parts_at` / `detail_name_spans`.
- Background tasks are spawned next to the usage poller in `src/proxy/server.rs` (sweep-task pattern).

## Round 1 — 2026-10-26

| WU | Scope (files) | Gate | Verify |
| --- | --- | --- | --- |
| 1 | `src/auth/plan.rs` (new), `src/auth/profile.rs`, `src/proxy/plans.rs` (new), `src/proxy/server.rs`, `src/dashboard.rs`, `src/tui/view.rs`, `src/tui/ui.rs`, `src/cli/status.rs`, `tests/mock_upstream.rs`, docs | GREEN 2026-10-26: `just check` = fmt clean, clippy -D warnings clean, `cargo test` 1620 lib / 31 cli / 127 e2e (1 ignored) / 5 / 31 / 7 / 5 / 36, 0 failed. First run: 21 e2e failures — the plan pass's start-up profile read landed in the mock's `seen()`; fixed by serving `/api/oauth/profile` on its own mock route (like `/api/oauth/usage`). One `cli` drain-deadline failure under full-suite load did not reproduce (3/3 alone, 2/2 binary, then full gate green) | unit: `auth::plan` 4, `proxy::plans` 3, `parse_profile_reads_the_organization_rate_limit_tier`, `status_json_reports_each_accounts_plan`, `account_lines_show_the_plan_beside_the_weekly_quota_left`, `accounts_detail_pane_names_the_plan`; live: pending release |

## Gap matrix

| Acceptance | Status | Observation |
| --- | --- | --- |
| status JSON plan fields | open | |
| `llmux status` plan + 7d left | open | |
| TUI detail plan | open | |
| release + dev hosts | open | |
| soma-work without the map | open | |
