# Account plan tier — loop

Status: done (v0.2.26, 2026-10-26)
Date: 2026-10-26

## Build facts (measured 2026-10-26)

- Gate: `just check` = `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test`.
- Profile client: `src/auth/profile.rs` (`fetch_profile`, `parse_profile`).
- Status document: `src/proxy/server.rs` `status_json` (+ `ServerMeta`); dashboard document:
  `src/dashboard.rs` `AccountDoc` (+ `DocMeta`); CLI: `src/cli/status.rs` `account_lines`;
  TUI detail: `src/tui/ui.rs` `detail_parts_at` / `detail_name_spans`.
- Background tasks are spawned next to the usage poller in `src/proxy/server.rs` (sweep-task pattern).

## Next

- Nothing open. A plan label for Team/Enterprise `rate_limit_tier` values (e.g. `default_raven`) needs a measured weight before it is added.
