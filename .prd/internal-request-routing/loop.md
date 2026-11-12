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

## Re-verified

- G6, G9 re-verified 2026-11-12 20:05 KST on preview `2026-11-12-0929-740bc0c3039b`: `/clear` in the real astra session `2f8d5bb5` and grok session `9b63cf4d`, then a file write; raw-io: title #2412 `claude-haiku-5-5` → `gpt-6-luna` low, title #2413 → `grok-4.7` low, the other 8 requests were the sessions' own main turns (`gpt-6-astra` ×6, `grok-4.7` ×2), 0 requests to a Claude model.
- soma-work side: 2lab-ai/soma-work#1224 deployed to dev (deploy run 43206305851, all three targets); the deployed `auxModelFor` resolves Claude → `claude-haiku-5-5[1m]`/`claude-sonnet-5-5[1m]`, Codex (astra or sol) → `gpt-6-luna[1m]`/`gpt-6.1-terra[1m]`, Grok → `grok-4.7` low/medium.

## Next

- None open. Follow-up outside this loop: failed idle probes are silent — https://github.com/2lab-ai/llmux/issues/292.
