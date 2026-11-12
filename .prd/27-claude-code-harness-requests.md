# 27 — Claude Code harness requests follow the main model's provider
Status: in-progress
Date: 2026-11-12

Original request and frozen scope: [internal-request-routing/ssot.md](internal-request-routing/ssot.md).
Round plan and open gaps: [internal-request-routing/loop.md](internal-request-routing/loop.md).
Supersedes the GPT-only exception in [02-architecture.md](02-architecture.md#claude-code-auto-mode-routing-exception-2026-10-07)
(#187, #191, #194).

## Problem

Claude Code sends requests of its own beside the user's turns: a session title, a security check
before risky tools, a status check for phone notifications, recaps, suggestions and helper turns.
It names a Claude model for each one (`claude-haiku-*`, `claude-sonnet-*`), whatever model the
user picked for the session. llmux moved some of these to GPT models in GPT sessions, but not all
of them, and none in Grok sessions.

Measured [LIVE owner Mac, raw-io partition 2026-11-12, Claude Code 2.1.322]:

1. **The title leaks to Claude in a GPT session.** Session `d9a220bf` (main model `astra`):
   warmup `astra` (`max_tokens: 1`) at 15:36:58 → title `claude-haiku-5-5` started 15:37:21.867 →
   first main turn started 15:37:22.122. The title arrives 255 ms before the first main turn, so the
   session is still unknown. The launch hint on both requests was 4.5 hours old
   (`claude-opus-5-5[1m]`, written before `/model astra`), so the 120-second bootstrap rejected it.
   The title went to Claude Haiku.
2. **Grok sessions have no routing at all.** Session `20043f30` (main model `grok`): the title went
   to `claude-haiku-5-5` and 3 of 3 security checks went to `claude-sonnet-5`. The existing code
   only knows "GPT session or not".
3. **A harness family is not recognised.** "A user kicked off a Claude Code agent to do a coding
   task and walked away…" is a status classifier for phone notifications. It names
   `claude-haiku-5-5`, carries no tools, and fired 26 times in one Claude session that day. llmux
   classified it as a user turn (`지시` in the web dashboard), and would send it to Claude in GPT or
   Grok sessions too.

## Goal

One rule, not a list of special cases:

> In a Claude Code session whose main model is served by provider P (Codex or Grok), every
> harness request that names a Claude tier is served by P's model for that tier.

A harness request is one Claude Code sends without the user typing it. The tier is read from the
Claude model the request names: `opus`, `sonnet` or `haiku`.

## Policy

| Harness request | Tier it names | Codex main | Grok main |
| --- | --- | --- | --- |
| title, quota probe, status classifier | haiku | `luna`, effort low | `grok`, effort low |
| security monitor (both stages) | sonnet | `auto_classifier_model` (`luna`), effort medium | `grok`, effort medium |
| helper turns (subagent, recap, suggest, summary, audit, compact) naming a tier | haiku / sonnet / opus | `gpt_model_mapping`: `luna` / `terra` / `terra`, client effort | `grok`, client effort capped at low / medium / high |
| any request that names the main model itself (warmup, compact) | — | unchanged | unchanged |

- Codex uses smaller models. The default `gpt_model_mapping.opus` moves from `sol` to `terra`, so
  Codex harness requests use only `terra` and `luna`.
- Grok has one model line, so it lowers effort instead. The request-owned effort wins over the
  global `grok.reasoning_effort` (live value `xhigh`), the same way the Codex route already wins
  over the global Codex effort.
- Every target is an alias (`terra`, `luna`, `grok`). It is resolved on each request through the
  catalog (Codex) or the configured Grok pin (Grok), so a model update moves these requests without
  a config change.

## Session context

The rule needs to know the main model before the harness request arrives. llmux learns it per
tenant and Claude Code session (`metadata.user_id` device and session), in this order of authority:

1. **Main turn** — the Claude Code identity, tools present, not a subagent, `max_tokens` not 1.
   Always overwrites. A `/model` switch back to Claude clears the routing.
2. **Warmup** — the Claude Code identity, `max_tokens: 1`, no tools. Claude Code sends it with the
   session's main model when a session starts. It seeds the session only when the session is
   unknown and never overwrites a main turn.
3. **Launch hint** — `llmux run` headers, at most 120 seconds old, for an exact quota or title
   request only. Unchanged.

Entries remain bounded (4096), expire after 6 hours and are not persisted.

## Non-goals

- Requests that name a non-tier model stay as they are — the security monitor included: `fable`, explicit GPT or Grok ids, and
  `claude -p` calls a user scripts inside a session. Subagent `fable-5-2` rows in a GPT session are
  the agent file's explicit choice.
- Compaction keeps the main model. Claude Code sends compact with the main model's own name, so it
  already reaches the main provider; a smaller model would weaken the summary the session continues
  from.
- Claude Agent SDK host sessions (soma-work). Their subagent models are explicit picks (for example
  a `haiku` agent beside a `luna` agent), so a tier rewrite would override the host's choice.
- Gemini and OpenRouter main sessions. Claude Code on Gemini is refused; OpenRouter is a passthrough
  with no tier line-up.
- Arbitrary task-content routing. Only recognised harness signatures are routed.

## Acceptance scenarios

Each runs against the mock upstream in `tests/e2e.rs` unless marked live.

- A1. GPT session: warmup `astra` → title `haiku` before any main turn → served by `luna`, effort
  low, JSON schema format kept.
- A2. Grok session: warmup `grok` → title `haiku` → served by the Grok pin, effort low, although
  the global Grok effort is `xhigh`.
- A3. Grok session: security monitor `claude-sonnet-5` → served by Grok at effort medium; the
  verdict is validated exactly as on Codex; no Claude account is needed.
- A4. Status classifier in a GPT session → `luna`, effort low; in a Grok session → Grok, low. It
  classifies as `status`, not `user`.
- A5. Claude main session: no harness request is rerouted.
- A6. A warmup naming Claude after a GPT main turn does not clear GPT context; a Claude main turn
  does.
- A7. Default `gpt_model_mapping.opus` is `terra`; targets stay alias strings and resolve at
  request time.
- A8 (live). Replaying the captured 2026-11-12 warmup and title from both sessions against the new
  build sends the title to `gpt-6-luna` and to Grok, with no Claude account involved.
