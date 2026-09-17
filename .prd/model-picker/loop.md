# Model picker — loop

Status: in-progress
Date: 2026-09-17

## Build facts (measured 2026-09-17)

- Gate: `just check` = `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test`.
- Launcher: `src/cli/run.rs` — `run()` loads config, resolves endpoint, ensures the local daemon, strips a leading `--` from `RunArgs.args` (`src/cli/mod.rs:144-153`), spawns `claude` with `ANTHROPIC_BASE_URL` only (local) or plus `ANTHROPIC_API_KEY` (remote with key).
- Catalog: `crate::catalog::catalog(grok_pin, codex_pin, openrouter_pin) -> Vec<ModelEntry>` (`src/catalog.rs:285`), served at `GET /llmux/models` (`src/proxy/server.rs:2343`) as `{"models":[{id,aliases,name,efforts,max_context,group}]}`.
- Claude Code local: 2.1.274 (`modelPicker` needs ≥ 2.1.242).

## Round 1 — 2026-09-17

| WU | Branch / worktree | Scope (files) | Owner | Gate | Verify |
| --- | --- | --- | --- | --- | --- |
| 1 | `feat/run-model-picker` / `.worktrees/feat-run-model-picker` | `src/cli/run.rs`, `src/cli/mod.rs` (`--no-model-picker`), `docs/models.md`, `docs/operational-reference.md`, `README.md`, `.prd/17-*.md` status | opus-coder | GREEN 2026-09-17: `just check` = fmt --check clean, `clippy --all-targets -D warnings` clean, `cargo test` 1237 lib (+8) / 22 cli / 74 e2e (1 ignored) / 31 keys_history / 7 relogin / 5 token_limits / 32 usage_controls, 0 failed | unit tests on the pure row-builder + arg logic DONE (8 tests, `cli::run::tests`); live: `llmux run` → `/model` screen capture + activity log line for a grok turn — pending (orchestrator) |

## Gap matrix

| Acceptance | Status | Observation |
| --- | --- | --- |
| picker lists catalog rows | code landed, live open | `model_picker_settings` emits one row per catalog entry in catalog order (`model_picker_settings_round_trips_the_real_catalog`, asserted against the real `catalog::catalog`); nothing yet observed in a `/model` screen |
| user `--settings` wins + warning | code landed, live open | `injects_model_picker_matrix` covers `--settings x` and `--settings=x` (both → no injection); the warning line itself is not asserted (it is an `eprintln!` in `picker_args`) |
| `--no-model-picker` spawns without `--settings` | decision tested, spawn open | `injects_model_picker_matrix` (flag → false); the spawned argv is not observed in-process |
| catalog fetch failure → warning, launch continues | code landed, live open | `fetch_catalog_reports_an_unreachable_daemon` / `fetch_catalog_rejects_non_200_and_junk_bodies` return sanitized `Err` (no key, no body) and `picker_args` degrades to an empty argv addition |
| selected grok row routes to grok | open | — |
