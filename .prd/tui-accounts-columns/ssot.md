# Accounts table columns — SSOT

Status: shipped — v0.2.23 (PR #164 fcd78ce; stable v0.2.23 + preview-2026-09-18-0311-7ef84b1c5d56)
Date: 2026-09-18

## User instruction (verbatim, 2026-09-18)

> llmux 개선과 릴리즈배포
>
> 1. 코덱스랑 grok은 5시간 제한이 없음 cold가 아니라 "-" 처럼 n/a 표시해야함
> 2. 5시간 제한은 클로드만 있고 큰 의미 없으니 최소한의 칸만 할당해줘.  최대 8칸 넓이로 해줘.
> 3. 좌우 칸이 부족할때 지금 7d-fable을 줄이는데 7d-fable이 아니라 5h를 줄여줘
> 4. 대시보드 accounts 공간이 부족하면 status 창부터 줄여줘 (최소칸 8칸)
>
> 이렇게 개선하고 릴리즈배포해줘
> (릴리즈 배포 워크플로우 다음 처럼 해줘.
> 릴리즈 배포는 항상 릴리즈배포후 같은 버전으로 플리릴리즈도 릴리즈버전으로 배포해줘 항상.)
>
> 항상 /using-dotprd 스킬써서 작업 진행

## Acceptance contract

| User clause | Required observable outcome | Evidence |
| --- | --- | --- |
| 코덱스랑 grok은 … cold가 아니라 "-" 처럼 n/a 표시 | A CODEX row and a GROK row render `-` in the `5h` cell; no `cold`, no `○` | unit: `five_hour_cell_is_n_a_for_non_claude_groups`; live: TUI capture of the deployed daemon (codex ×4, grok ×1 rows) |
| 5시간 제한은 클로드만 … 최소한의 칸만 … 최대 8칸 넓이 | The `5h` column is `Length(8)` at every terminal width; a CLAUDE row shows `68%` / `○ cold` etc. inside 8 cells; the column never grows with leftover width | unit: header offset `7d` − `5h` ≤ 9 at 100/149/200 cols; `five_hour_cell_fits_eight_cells` for every `WindowDisplayState` × over |
| 좌우 칸이 부족할때 … 7d-fable이 아니라 5h를 줄여줘 | At a width where the row overflows, `7d Fbl` keeps its full gauge (header `7d Fbl` present, bar + percent rendered); `5h` is the column that drops, and only after status is at its floor | unit: `narrow_100_keeps_fable_gauge_and_drops_five_hour_last`; the old `narrow_100_…dropping_the_fable_marker` and `fable_gauge_narrow_uses_compact_marker…` are replaced |
| accounts 공간이 부족하면 status 창부터 줄여줘 (최소칸 8칸) | Shrink order = status 20→8 first; `status` never below 8 at or above the minimum width (74 cols with Fbl+rst, 70 without rst — below that the frame clips, `.prd/18` rule 5); at a width where status alone absorbs the deficit, `5h` and `Fbl` both survive; after 5h drops, `account` shrinks (20→7) before anything else | unit: `status_column_shrinks_before_any_gauge_gives_way` (width chosen so 20 overflows but 8 fits), `narrow_80_shrinks_name_to_protect_status_floor_and_fable_gauge`; live: 100 cols status 16, 80 cols status 8 + name 13, 74 cols name 7 |
| 릴리즈배포 | `v0.2.23` tag on main → Release workflow green → tap stable formula at v0.2.23 → `brew upgrade llmux` on fable-m5max → daemon restart one-shot → `llmux status` reports `0.2.23 (stable …)` | loop.md ship table |
| 같은 버전으로 프리릴리즈도 릴리즈버전으로 배포 | Preview workflow dispatched on the v0.2.23 commit → prerelease `preview-<stamp>-<sha of v0.2.23>` → tap preview bumped → `llmux-preview` formula version derived from that build | loop.md ship table; standing rule recorded in zbrain `rules/DEV.md` §6 |

## Boundaries

- Table cells only. `/llmux/status`, the detail pane, ceilings editor, poller, scheduling: untouched.
- `WindowDisplayState` labels/glyphs unchanged (detail pane and 7d still use them).
- Grok's header-fed 5h window is still recorded; the table simply does not show it (see `.prd/18` §Tension).
