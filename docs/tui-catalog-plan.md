# TUI catalog plan — every twolab-tui entry against the dashboard

**In one line:** every catalog entry that fits the dashboard is done. It landed in five pull requests (#231, #233, #234,
#235, #236); the four entries that do not fit say why. Tracker: https://github.com/2lab-ai/llmux/issues/232.

**How to read this:** the map below says where each catalog entry landed. The table after it gives, for every entry,
what llmux drew before, what the person at the terminal sees now, and the pull request. Evidence (file and line,
captured frames) is in the two research reports named at the end.

## Map

```
catalog (27 + herdr) ─┬─ #231 move onto twolab-tui ... text · spinner · gradient_text · osc52 · key_hints (main bar)
                      │                                · tabs (main row) · json_view highlighter · theme decolor
                      ├─ #233 prompts ................ choice_form · select_list · key_hints · multi_select (+ scroll, input)
                      ├─ #234 viewers, data .......... pager · modal · format · ledger · status_card · mini_bar
                      ├─ #235 round 2 ................ scrollbar · tabs (raw viewer) · json_view (raw body) · toast · banner Flash
                      │                                · menu · status_bar · trend_chart · working · composer · meter
                      │                                · packed_grid · rule · shimmer · agent_list · pager selection · fact_table
                      ├─ #236 last pass .............. theme sweep · hit module
                      └─ Not applicable .............. completion_menu · diff_view · approval_card · layout (reasons in the table)
```

## Every entry

| Catalog entry | From | llmux before | What the person sees now | Status |
|---|---|---|---|---|
| choice_form | fx | reset, remove and config confirm prompts are one footer line; the reset subject lives in a 5 s status line | the prompt names the account (or setting old → new) for as long as it is open, with numbered choices and the keys they need | done (#233) |
| select_list | Codex | new-login picker runs off the right edge at 80 columns; config labels run into values | one row per provider with digits; config labels on a measured column, read-only rows marked, `N of M` when it scrolls | done (#233) |
| key_hints | Codex | five prompts and the raw-viewer legend are hand-built and cut mid-word; `?` help is a fixed list that disagrees with the footer | every prompt drops whole hints and says `… +N more`; help is built from the footer's own hint tables | done (#233) |
| multi_select | Codex | keys model picker has no filter and its options freeze when it opens | type to filter, `k of n selected`, options refresh when the query lands, popup sized to content | done (#233) |
| scroll, input (modules) | Codex | each list clamps its own cursor; handlers match crossterm keys directly | PgUp/PgDn/Home/End behave the same in every list | done (#233) |
| pager | Codex | the logs overlay cannot scroll and shows a blank pane when empty; modals scroll with no position | scroll back through the log ring, long lines wrap, `NN%` position, an honest "no log lines yet" | done (#234) |
| modal | herdr | popups are 80%×80% (input), 80%×70% (keys picker), 80%×85% (account) whatever their content | popups fit their content up to a cap, with a footer of keys and a scrollbar when the body overflows | done (#234) |
| scrollbar | herdr | tables and modals scroll with no scrollbar; the raw viewer's bars cannot be dragged | a thumb that shows where you are and can be dragged | done (#235) |
| tabs (raw viewer) | llmux | Request/Response tabs measured in bytes | same look and click behaviour as the main tab row | done (#235) |
| json_view (raw body) | llmux | bodies chunked at 2048 bytes and panned with H/L | wrapped at the width, `lines 12–40 of 213`, still fast on 8 MiB bodies (row heights cached per width) | done (#235) |
| toast | herdr | status messages are one row, cut at 80 columns, replaced by the next one and gone after 5 s | messages stack in a corner card with ok/warn/fail glyphs; a multi-account refresh result can be read line by line | done (#235) |
| banner (Flash) | llmux | every status message is yellow, failures included | ok, warn and fail look different, also under `NO_COLOR` | done (#235) |
| format | llmux | the models table shows `243578.5` for 243.6 billion tokens (unit clipped) | `243.6G`; `999_999` reads `1.0M` | done (#234) |
| ledger | llmux | cost columns cut digits (`$112293.0`) | amounts line up on the decimal point and keep their cents | done (#234) |
| menu | llmux | the right-click menu covers the clicked row near the bottom; items show no keys | opens above the row when there is no room below, shows each item's key, marks delete as destructive | done (#235) |
| status_bar | fx | the header is cut after `llmux 0.2.26` at 80 columns; the settings bar is cut mid-segment | whole segments drop by importance; settings-bar clicks land on the segment that is drawn | done (#235) |
| status_card | Codex | the account modal prints quota windows as `util 0.123 · eff …` | a bar per window with its reset time | done (#234) |
| trend_chart | llmux | series are told apart only by colour; the tokens chart zero-fills days before history | `* + x o` per series without colour, gaps for missing days; tokens and perf charts on one renderer | done (#235) |
| working | Codex | the attach screen is static yellow text | a spinner and how long it has been retrying | done (#235) |
| mini_bar | llmux | an empty share bar when the total is zero | `—` when the share cannot be computed | done (#234) |
| composer | fx + herdr | add-key and config edits only append or delete at the end | add key, limits and config value are one-line composer fields: caret moves, mid-value edits, Ctrl+U/K/W cut and Ctrl+Y puts back, a paste lands whole; the API key is a masked composer, `∙` in every frame | done (#235) |
| text, spinner, gradient_text, osc52 | llmux, Codex | done in PR #231 | — | done (#231) |
| key_hints (main bar), tabs (main row), json_view (highlighter), theme (decolor) | | done in PR #231 | — | done (#231) |
| meter | llmux | the original of the crate's meter | the same bars (the 179×43 frame digest is unchanged); the cursor row's bar keeps its fill visible; under `t` a stamp beside a state glyph shows its date instead of losing a digit | done (#235) |
| packed_grid | llmux | the original, twice (fold and models strip) | one packer for both; a click on a strip entry opens that model in `stats`; the strip's columns stay put when a model starts or stops running; a fold cut by a short pane ends in `+N more` | done (#235) |
| rule | fx | 28 section rules drawn as block borders | every section heading is a library rule (`── accounts ──`); with `LLMUX_AMBIGUOUS_WIDE=1` the rules draw ASCII `-` and end at the right edge on a terminal that draws `─` two cells wide | done (#235) |
| shimmer | Codex | loading labels are plain text | a band of light passes across `loading…` while it waits (sessions, keys title, raw viewer); plain bold under reduced motion | done (#235) |
| theme (sweep) | 2lab | 149 literal colours left in drawing code (118 by the time of the sweep), two tab strips on a second theme, four tables building their own cursor style | nothing moves: every colour is a field of the one dashboard theme or a named palette hue (provider, chart series, message kind), every selection is the theme's reverse video, and a test fails on a new literal (the 179×43 digest is unchanged). Two hues fixed: openrouter's labels are green like its spinner (they were dim, like an unknown group), and a `user` tag in the activity log is the default foreground (it was white, near-invisible on a light terminal) | done (#236) |
| hit (module) | 2lab | ten hand-written hit structs and eight inline `find` closures, most assuming one-row targets | nothing changes at the terminal: every surface pushes its click targets into one twolab-tui `HitMap` in paint order while it draws, so the one on top wins (a modal over a row, a menu item over a row, the `🔍 input` line over its entry); the map is rebuilt every frame and the mouse handler asks it once; a new widget brings its rects by pushing them | done (#236) |
| agent_list | herdr | 29 accounts show 1–4 rows on MAIN at 80 columns | a compact account list: one row per account with a one-cell glyph per state (`✗ ◌ ◉ ⠋ ◦ ǁ ?`), the cursor's account highlighted, a one-line tally and `+N more` in the title; `V` on the accounts tab, and MAIN uses it whenever its table would show fewer than 5 rows (21 accounts at 80×24: 10 shown instead of 2) | done (#235) |
| pager selection (copy mode) | herdr | mouse capture blocks the terminal's own selection | `v` releases the mouse for the terminal's selection under a banner (screen held still, `v`/Esc return); a drag in the Logs pager selects and copies through the clipboard/OSC52 path with a status confirmation | done (#235) |
| fact_table | fx | model drill-down facts as free text | figures aligned on the last digit, two columns on a wide pane, `—` for unknown; the misc tab's build/daemon facts too | done (#235) |
| completion_menu | fx | no typed command line | — | not applicable — llmux has no typed command line to complete |
| diff_view | fx | config edits are single values; prompts already show old → new | — | not applicable — every config edit is one value, and the confirm prompt already shows old → new |
| approval_card | fx | three fixed choices; llmux gates need two | choice_form covers it | not applicable — its three fixed choices do not fit llmux gates, which need two; choice_form covers them |
| layout | Codex | ratatui Layout and llmux's own planners | comes in with pager and select_list | not applicable — ratatui `Layout` and llmux's own planners already do this; pager and select_list brought what was missing |

## Decisions made on the way

- Old keys stayed as aliases when the prompts moved (`y`, `n`, `Esc`).
- Copy mode took `v`, so the compact account list toggles with `V`.
- Copy mode works over a modal too, because the modal holds the text worth selecting.
- A paste outside a text field types nothing; only the keys picker's open filter takes it.
- The compact list replaces the account table in the same pane, never beside it.
- Every move of the pinned 179×43 frame digest is explained next to it. So is every change that left it unchanged
  (meter, theme sweep, hit map).

## Library gaps found (2lab-ai/tui, not blocking)

- `Theme` has no series or provider hues; llmux keeps them as a named palette.
- ANSI `surface_raised` is `Reset`, so the OpenAI-endpoint row background (and the light text on it) stay palette constants.
- `Theme::selected()` is not `const`.
- `HitMap` has no `FromIterator`.
- menu, modal, tabs and packed_grid return their own hit lists instead of pushing into a `HitMap`.
- `ToastStack::render` returns each toast's rect but no id, so llmux maps the stack's index back to the queue's
  `entries()` to record a toast as a click target (a click takes it down; it no longer reaches the row under it).

## Evidence

- Audit of llmux against the 27 widgets, with 80×24 frames: [twolab-tui docs/research/llmux-audit.md](https://github.com/2lab-ai/tui/blob/main/docs/research/llmux-audit.md).
- herdr inventory and health check: [twolab-tui docs/research/herdr-inventory.md](https://github.com/2lab-ai/tui/blob/main/docs/research/herdr-inventory.md).
- The catalog with live demos: https://2lab.ai/tui
- Tracker: https://github.com/2lab-ai/llmux/issues/232
