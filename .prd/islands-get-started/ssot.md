# Islands get started — fixed target
Status: shipped
Date: 2026-10-08

## User request (verbatim)

> 너무 좋은데 이거 어떻게 마케팅하면 좋을까? 일단 일반 유저가 사용하기 쉬워 보이지 않는게 문제 같긴함 먼저 llmux islands를 개선해주고 이걸로 쇼츠 영상 만들어서 쓰레드에 드래프트로 올려줘

## Scope

Existing Claude Code/Codex users who find terminal setup intimidating. Improve the
actual macOS first-run → connect account → launch chosen client in a project flow.
Preserve native account tiles, shared semantic/privacy state and Linux behavior.
No server-auth weakening; no provider credentials in the GUI. A narrow CLI-owned
local control-key handoff is an explicit amendment to the original no-config-read
GUI boundary: the app still never reads the daemon config or provider credentials.
Keys remain executor-only memory. Remote keys never flow to loopback or another endpoint.

The initial media plan was one 30–45 second Korean demo. Subsequent user steering
expanded delivery to five distinct vertical videos and five separate Threads
drafts. All use actual rendered app states with disclosed synthetic fixture data;
two include Korean narration, and lengths vary with each concept. Drafts are saved
without publishing or scheduling posts. Do not quit the user's running app or
change real accounts for a capture. The frozen v0.2.24 release is separate; do not
merge this feature before its tag exists.
