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
| picker lists catalog rows | GREEN (live 2026-09-17) | `model_picker_settings` emits one row per catalog entry in catalog order (`model_picker_settings_round_trips_the_real_catalog`); live: worktree binary `llmux 0.2.22 (dev dev)` → `llmux run --remote localhost:3456` (remote mode so the running daemon is never version-restarted) → `/model` in Claude Code v2.1.274 showed the 6 built-in rows followed by the catalog rows 7–27 ("Claude Fable 5 · claude · efforts low…max · ctx 1000000", "GPT-6-Astra [1M] · codex · efforts low…ultra · ctx 1000000", "Grok 4.6 · grok · efforts low…xhigh · ctx 500000", "Ox Alpha (free) · openrouter …", "… +21 models"). `claude-fable-5-1[1m]` did not appear as a catalog row — Claude Code de-duplicated it against the built-in "Fable" row, as the docs say it does for ids the lineup already covers |
| user `--settings` wins + warning | code landed, live open | `injects_model_picker_matrix` covers `--settings x` and `--settings=x` (both → no injection); the warning line itself is not asserted (it is an `eprintln!` in `picker_args`) |
| `--no-model-picker` spawns without `--settings` | decision tested, spawn open | `injects_model_picker_matrix` (flag → false); the spawned argv is not observed in-process |
| catalog fetch failure → warning, launch continues | code landed, live open | `fetch_catalog_reports_an_unreachable_daemon` / `fetch_catalog_rejects_non_200_and_junk_bodies` return sanitized `Err` (no key, no body) and `picker_args` degrades to an empty argv addition |
| selected grok row routes to grok | GREEN (live 2026-09-17) | picker row 20 selected with `s` ("Set model to Grok 4.6 for this session only"), one turn "Reply with exactly one word: pong" → "pong" after 24 s; daemon `activity.jsonl` row: `path /v1/messages?beta=true · account grok:icedac@gmail.com · group grok · model grok-4.6 · effort xhigh · status 200 · duration_ms 24155 · input 187958 / output 226`. Observation: Claude Code's "high" effort arrived at the daemon as `xhigh` for grok — llmux's effort mapping, not the picker |

Residual (not blocking): a `modelPicker` key in the user's own `~/.claude/settings.json` is overridden by the injected `--settings` (detection looks at argv only) — documented in `docs/models.md`.
