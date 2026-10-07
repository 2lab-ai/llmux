# Islands get-started loop
Status: shipped
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

## Round 3 — native authentication and media delivery

The secret-safe Swift harness compiled the actual 5711d96 LlmuxClient, settings
and DTO sources without changes; LocalControlAuth changed only its default loader
argument to select the newly built test CLI. On an isolated, empty-account daemon,
unauthenticated GET returned 403 and the actual Swift dashboard/status calls
succeeded through the CLI handoff. The config stayed byte-identical, HOME was
unchanged, settings were volatile only, no provider traffic occurred, and the
isolated daemon shut down. This proves the HTTP executor path while explicitly
excluding installed-binary discovery, Finder startup and Terminal Automation.
Durable sanitized receipt: `zbrain/workflow/receipts/llmux-islands-native-auth-2026-10-08/`.

Five distinct 1080×1920 H.264 videos (31.0, 15.0, 22.0, 20.0 and 14.4 seconds) use
the actual be60c47 native captures with disclosed demo data. Independent review
covered compositions, captions and saved-draft readbacks. Each was saved once as
a separate Threads draft; no existing draft was modified and no post was published
or scheduled. Source provenance remains be60c47; the 5711d96 fixture correction
preserved the relevant screenshots byte-for-byte.
Durable primary delivery/review receipts: `zbrain/workflow/receipts/2026-10-08-llmux-islands-shorts/`.

[PR #196](https://github.com/2lab-ai/llmux/pull/196) merged as 7fe64d1 after the
corrected head passed all five required CI jobs. Paired preview publication and
installation were then verified in Round 4 below.

## Round 4 — paired preview and closure

[Preview run 37683746006](https://github.com/2lab-ai/llmux/actions/runs/37683746006)
succeeded, including automatic tap publication. The release
[`preview-2026-10-07-2040-7fe64d172929`](https://github.com/2lab-ai/llmux/releases/tag/preview-2026-10-07-2040-7fe64d172929)
was published at 2026-10-07 20:51:07 UTC from 7fe64d17292999a8e1f1115b4faa9e4462db8c09.
[Main CI](https://github.com/2lab-ai/llmux/actions/runs/37681040795) and
[all three Islands jobs](https://github.com/2lab-ai/llmux/actions/runs/37681040598)
passed at the corrected PR head before merge.

Installed acceptance (2026-10-08 KST):
- CLI and running daemon both report `llmux 0.2.24 (preview 2026-10-07-2040-7fe64d172929)`.
  The installed arm64 CLI SHA-256 matches the released asset:
  `6b4e9f8d5b9baf779ad576414b54aaacb8122ae34a1822e60acec5cdc083b53f`.
- Islands reports `2026.10.07.2040`; its running process was observed and all three
  installed Mach-O payloads match the verified release ZIP.
- Six unmodified Swift files exercised default InstalledTools discovery → installed
  private CLI helper → actual daemon dashboard/status. Both requests succeeded,
  the config remained unchanged, and no key was printed or persisted by Swift.
- Actual installed Codex CLI tool roundtrips passed for `gpt-5.6-sol` and
  `claude-haiku-4-5-20251001`: each executed one command and exited 0 with its marker
  in the final response. Claude's final reply was verbose, not exact-marker-only.
  Four corresponding Activity records were `open_ai`, HTTP 200.
- The previously released stable v0.2.24 tag/source remains unchanged; the locally
  running pair is this newer preview for the Islands feature.

Durable installation receipt: `zbrain/workflow/receipts/llmux-islands-preview-2026-10-08.json`.
Root independently read the receipt and installed version before approving closure.

Verification limits remain explicit: Accessibility read returned -25211, so actual
GUI connection state was not observed. The successful unmodified HTTP harness is
not a GUI-session receipt. Finder/Terminal Automation dispatch is still unobserved;
no claim of a live GUI click/login/model session is inferred from fixture videos.

Final docs-only gate: `just check` exited 0 (1372 library, 25 CLI, 106 e2e with one
existing ignored test, remaining suites/doc-tests passed), recorded in
`/private/tmp/llmux-islands-docclosure-check.log`. This closure changes four Markdown
files only, retains the original request and failed-CI history, and uses `[skip ci]`
for publication so unchanged runtime is not rebuilt or restarted.
