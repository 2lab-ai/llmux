# TUI status animations — ideas + design (2026-06-14)

## 1. Ideas extracted from `agents-are-thinking`

Source: https://github.com/czl9707/agents-are-thinking (Rust crate, no runtime deps,
also WASM/PyO3 bindings). It's a library of **48 terminal "thinking" effects across 6
glyph families**, each an `Effect` with a `cycle_length` and a `step() -> frame` state
machine. Only the *ideas* were taken; teamagent does not depend on the crate.

- **Glyph families**: `braille` (U+2800–28FF dot patterns), `shade` (`░▒▓█`),
  `bar` (`▁▂▃▄▅▆▇█`), `vblock` (vertical blocks), `square`, `dot`.
- **Motion patterns** (effect names): `Spin`, `Wave`, `Pulse`, `Breathe`, `Heartbeat`,
  `Bounce`, `Ripple`, `Scanner`, `Fire`, `Arrow`, `Blink`, `Cascade`, `SeeSaw`, `Tide`,
  `Dissolve`, `Matrix`, `Checkerboard`.
- **Model**: one shared frame counter advances every tick; each effect is a pure
  `frame → glyph(s)` function. Cheap, deterministic, testable.

### Adaptation constraints for teamagent

- The reference renders multi-line *grids* (for a web preview). Our dashboard needs
  **one glyph per table row** → we use single-char frame cycles, not grids.
- **CJK width**: the owner runs a Korean locale. Geometric shapes (`◐ ● ○ ◜ …`) are
  East-Asian *Ambiguous* width and would render double-wide and misalign the table.
  We restrict the palette to **braille + block-elements only** — both *Narrow* — which
  happen to be the strongest families in the reference anyway. Guarded by a unit test
  (`all_glyphs_are_braille_or_block_elements`).

## 2. Implemented (`src/tui/anim.rs`)

Pure `glyph(frame)` functions, driven by the existing `chrome.frame` counter. Render
cadence raised 250ms → 120ms (~8fps) so motion is smooth, not choppy.

| function | glyphs | used for |
|---|---|---|
| `braille_spin` | `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` | **Claude** in-flight (magenta); current account working |
| `block_spin` | `▖▘▝▗` | **Codex** in-flight (cyan) — distinct family from Claude |
| `half_block_clock` | `▌▀▐▄` | cooldown / waiting-for-reset (yellow) |
| `bar_pulse` | `▂▃▄▅▆▅▄▃` | active (current, idle) heartbeat (green) |
| `idle_drift` | `⠁⠂⠄⠂` | ready (eligible, not current); stale data (dim) |
| `shade_breathe` | `░▒▓█▓▒` | over 5h/7d threshold (red) — "quota filling" |
| `blink` | `!` / space | auth failure alert (red, slow pulse) |

## 3. Where it shows

- **Working indicator (deliverable 2)** — activity log in-flight rows: Claude requests
  spin with the braille orbit in magenta, Codex with the quarter-block orbit in cyan
  (same colors as the group labels), so you can tell at a glance what's running where.
  Pre-routing rows (no account yet) are a dim braille spin. The provider is looked up
  from the snapshot by account name (`group_of`), so no new event/struct plumbing.
- **Animated account status (deliverable 3)** — the `status` column glyph animates per
  state: braille spin (active+working) / bar heartbeat (active idle) / idle drift
  (ready) / half-block clock (cooldown) / shade breathe (over threshold) / blink
  (auth failed) / dim drift (stale). The status column widened 18 → 20 to fit the
  leading glyph alongside the longest reason ("usage stale 14m03s").

Animation works identically in the local TUI and the `teamagent dashboard` attach client
(both render from `chrome.frame`).
