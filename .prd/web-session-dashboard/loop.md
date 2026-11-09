# web-session-dashboard — convergence loop

SSOT: ./ssot.md (frozen) · Spec: ../26-web-session-dashboard.md · Done: ./loop.done.md · Repo: 2lab-ai/llmux ·
Channel: Slack thread 1793996619.575599 · Started: 2026-11-09 (12:45 KST)

Status: R1 closed (8/8 ✅), R2 closed (3/3 ✅). Runtime is live on both hosts from branch `feat/web-session-dashboard`.
Related fix outside this repo: soma-work side calls (workflow classifier, goal eval, guardian, titles/summaries) sent
without the user's llmux key — https://github.com/2lab-ai/soma-work/pull/1065 (ssot 7; open, deploy pending).

## Build facts (measured)

- Syntax gate: `node --check webui/server.mjs webui/lib/*.mjs webui/public/*.js` → exit 0.
- Unit gate: `node --test webui/test/` → all pass.
- Rust gate: `just check` → exit 0 (webui adds no Rust).
- Observation gate: QA token minted from the host's soma-work secret (admin, ≤ 2 h), Playwright 1600×1000 against the
  public URL (iq-64 via `--host-resolver-rules` to 192.168.50.49); curl SSO matrix on each host.
- Deploy: `rsync` `webui/` → `~/llmux-webui/` (keeps `server.log`, `run.sh`, plists), then
  `launchctl kickstart -k gui/$UID/ai.2lab.llmux-webui` (fable-m5max) or `user/$UID/…` (iq-64, no GUI login).

## Re-verification (2026-11-09 16:30 KST)

- re-verified ✅: H3 · Playwright against both public URLs, viewer output compared with the raw-io record it was given
  (`raw-verify.mjs`): iq-64 request #13467 (808 KB, 184 messages) and fable-m5max #12270 (1.79 MB, 566 messages) open with
  only `/messages/<last>` expanded; `/system` and the previous message expand on demand; an expanded string equals the
  record value; headers 21/21 with credentials redacted; raw view equals `request_body` (757,912 chars; 1,794,916 after
  "나머지 더 보기"); pretty view equals `JSON.stringify(body, null, 2)`; the Response leg shows the message assembled from
  342/348 SSE events and its raw view equals `response_body`; 0 page errors · branch e9cf725

## Next

- PR review and merge to `main` (owner decision; runtime does not depend on it).
- Candidates for a later round (not in SSOT scope): account/key write actions from the web page; raw-io meta for
  remote sessions older than 7 days is fetched only when the card is opened.

## Gap matrix

| ID | Source | Universe Item | Observable Acceptance | AS-IS Evidence | Status | Gap | Dispatch | Verification Evidence | Last Measured |
|---|---|---|---|---|---|---|---|---|---|

- Status: ✅ met · 🟡 partial · 🔴 to work · 🚪 gated · ⚫ out of scope
- Accounting: universe 11 = 11✅ (loop.done.md R1 8 + R2 3) + 0 open
