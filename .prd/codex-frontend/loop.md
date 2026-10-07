# Codex frontend convergence loop
Status: shipped
Date: 2026-10-07

## Work units

| WU | Branch / worktree | Ownership |
|---|---|---|
| frontend | feat/codex-frontend / .worktrees/feat-codex-frontend | CLI, Responses adapter, shared forwarding, specification/docs |
| SDK | same worktree, disjoint files | src/provider/claude_sdk.rs, bridge/ |
| activity | same worktree, disjoint files | activity event/persistence/dashboard/TUI origin |

## Round 1

[LIVE] main 4b3d4a4, clean; isolated feature worktree created 2026-10-07.
[RULES] just check = cargo fmt --check + cargo clippy --all-targets -- -D warnings + cargo test.
[BUNDLE] existing forward.rs owns all scheduler/refresh/retry and stream lease lifetime.
[TARGET] all seven acceptance scenarios in 21-codex-frontend.md remain open.

## Gap matrix

| Gate | Evidence | State |
|---|---|---|
| protocol contract RED → GREEN | Responses HTTP and SDK transcript regressions | passed |
| native Codex model text/tool | Installed-client receipt below | passed |
| Claude SDK model text/tool | Installed-client receipt below | passed |
| activity origin | docs/codex-frontend/activity.md; Islands CI | passed within documented visual evidence limits |
| local complete gate | root independently reran just check on integrated source; correction gate passed | passed |
| independent review / CI | reviewer approved feature and CI correction; [both OS checks](https://github.com/2lab-ai/llmux/actions/runs/37631245560), [all three Islands checks](https://github.com/2lab-ai/llmux/actions/runs/37631245544) | passed at 8e90683 |
| preview / installed smoke | [preview-2026-10-07-1404-f85b26bb2b81](https://github.com/2lab-ai/llmux/releases/tag/preview-2026-10-07-1404-f85b26bb2b81); installed receipts below | passed |


## Round 2 — actual protocol and runtime receipts (2026-10-07)

- Actual Codex0.160.1 requests captured: ordinary function/custom tools and gen6
  additional_tools/namespace format. Native route preserves original item payloads.
- RED found by real client: subcommand -c replaced globally injected provider
  config, bypassing llmux. Launcher now injects inside exec/resume/etc; dedicated
  CLI regression passes. Native live CLI text and exec_command cycle passed.
- RED found by actual SDK: a tool_result passed as fresh prompt was discarded.
  Session-store continuation now preserves user/assistant/tool-result order;
  actual Claude SDK OAuth CLI text, exec_command and custom apply_patch+readback
  passed. Raw receipts: /private/tmp/llmux-codex-live/{native-tool,claude-tool,claude-patch}.out.
- Native gateway completed envelope can have output=[] after real output_item.done;
  JSON aggregation now merges collected items. A regression fixture covers it.
- Stream disconnect receipt: Node/SDK pids41802/41895 in group41802 observed;
  both absent after HTTP close. Receipt /private/tmp/llmux-codex-live/cancel-receipt.json.
- Activity WU: TUI349, shared core49, macOS104, Linux source-contract9 tests passed;
  rendering buffer coordinates and limitations in docs/codex-frontend/activity.md.
- Full gate exposed old tests expecting /v1/models fallback. Contract intentionally
  changes that route to OpenAI model list; tests now assert list shape and keep
  unsupported-route coverage at /v1/unsupported. Two timing/network tests failed
  under first full parallel run and passed isolated reruns; full rerun required.
- Main advanced independently to1991a8b (#187) while feature work was in progress.
  Preserve that classifier change by integrating latest main after feature commit,
  then rerun the full gate and external review.

At Round 2, the remaining gates were main integration, its full gate,
independent review, CI, preview publication and installed-binary smoke. These
historical gaps are closed by the final round below.

- SDK final gate: 23/23 actual-SDK+unit tests pass, including structured JSON
  object/scalar/array/null/local references and external tools. SDK native child
  is terminated before iterator cleanup, avoiding unrequested extra inference.
- Pre-header cancellation verified separately: SDK processes59582/59631 alive
  before any readable HTTP bytes, both gone0.17s after socket shutdown. Receipt
  /private/tmp/llmux-codex-live/cancel-before-headers-receipt.json.

## Round 3 — main integration

Feature source committed as a9189d9 after the complete just check passed. Main
1991a8b is integrated without dropping its auto-classifier tests or documentation.
The outer classifier deadline and request-owned model shape remain intact; only
Responses ingress uses the new boundary adapter. Integrated gate receipt:
/private/tmp/llmux-codex-integrated-check.log — complete just check passed (exit 0),
including all auto_classifier and Responses e2e cases. Integrated cargo build
also passed; packaged bootstrap/live schema verification remains release work.

## Round 4 — CI packaging correction

PR #188 Arch KDE CI exposed a reduced Docker source context missing the three
compile-time embedded bridge files. Both the initial COPY and the local Git
fixture consumed by makepkg now carry those files. Docker context excludes
node_modules; the pinned manifests and script remain included. The Islands CI
path filters include bridge and Docker context rules. Other release
workflows build a complete checkout. CI is rerun on the corrected source before
release; the feature remains in-progress until installed-preview verification.

The same CI run exposed the existing macOS shutdown-test fixture reaching the
real upstream through its default startup idle probe. The fixture now disables
that unrelated network work and preserves daemon stderr on timeout; its 1-second
drain and 15-second process-exit assertions remain unchanged. CI did not retain
the original daemon stderr, so a specific runtime-stall cause is not claimed.

Local correction receipts: complete just check passed (including the unchanged
drain deadline assertions), /private/tmp/llmux-codex-packaging-check.log. A real
Docker build using the same COPY and fixture git-add lines cloned the resulting
Git repository and byte-compared all three bridge assets successfully; receipt
/private/tmp/llmux-codex-packaging-context.log. The full Arch Qt/package job remains
the remote CI gate, rather than being inferred from the smaller context check.

## Round 5 — installed-preview account failure (reopened)

All five checks passed on d4ac07a, merged as 8029d95 in PR #188. The first installed
preview passed the native Codex tool smoke but failed the Claude SDK smoke: SDK
`oauth_org_not_allowed` was mapped to transient 502, so client retries selected
the same unusable account. At this round the release acceptance gate failed, so status stayed
in-progress. No user accounts were manually changed as a workaround.

Root reproduced RED using an isolated two-account configuration with the same
bad-first/healthy-second credentials, idle probes disabled and no token refresh:
/private/tmp/llmux-sdk-failover-live/old-red.json returned 502 in 1.9 seconds. The
original user's account configuration is untouched. Correction branch
fix/codex-sdk-account-errors owns bridge typed-error mapping, explicit Rust
account disposition, shared failover and regression/document updates. Gates and
new live/release receipts are recorded before completion.

Root also sent the same rejected credential through native `/v1/messages` and
observed HTTP 403 with `OAuth authentication is currently not allowed for this
organization.` and `oauth_not_allowed_for_organization` in the response details.
Receipt: /private/tmp/llmux-sdk-failover-live/native-policy.json. This establishes
that marking this shared credential unusable does not disable a working native
Claude route because of an SDK-only local configuration assumption.

Correction gates: full just check passed (1,363 library tests, 24 CLI tests,
101 e2e tests plus the remaining suites), receipt
/private/tmp/llmux-sdk-account-just-check.log; bridge SDK suite passed 45/45.
Focused Rust subprocess tests cover four permanent account errors, same-request
healthy failover, zero refresh for policy failures, unchanged request/transient
health and one refresh for expired authentication.

Observed live GREEN with the exact isolated bad-first/healthy-second setup:
/private/tmp/llmux-sdk-failover-live/fixed-green.json returned HTTP 200 in 2.75
seconds with exact text `LLMUX_AUTH_FAILOVER_OK`. The daemon log shows the first
account rejected with SDK organization HTTP 403, then request id 1 completed
using the healthy second account at `/v1/responses`, endpoint OpenAi. The old
binary returned HTTP 502 for that same setup. Corrected prerelease installation
and actual installed-client tool smokes remain required before shipped status.

Root's independent full gate observed two refreshes in the C3 exactly-once
fixture. Source inspection identifies a scheduling race consistent with this
result (the failing schedule itself was not traced): its mock returns a 1-hour
token, inside the 7-hour background-refresh window; a
late startup sweep can issue a second legitimate refresh after the request's
forced refresh. This same race is already documented in server.rs near pending
receipt hydration. The native 401, concurrent OAuth request-refresh and SDK 401
fixtures now set refresh_ahead_secs=0 so their counts measure request-owned
refreshes. Exact counts, bearer order and persistence assertions are unchanged;
the separate background-refresh acceptance test remains active. Complete just
check passed afterward, /private/tmp/llmux-sdk-refresh-fixture-check.log.

## Round 6 — corrected preview and installed client

- Initial feature: [PR #188](https://github.com/2lab-ai/llmux/pull/188), tested
  head `d4ac07aaff690b23722373d2d1a2fffc99248075`, merged as `8029d95`.
  [Initial Rust CI](https://github.com/2lab-ai/llmux/actions/runs/37623111739)
  and [initial Islands CI](https://github.com/2lab-ai/llmux/actions/runs/37623111832)
  passed, but its first installed Claude smoke failed as preserved in Round 5.
- Runtime correction: [PR #189](https://github.com/2lab-ai/llmux/pull/189), reviewed
  and tested head `8e906830f30b23c0e9a18df7888f7c5dc90efb59`, merged as
  `f85b26bb2b81a49fd285497e2905aff3ad7c155f`. Both changes are included in the corrected preview.
- Final CI: [macOS and Ubuntu checks](https://github.com/2lab-ai/llmux/actions/runs/37631245560) and [semantic core, Arch KDE,
  macOS parity](https://github.com/2lab-ai/llmux/actions/runs/37631245544) all passed on the correction's exact PR head. Arch
  exercised the complete Qt/package path, including the makepkg Git clone.
  Root independently reran the full local gate successfully; receipt
  `/private/tmp/llmux-sdk-account-root-final-check.log`.
- Actual isolated regression: the old binary returned 502 for a restricted
  first account; the corrected binary rejected that account with SDK 403 and
  completed the same HTTP request on the healthy second account with 200 and
  `LLMUX_AUTH_FAILOVER_OK`. Native Messages also rejected the same restricted
  credential with 403. The root account configuration was not modified.
- Published preview: [preview-2026-10-07-1404-f85b26bb2b81](https://github.com/2lab-ai/llmux/releases/tag/preview-2026-10-07-1404-f85b26bb2b81);
  [build workflow](https://github.com/2lab-ai/llmux/actions/runs/37633633708).
- Installed client/server both report `llmux 0.2.23 (preview 2026-10-07-1404-f85b26bb2b81)`.
  Installed artifact SHA-256: `dd4663d1c3a1416df73b3995638b89b0447e18967827ea8faf8cfa7c6cd1e320`.
- Shared-daemon Activity recorded four successful `/v1/responses` turns with
  `endpoint: open_ai` across both provider families; receipt
  `/private/tmp/llmux-codex-release/corrected/installed-activity.json`.
  Those shared-pool smokes selected a healthy Claude account directly.
- The installed preview binary was separately exercised with an isolated
  restricted-first/healthy-second pool, using the normal packaged SDK cache:
  SDK 403 excluded the first account and the same HTTP request id 1 completed
  on the second account with 200 in 3.15 seconds and exact text
  `LLMUX_INSTALLED_FAILOVER_OK`. Receipts:
  `/private/tmp/llmux-sdk-failover-live/installed-green.json` and
  `/private/tmp/llmux-sdk-failover-live/installed-daemon.log`.
  This isolated failover proof is distinct from the successful shared-pool
  tool smokes; user account configuration was unchanged.
- Native Codex model `gpt-5.6-sol`: `CODEX_HOME=/private/tmp/llmux-codex-release/corrected/installed-smoke/codex-home /opt/homebrew/bin/llmux run --codex -- exec --ignore-user-config --ephemeral --skip-git-repo-check -C /private/tmp/llmux-codex-release/corrected/installed-smoke/work --sandbox read-only -m gpt-5.6-sol -c model_reasoning_effort="low" --json "Use exec_command to run printf LLMUX_INSTALLED_NATIVE_TOOL_OK and reply with that output."`.
  Observed: Actual client command_execution exit 0/output LLMUX_INSTALLED_NATIVE_TOOL_OK; exact same final agent message; turn.completed; llmux/Codex exit 0; 6.56 seconds.
  Raw receipt: `/private/tmp/llmux-codex-release/corrected/installed-smoke/native.out`.
- Claude model `claude-haiku-4-5-20251001` through official Claude Agent SDK:
  `CODEX_HOME=/private/tmp/llmux-codex-release/corrected/installed-smoke/codex-home /opt/homebrew/bin/llmux run --codex -- exec --ignore-user-config --ephemeral --skip-git-repo-check -C /private/tmp/llmux-codex-release/corrected/installed-smoke/work --sandbox read-only -m claude-haiku-4-5-20251001 -c model_reasoning_effort="low" --json "Use exec_command to run printf LLMUX_INSTALLED_CLAUDE_TOOL_OK and reply with that output."`.
  Observed: Actual client command_execution exit 0/output LLMUX_INSTALLED_CLAUDE_TOOL_OK; exact same final agent message; turn.completed; llmux/Codex exit 0; 5.63 seconds.
  Raw receipt: `/private/tmp/llmux-codex-release/corrected/installed-smoke/claude.out`.
- Final integrated-source probes additionally verified native nonstream text,
  a fresh HOME installing packaged SDK 0.3.292 without a repository checkout,
  private cache mode 0700, and actual Codex output-schema + external-tool output
  as valid JSON. SDK unit/actual-SDK suite: 45 tests passed. Pre-header and
  midstream cancellation observed both Node and SDK child processes terminate.
- Activity rendering has direct ratatui buffer evidence, native macOS build and
  projection tests, and Linux source-contract plus CI coverage. The feature
  does not claim a local Linux graphical-runtime or native screen-pixel probe;
  these evidence boundaries remain explicit in the activity guide.

All seven acceptance scenarios are closed. The original request remains verbatim
in SSOT. This documentation-only closure does not change released runtime code.

Documentation closure also passed the mandatory complete `just check` before
commit; receipt `/private/tmp/llmux-codex-docclosure-check.log` (exit 0).
