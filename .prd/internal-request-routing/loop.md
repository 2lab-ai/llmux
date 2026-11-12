# internal-request-routing — convergence loop

SSOT: ./ssot.md (frozen) · Spec: ../27-claude-code-harness-requests.md · Done: ./loop.done.md ·
Repo: 2lab-ai/llmux · Channel: Slack thread 1794465952.178559 · Started: 2026-11-12 (15:46 KST)

Status: R1 closed (5/5 ✅), R2 review closed (2/2 ✅) — see loop.done.md. R3 open — the shipped preview on the [owner Mac].

## Build facts (measured)

- Rust gate: `just check` (fmt + clippy -D warnings + tests) → exit 0, 2084 passed (2026-11-12 16:03 KST).
- Web dashboard gate: `node --test webui/test/`.
- Evidence source: raw-io day partition `~/.local/state/llmux/store/raw-io/2026-11-12.sqlite3`
  on the [owner Mac], read with `sqlite3 "file:…?mode=ro"`; ids #50903–#50953.
- Isolated live probe: the branch's debug binary on port 3999 with `LLMUX_CONFIG`/`XDG_*` pointed
  at a scratch dir, one Codex and one Grok account copied with refresh disabled (dummy refresh
  token and token endpoint), no Claude account; the production daemon on 3456 is not touched.

## R3 — the shipped preview

| ID | Gap | Status | Source |
| --- | --- | --- | --- |
| G6 | Preview build on the [owner Mac] routes a real GPT and a real Grok session's title, status and security requests (spec A8); live `claude_code.gpt_model_mapping.opus` set to `terra` | 🟡 partly measured: preview `2026-11-12-0803-3c2b27a9a5f5` running, `opus: terra` set; real `astra` and `grok` sessions (tmux, auto mode): quota and title → `gpt-6-luna` / `grok-4.7` low, security → `gpt-6-luna` / `grok-4.7` medium | spec A8 |
| G9 | After `/clear` the title of the new session still reached `claude-haiku-5-5` in both real sessions (no warmup, launch hint past 120 s; raw-io #121, #126) | 🟡 fix on branch `fix/title-waits-for-first-main-turn`: bounded wait for the first main turn; E2E `harness_title_after_clear_waits_for_the_first_main_turn` (RED on main: 404) | live, spec A9 |

## Next

- Merge G9 fix → preview release → `brew upgrade llmux-preview` → restart → `/clear` in a real `astra`
  and `grok` session → raw-io shows the new title on the main provider → close G6 and G9, set spec 27 to shipped.
