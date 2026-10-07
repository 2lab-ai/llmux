# Islands get-started traces
Status: in-progress
Date: 2026-10-08

## T1 — local connection

0. App startup/retry captures the selected endpoint; connecting renders progress.
1. Direct trusted-binary Process invokes hidden `islands-connection --port N` via
   private pipes. Existing config loader owns reading local proxy configuration.
2. Requested port must equal configured proxy port; any remote config/flag rejects.
   Output contains only canonical local endpoint and proxy control key.
3. Swift validates JSON and endpoint → private endpoint-bound control credential →
   request x-api-key → existing daemon client_auth → shared dashboard → existing
   Rust projection → visible ready/empty state. Reconfiguration invalidates captured
   results; every request reads a fresh handoff; authorization failure invalidates in-flight handoffs for the next bounded retry.
4. No account/config mutation in handoff. Secret never enters defaults, UiState,
   logs, clipboard or capture. Local auto-start remains existing behavior.
5. Missing helper/timeout/nonzero/mismatch: actionable connection failure. HTTP
   401/403 is an existing unauthorized daemon, never a reason to spawn/restart.
6. Dashboard receipt drives readiness; no optimistic success from helper alone.
7. Tests inspect request header scope with synthetic keys, never live secrets.

## T2 — connect and start coding

0. Empty view → labeled account CTA → existing OAuth provider flow → dashboard
   with an account → client choice + project folder + explicit start button.
1. Existing authenticated account/control endpoints remain unchanged.
2. Directory selection must be a local existing directory; chosen client is an enum.
3. Client + folder → fixed llmux binary + quoted `run [--codex]` command → Terminal
   through a fixed AppleScript program with arguments → existing CLI lifecycle.
4. Only explicit launch opens Terminal; copying a command is a separate action.
   No new provider credential flow; unsupported remote launch fails clearly.
5. Missing executable/folder/automation refusal shows inline failure, never claims
   the model session ran. UI may claim only Terminal dispatch after successful dispatch.
6. Existing client's terminal session is the actual destination, not a mock console.
7. Pure quoting/command tests include apostrophes, quotes, dollar substitution and
   newlines; injected launch executor proves exactly one dispatch on explicit action.

## T3 — verification/media

0. Offscreen SnapshotMode begins before app window/model startup.
1. Fixture state → production SwiftUI views → native bitmap renderer → PNG artifacts.
2. Snapshot env selects explicit onboarding/error/ready states; stable fake identities.
3. No network, helper execution, account changes or Terminal effects are allowed. Launcher remote/local state, tool availability and project paths use fixed fixtures.
4. Capture files live in session scratch; media work unit labels fixtures and saves draft.
5. Missing fixture/render failure returns nonzero; no synthetic success receipt.
6. Real view screenshots plus interaction tests, then isolated safe demo footage.

## File map

CLI helper/dispatch/tests; native LlmuxClient/DaemonLauncher/IslandUsageModel;
new get-started/launch support and Swift tests; IslandUsageView/NotchViewModel;
SnapshotMode and synthetic dashboard fixture; project.yml and Info.plist Automation purpose; docs/llmux-islands.md and native README; shared port spec/design explicit native amendment.

ADDED: remote settings keep-key intent is endpoint-bound; changing a remote host/port
requires explicit replacement, and switching to local discards the remote key.
