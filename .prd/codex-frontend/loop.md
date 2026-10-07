# Codex frontend convergence loop
Status: in-progress
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
| protocol contract RED | baseline /v1/responses absent → actual HTTP regression | implemented and passing |
| native Codex model text/tool | actual CLI native-tool.out | verified pre-integration |
| Claude SDK model text/tool | actual CLI claude-tool.out / claude-patch.out | verified pre-integration |
| activity origin | docs/codex-frontend/activity.md | implementation gates pass |
| just check | /private/tmp/llmux-codex-just-check.log | final feature source passed (exit 0) |
| independent review / CI | none | pending |
| preview / installed smoke | none | pending |


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

Remaining: main integration and its full gate, integrated-head review, CI, preview
publication, installed-binary smoke. Status remains in-progress.

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
