# internal-request-routing — convergence loop

SSOT: ./ssot.md (frozen) · Spec: ../27-claude-code-harness-requests.md · Done: ./loop.done.md ·
Repo: 2lab-ai/llmux · Channel: Slack thread 1794465952.178559 · Started: 2026-11-12 (15:46 KST)

Status: R1–R3 closed — see loop.done.md. R4 open — llmux's own idle probe on the small tier (#291).

## Build facts (measured)

- Rust gate: `just check` (fmt + clippy -D warnings + tests) → exit 0, 2084 passed (2026-11-12 16:03 KST).
- Web dashboard gate: `node --test webui/test/`.
- Evidence source: raw-io day partition `~/.local/state/llmux/store/raw-io/2026-11-12.sqlite3`
  on the [owner Mac], read with `sqlite3 "file:…?mode=ro"`; ids #50903–#50953.
- Isolated live probe: the branch's debug binary on port 3999 with `LLMUX_CONFIG`/`XDG_*` pointed
  at a scratch dir, one Codex and one Grok account copied with refresh disabled (dummy refresh
  token and token endpoint), no Claude account; the production daemon on 3456 is not touched.

## R4 — llmux's own automatic calls

| ID | Gap | Status | Source |
| --- | --- | --- | --- |
| G10 | The idle probe asked Claude accounts for a pinned old Haiku id (HTTP 404 on subscription tokens) and Codex accounts for the configured main model; it should run each provider's small tier by alias | 🟡 merged #291 (`haiku` owner with the Claude Code identity block and `oauth-2025-04-20` beta; Codex `luna` low); preview build pending | spec 27 "llmux's own automatic calls" |

## Next

- Preview with #291 → `brew upgrade llmux-preview` → `llmux restart` → raw-io shows idle probes as `claude-haiku-5-5` 200 with quota headers and `gpt-6-luna` low → close G10.
