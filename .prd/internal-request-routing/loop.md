# internal-request-routing — convergence loop

SSOT: ./ssot.md (frozen) · Spec: ../27-claude-code-harness-requests.md · Done: ./loop.done.md ·
Repo: 2lab-ai/llmux · Channel: Slack thread 1794465952.178559 · Started: 2026-11-12 (15:46 KST)

Status: R1–R4 closed — see loop.done.md. No open rows.

## Build facts (measured)

- Rust gate: `just check` (fmt + clippy -D warnings + tests) → exit 0, 2084 passed (2026-11-12 16:03 KST).
- Web dashboard gate: `node --test webui/test/`.
- Evidence source: raw-io day partition `~/.local/state/llmux/store/raw-io/2026-11-12.sqlite3`
  on the [owner Mac], read with `sqlite3 "file:…?mode=ro"`; ids #50903–#50953.
- Isolated live probe: the branch's debug binary on port 3999 with `LLMUX_CONFIG`/`XDG_*` pointed
  at a scratch dir, one Codex and one Grok account copied with refresh disabled (dummy refresh
  token and token endpoint), no Claude account; the production daemon on 3456 is not touched.

## Next

- None open. Follow-up outside this loop: failed idle probes are silent — https://github.com/2lab-ai/llmux/issues/292.
