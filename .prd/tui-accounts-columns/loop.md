# Accounts table columns — loop

Status: in progress
Date: 2026-09-18

## Build facts (measured 2026-09-18)

- Gate: `just check` = `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test`.
- Table renderer: `src/tui/ui.rs` `draw_accounts_table` (`wide` decision :4268, narrow Fbl give-way :4286, `bar_width` :4311, header/constraints :4388/:4414), `account_row` :4542-4595, `window_gauge_cell` :4852, `fable_gauge_cell` :4913; display states `src/scheduler/window.rs:172-191`.
- Live daemon at start: `0.2.22 (preview 2026-09-17-0944-464f5bb7ab85)`; `/llmux/status` shows `five_hour: null` on all 4 codex accounts, grok `five_hour.utilization 0.0` (header-fed), claude 13/15 populated.
- Release chain: `release.yml` on tag `v*` (tag must equal Cargo.toml version; assets + `LlmuxIslands-<v>.zip` + SHA256SUMS) → tap `bump.yml` stable jobs are **cron-only (6h)** → rendered locally instead (`render-tap-stable.sh`). Preview: `preview.yml` on push to main or `workflow_dispatch --ref <tag>`; tap preview bump is event-driven in the same job. main is not branch-protected; last release commit (`848e5c4`) was a bump-only commit touching Cargo.toml + 4 lockfiles.

## Round 1 — 2026-09-18

| WU | Branch / worktree | Scope (files) | Owner | Gate | Verify |
| --- | --- | --- | --- | --- | --- |
| 1 | `feat/tui-accounts-columns` / `.worktrees/feat-tui-accounts-columns` | `src/tui/ui.rs` (+ `.prd/18`, `.prd/tui-accounts-columns/*`) | opus-coder | GREEN 2026-09-18 (dispatcher re-run ×2): `just check` exit 0 — fmt/clippy silent, lib 1270 passed / 0 failed (+5), cli 22, e2e 80 (1 ignored), grok_usage 5, keys_history 31, relogin 7, token_limits 5, usage_controls 32 | 6 new unit tests; live read-only attach (`llmux dashboard`, worktree release binary) to the real 20-account daemon at 200/100/80/74 cols — captures in the session scratchpad `cols-receipt/dash-*-r2.txt` |

Review (trinity, 3 engines): R1 grok APPROVE / astra REJECT (after 5h drops, 80 cols with a 20-char name still summed to 87 → ratatui shaved `group` to `grou`; reproduced live) / fable APPROVE → fix: `account` column is give-way step 3 (`NAME_COL_MIN = 7`), test `narrow_80_shrinks_name_to_protect_status_floor_and_fable_gauge` → R2 unanimous APPROVE, MUST-FIX none.

## Gap matrix

| Acceptance | Status | Observation |
| --- | --- | --- |
| codex/grok 5h = `-` | GREEN (live 2026-09-18) | 200-col capture: all 4 CODEX rows and the GROK row render `-` in the 5h slot; CLAUDE rows `○ cold` / `14%` / `○ 100%` |
| 5h column fixed 8 wide | GREEN (live 2026-09-18) | header `5h`→`7d` offset 9 at 200 and 100 cols; `five_hour_cell_fits_eight_cells` enumerates every state (widest `◑ 100%!` = 7) |
| Fbl never gives way; 5h drops last | GREEN (live 2026-09-18) | 80 cols: `7d Fbl` full gauge with percent, `5h` header absent; 100 cols: both present |
| status shrinks first, floor 8 | GREEN (live 2026-09-18) | 100 cols: status 16 (`▒ 7d 100.0% > 99`); 80 cols: status 8 (`▒ 7d 100`), name 13; 74 cols: name 7, no shaving |
| stable release v0.2.23 deployed (tap + local brew + daemon) | pending | |
| preview rebuilt from the v0.2.23 commit and deployed | pending | |

## Ship table

| Step | Evidence |
| --- | --- |
| PR merged | |
| `chore: release v0.2.23` on main | |
| tag `v0.2.23` → Release run | |
| tap stable formula + cask | |
| `brew upgrade llmux` → `--version` | |
| preview dispatch on `v0.2.23` → prerelease + tap bump | |
| `brew upgrade llmux-preview` → daemon restart → `llmux status` | |
