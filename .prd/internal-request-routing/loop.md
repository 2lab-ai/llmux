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
| G6 | Preview build on the [owner Mac] routes a real GPT and a real Grok session's title, status and security requests (spec A8); live `claude_code.gpt_model_mapping.opus` set to `terra` | 🔴 open | spec A8 |

## Next

- Merge → preview release → `brew upgrade llmux-preview` → restart → one real session per provider →
  raw-io rows prove the routing → close G6, set spec 27 to shipped.
