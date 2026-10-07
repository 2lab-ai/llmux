# Islands get started
Status: shipped
Date: 2026-10-08

## Problem

The default empty Usage panel hides account setup behind a 24px plus icon; loading
can look like no accounts. Offline copy names endpoint/credentials without a next
action. A connected user cannot start Claude Code or Codex from the app. Current
local GUI HTTP auth also conflicts with the daemon's admin-only control API.

## Acceptance

1. New install: show distinct connecting, needs-attention, no-account and ready
   states, with an obvious browser account-connection action and retry/settings.
2. Local control requests authenticate through a narrow CLI helper, without reading
   provider credentials, persisting/rendering/logging the key or weakening server auth.
   Remote config, wrong port and stale endpoint results fail closed.
3. Connected user chooses Claude Code/Codex and a project folder, then explicitly
   opens Terminal with llmux. Missing tools and unsupported remote launch show an
   actionable result. Untrusted folder text cannot become shell/AppleScript code.
4. Existing account grids, shared semantic state and Linux behavior remain intact.
5. Tests exercise auth isolation, races, bounded helper failure, launch quoting and
   UI-state derivation; actual native offscreen renders prove readable layout. No
   snapshot invokes daemon, OAuth, Terminal or user-preference writes.
6. Owning guides explain ordinary installation separately from source-building.
   Independent review and required gates precede publication.

Source boundary, executable paths and evidence: [trace](islands-get-started/trace.md),
[original request](islands-get-started/ssot.md), [loop](islands-get-started/loop.md).

## Delivery verified 2026-10-08

Shipped in [preview 2040](https://github.com/2lab-ai/llmux/releases/tag/preview-2026-10-07-2040-7fe64d172929),
source 7fe64d1. The installed CLI/server and Islands app match that release; the
unmodified native HTTP executor discovers the installed CLI and authenticates
dashboard/status. Five reviewed native-view videos were saved as separate Threads
drafts without publishing. See the [acceptance receipts and limits](islands-get-started/loop.md#round-4--paired-preview-and-closure).

Actual GUI connection state and Finder/Terminal Automation dispatch were not
observed. The app process/payload, native HTTP path, command quoting and installed
CLI tool roundtrips were verified separately; fixture footage does not imply a
live login or button-click recording.
