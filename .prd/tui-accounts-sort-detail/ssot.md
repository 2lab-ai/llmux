# Accounts table: Fbl n/a, sort modes, detail modal — SSOT

Status: shipped (preview-2026-09-18-0609-850db8b80519)
Date: 2026-09-18

## User instruction (verbatim, 2026-09-18)

> llmux 개선
>
> 1. 7d fable도 코덱스 그록등 에비르 사용량 없는 열에 cold가 아니라 "-"로 비워줘
> 2. accounts를 클로드, 코덱스, 그록, or 순으로 정렬해줘 그리고 같은 그룹에서도 어카운트 이름으로 정렬해줘 (이거 정렬을 2가지로 해줘. 1 이름 정렬, 다음 사용 차례 순 정렬)
> 3. accounts에서 특정 accounts를 클릭하면 해당 계정의 모든 상세한 디테일 정보 모달로 출력해줘 출력할수 있는 모든 정보)
>
> 프리릴리즈까지 배포 ㄱ

Standing rules carried over from `.prd/tui-accounts-columns/ssot.md`: work through
`.prd/` (using-dotprd); preview deploy = full chain (main push → preview.yml →
tap bump → `brew upgrade llmux-preview` → daemon restart → `--version`/`status`).

## Acceptance contract

| User clause | Required observable outcome | Evidence |
| --- | --- | --- |
| 7d fable도 코덱스 그록등 … cold가 아니라 "-" | On a CODEX / GROK / OPENROUTER row the `7d Fbl` cell is a dim `-`; no `cold`, no `○`. A CLAUDE row with no Fable scope still reads `○ cold`; a CLAUDE row with a Fable window keeps the gauge unchanged. | unit: `fable_gauge_cell_is_n_a_for_non_claude_groups`; live: TUI capture of the deployed daemon |
| 클로드, 코덱스, 그록, or 순으로 정렬 | Rows are grouped in `BackendGroup` order Claude → Codex → Grok → OpenRouter in BOTH sort modes. | unit: `display_order_groups_claude_codex_grok_openrouter`; live capture |
| 같은 그룹에서도 어카운트 이름으로 정렬 (1 이름 정렬) | Sort mode **name**: within a group, rows ordered by account name — case-insensitive NATURAL sort: digit runs compare as numbers, so `ai` < `ai1` < `ai2` < `ai10` (the empty number slot sorts first), with the lowercased spelling (`ai01` vs `ai1`) and then the config index as the stable tiebreaks. Default mode. Natural order added 2026-09-18 on the owner's correction ("ai, ai1, ai2 ... ai10 이거 정렬하면 실제 무낵이랑 정렬이 병신이잖아"). | unit: `display_order_by_name_sorts_within_group`, `display_order_by_name_is_natural_for_numbered_accounts`, `natural_key_orders_like_a_human` |
| 다음 사용 차례 순 정렬 | Sort mode **next**: within a group, the literal order the scheduler would pick — that group's current account first, then eligible accounts in `select::rank(group)` order (round-robin mode: roster order after current), then ineligible accounts in config order. Reuses the selector's own `eligibility` + `rank`, so the table can never disagree with the daemon. | unit: `display_order_by_next_follows_group_selection_order`; parity test vs `select::pick` head |
| 정렬을 2가지로 해줘 | A key (`o`) toggles name ↔ next on MAIN and the accounts overlay; the accounts pane title shows the active mode (`accounts · sort name` / `sort next`); footer advertises `o sort`. Session-local (like `t`/`u`). Every row cursor (switch/remove/limits/context menu/reset confirm) follows the same order as the render. | unit: `sort_key_toggles_mode_and_title`; existing pinned-identity tests still pass |
| 특정 accounts를 클릭하면 … 모달 | Left-click on an accounts-table row (MAIN and accounts overlay, `Mode::Normal`, no other modal open) opens a centered modal pinned to that account's REAL id (not the display index). Esc/q/Enter close; ↑↓/PgUp/PgDn/Home/End + wheel scroll; every other key/click is swallowed. If the account vanishes from the snapshot the modal closes. | unit: `account_click_opens_detail_modal_pinned_by_id`, `account_modal_closes_when_account_gone`, `account_modal_swallows_keys_and_clicks` |
| 출력할수 있는 모든 정보 | Modal body lists every per-account datum the view holds: identity (name, credential kind, group, healthy, paused), position in the current sort + mode, status/gate reason, current-per-group / fable-current / manual pin, in-flight, token expiry/refresh, 5h + 7d windows (utilization raw+effective, reset countdown+absolute, source, fetched age, display state), every scoped limit (label, utilization, reset, severity, is_active, constraining vs effective ceiling, source, age), account-wide cooldown (until, source) + every scoped cooldown (scope, until, set_at, reason), per-account limit overrides + effective ceilings, lifetime totals (req/ok/err/tokens in/out), poll health (last ok, next, consecutive failures), usage-control doc (all fields). | unit: `account_modal_renders_every_section` (asserts each section label present); live capture |
| 프리릴리즈까지 배포 | PR merged to main → preview.yml green → `preview-<stamp>-<sha>` prerelease → tap `llmux-preview` bumped → `brew upgrade llmux-preview` on fable-m5max → `llmux restart` one-shot → `llmux --version` reports the new preview id and `llmux status` shows the daemon on it. | loop.md ship table |
