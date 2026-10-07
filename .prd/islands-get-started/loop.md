# Islands get-started loop
Status: in-progress
Date: 2026-10-08

Base 7af8018; integrated released main 34eb955 before final gates.
Worktree: `.worktrees/feat-islands-get-started`.
Implementation WU: local-auth, first-run and explicit project launch in native macOS.
Separate media WU: actual-view videos and Threads drafts. Stable v0.2.24 excluded.

## Round 1 — trace, implement, verify

Read-only audit confirmed three frictions and existing private snapshot renderer.
Parent approved the narrow CLI-owned handoff before code. No server auth weakened;
no provider accounts, shared daemon, production app or shared preferences changed.

| Before | After | Why |
|---|---|---|
| Loading resembled an empty list; setup hid behind + | Connecting, account setup, offline and ready views, labeled CTA | A new user can identify the next action |
| Local GUI omitted control authentication | Bounded endpoint-matched CLI pipe → HTTP executor only | Existing daemon admin policy now remains intact and usable |
| Usage screen had no project entry point | Client choice, folder picker and explicit Terminal action | Start existing coding tools without constructing llmux commands |

Independent review reproduced a descendant-held stdout pipe escaping the timeout.
The fix drains nonblocking under one monotonic deadline, caps bytes before appending,
and closes the pipe on failure. Actual descendant/oversize regression tests pass.
Review also corrected recovery to the app's release channel, added Terminal's
Automation purpose string, and made form-open fixture state match the live header.

Validation receipts (session scratch):
- `just check` after main integration: exit 0,
  `/private/tmp/llmux-islands-get-started-check.log`.
- Full native build/tests: 114 passed, exit 0,
  `/private/tmp/llmux-islands-get-started-tests.log`.
- Actual isolated cold start: helper with absent config does not write; empty
  configured daemon initializes key; helper returns only endpoint/key; authenticated
  dashboard returns 200. `/private/tmp/llmux-islands-coldstart.log`.
- Hostile folder names (quotes, dollar substitution, newline) executed through the
  generated shell command with fake CLI; cwd/argv correct, no injected file.
- Real SwiftUI offscreen fixtures: six `setup-*.png`, `receipts-detail.png`, stats
  and closed-label phases in `/private/tmp/llmux-islands-get-started-captures/`.
  Activity includes Anthropic→Claude and OpenAI→Claude/GPT; only OpenAI rows are gray.
  Synthetic accounts/project paths and fixed launch readiness; no network/actions.

Limits: Terminal app-context Automation/dispatch has not yet been observed; shell
quoting alone is not that receipt. Videos must label fixture data and cannot claim
an actual live click/login/model session. Publication, paired installation and
media drafts remain parent-owned acceptance before marking shipped.

## Round 2 — CI shared-fixture correction

PR #196 macOS shared-core job 112993452057 caught an omitted local gate: the app
bundle's dashboard fixture must be byte-identical to the core fixture. The initial
change updated only the app copy. Restore its exact bytes and construct the mixed
endpoint capture scenario only in SnapshotMode memory before the existing Rust
decoder/reducer. The parity test remains unchanged. Root `just check` does not run
subcrate tests; core and macOS-bridge gates are now explicit for this correction.
No production HTTP/auth/launcher behavior changes. Native capture comparison and
full gate results are recorded below after execution.

Correction validation: core 49 tests and macOS bridge 12 tests passed (including
unchanged fixture-byte assertion); root `just check` and native app build exited 0.
Logs: `/private/tmp/llmux-islands-fixture-{core,bridge,check,native}.log`.
All six regenerated statistics/Activity PNG files are byte-identical to the approved
be60c47 captures, including both OpenAI dark-gray rows. SHA-256 comparison receipt:
`/private/tmp/llmux-islands-fixture-capture-comparison.json` (`all_identical: true`).
Independent root review approved the three-file correction and independently proved
that the augmented JSON matches the earlier capture data. Existing videos need no
visual replacement; their original capture provenance remains be60c47.
