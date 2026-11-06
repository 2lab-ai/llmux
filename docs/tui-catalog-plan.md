# TUI catalog plan — every twolab-tui entry against the dashboard

**In one line:** the dashboard's data cells already match the catalog; the gaps people hit are prompts that forget what
they are asking about, viewers that cannot scroll, popups sized by percentage, and messages that vanish after 5 seconds.
This plan fixes those first, in three pull requests, and leaves the pure de-duplication for later.

**How to read this:** the map below says which batch each catalog entry lands in. The table after it gives, for every
entry, what llmux draws today, what changes for the person at the terminal, and why an entry is left out. Evidence (file
and line, captured frames) is in the two research reports named at the end.

## Map

```
catalog (27 + herdr) ─┬─ Batch 1  prompts ............ choice_form · select_list · key_hints · multi_select (+ scroll, input)
                      ├─ Batch 2  viewers, overlays .. pager · modal · scrollbar · tabs (raw viewer) · json_view (raw body)
                      ├─ Batch 3  feedback, data ..... toast · banner Flash · format · ledger · menu · status_bar · status_card
                      │                                · trend_chart · working · mini_bar · composer (masked field)
                      ├─ Already done (PR #231) ...... text · spinner · gradient_text · key_hints (main bar) · tabs (main row)
                      │                                · json_view highlighter · theme decolor · osc52
                      ├─ Later, with a reason ........ meter · packed_grid · rule · shimmer · theme sweep · hit · agent_list
                      │                                · pager selection (copy mode)
                      └─ Not applicable .............. completion_menu · diff_view · approval_card · layout · fact_table*
```
\* fact_table is "later": it fits the model drill-down but has no visible problem to fix today.

## Every entry

| Catalog entry | From | llmux today | What the person sees after | Batch |
|---|---|---|---|---|
| choice_form | fx | reset, remove and config confirm prompts are one footer line; the reset subject lives in a 5 s status line | the prompt names the account (or setting old → new) for as long as it is open, with numbered choices and the keys they need | 1 |
| select_list | Codex | new-login picker runs off the right edge at 80 columns; config labels run into values | one row per provider with digits; config labels on a measured column, read-only rows marked, `N of M` when it scrolls | 1 |
| key_hints | Codex | five prompts and the raw-viewer legend are hand-built and cut mid-word; `?` help is a fixed list that disagrees with the footer | every prompt drops whole hints and says `… +N more`; help is built from the footer's own hint tables | 1 |
| multi_select | Codex | keys model picker has no filter and its options freeze when it opens | type to filter, `k of n selected`, options refresh when the query lands, popup sized to content | 1 |
| scroll, input (modules) | Codex | each list clamps its own cursor; handlers match crossterm keys directly | PgUp/PgDn/Home/End behave the same in every list | 1 |
| pager | Codex | the logs overlay cannot scroll and shows a blank pane when empty; modals scroll with no position | scroll back through the log ring, long lines wrap, `NN%` position, an honest "no log lines yet" | 2 |
| modal | herdr | popups are 80%×80% (input), 80%×70% (keys picker), 80%×85% (account) whatever their content | popups fit their content up to a cap, with a footer of keys and a scrollbar when the body overflows | 2 |
| scrollbar | herdr | tables and modals scroll with no scrollbar; the raw viewer's bars cannot be dragged | a thumb that shows where you are and can be dragged | done |
| tabs (raw viewer) | llmux | Request/Response tabs measured in bytes | same look and click behaviour as the main tab row | done |
| json_view (raw body) | llmux | bodies chunked at 2048 bytes and panned with H/L | wrapped at the width, `lines 12–40 of 213`, still fast on 8 MiB bodies (row heights cached per width) | done |
| toast | herdr | status messages are one row, cut at 80 columns, replaced by the next one and gone after 5 s | messages stack in a corner card with ok/warn/fail glyphs; a multi-account refresh result can be read line by line | 3 |
| banner (Flash) | llmux | every status message is yellow, failures included | ok, warn and fail look different, also under `NO_COLOR` | 3 |
| format | llmux | the models table shows `243578.5` for 243.6 billion tokens (unit clipped) | `243.6G`; `999_999` reads `1.0M` | 3 |
| ledger | llmux | cost columns cut digits (`$112293.0`) | amounts line up on the decimal point and keep their cents | 3 |
| menu | llmux | the right-click menu covers the clicked row near the bottom; items show no keys | opens above the row when there is no room below, shows each item's key, marks delete as destructive | 3 |
| status_bar | fx | the header is cut after `llmux 0.2.26` at 80 columns; the settings bar is cut mid-segment | whole segments drop by importance; settings-bar clicks land on the segment that is drawn | 3 |
| status_card | Codex | the account modal prints quota windows as `util 0.123 · eff …` | a bar per window with its reset time | 3 |
| trend_chart | llmux | series are told apart only by colour; the tokens chart zero-fills days before history | `* + x o` per series without colour, gaps for missing days | 3 |
| working | Codex | the attach screen is static yellow text | a spinner and how long it has been retrying | 3 |
| mini_bar | llmux | an empty share bar when the total is zero | `—` when the share cannot be computed | 3 |
| composer | fx + herdr | add-key and config edits only append or delete at the end | caret moves, mid-value edits, Ctrl+U/Ctrl+Y; the API key stays masked in every render | 3 |
| text, spinner, gradient_text, osc52 | llmux, Codex | done in PR #231 | — | done |
| key_hints (main bar), tabs (main row), json_view (highlighter), theme (decolor) | | done in PR #231 | — | done |
| meter | llmux | the original of the crate's meter | no visible change; moving it re-pins the 179×43 frame digest for every account row | later |
| packed_grid | llmux | the original, twice (fold and models strip) | one packer and click rects per strip entry; largest digest change | later |
| rule | fx | 28 section rules drawn as block borders | only differs on CJK-wide terminals; needs a host setting first | later |
| shimmer | Codex | loading labels are plain text | cosmetic | later |
| theme (sweep) | 2lab | 149 literal colours left in drawing code | one selected style everywhere; each batch removes the literals it touches | later |
| hit (module) | 2lab | ten hand-written hit structs | one lookup path; new widgets bring their rects | later |
| agent_list | herdr | 29 accounts show 1–4 rows on MAIN at 80 columns | a narrow account sidebar with a state glyph per account; needs a layout decision first | later |
| pager selection (copy mode) | herdr | mouse capture blocks the terminal's own selection | `v` releases the mouse for the terminal's selection under a banner (screen held still, `v`/Esc return); a drag in the Logs pager selects and copies through the clipboard/OSC52 path with a status confirmation | done |
| fact_table | fx | model drill-down facts as free text | figures aligned on the last digit; no problem reported today | later |
| completion_menu | fx | no typed command line | — | not applicable |
| diff_view | fx | config edits are single values; prompts already show old → new | — | not applicable |
| approval_card | fx | three fixed choices; llmux gates need two | choice_form covers it | not applicable |
| layout | Codex | ratatui Layout and llmux's own planners | comes in with pager and select_list | not applicable |

## Order and risk

- Batch 1 changes key handlers in `src/tui/mod.rs`; its tests drive keys, and the old keys (`y`, `n`, `Esc`) stay as aliases.
- Batch 2 needs the library's modal, scrollbar and json_view row cache (2lab-ai/tui, herdr port).
- Batch 3 needs the library's toast and format units. `format`, `status_bar` and `mini_bar` can move the pinned 179×43
  frame digest; each re-pin is explained next to the digest, as PR #231 did.
- The later items either change nothing a person sees (meter, packed_grid) or need a design decision first (agent_list,
  copy mode, rule).

## Evidence

- Audit of llmux against the 27 widgets, with 80×24 frames: [twolab-tui docs/research/llmux-audit.md](https://github.com/2lab-ai/tui/blob/main/docs/research/llmux-audit.md).
- herdr inventory and health check: [twolab-tui docs/research/herdr-inventory.md](https://github.com/2lab-ai/tui/blob/main/docs/research/herdr-inventory.md).
- The catalog with live demos: https://2lab.ai/tui
- Tracker: https://github.com/2lab-ai/llmux/issues/232
