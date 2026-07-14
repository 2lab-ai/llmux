# Claude Code system prompts (multi-model, real wire captures)

**This directory ships the actual system-prompt text** captured from llmux
`raw-io.jsonl` on 2026-07-14, lightly sanitized (paths / host identity /
session memory). Not a taxonomy essay.

Use these when you route Claude Code (or Agent SDK) through non-Claude models
(`grok-4.5`, `gpt-5.6-sol`, …) and need to know **what the model is actually
being told**.

| Log source | Path |
| --- | --- |
| bodies | `~/.local/state/llmux/raw-io.jsonl` |
| meta | `~/.local/state/llmux/activity.jsonl` |

Related: [FAQ compact](../faq.md) · [models](../models.md) · [ops ref](../operational-reference.md)

---

## Open the samples

All files: [`samples/`](./samples/)

| # | File | Family | Model on wire | tools | chars | `cc_version` |
| --- | --- | --- | --- | ---: | ---: | --- |
| 01 | [cli main agent](./samples/01-cli-main-agent-grok-4.5.system.txt) | Main CLI agent | `grok-4.5` | 203 | ~15k | `2.1.207.4d8` |
| 02 | [security monitor](./samples/02-security-monitor.system.txt) | Auto-mode classifier | grok/claude path | 0 | ~106k | `2.1.207.899` |
| 03 | [SDK exec-summary](./samples/03-sdk-exec-summary.system.txt) | Session summary (SDK) | `gpt-5.6-sol` | 0 | 873 | `2.1.111.0c3` |
| 04 | [SDK Slack bot](./samples/04-sdk-slack-bot-gpt-5.6-sol.system.txt) | Agent SDK / soma-work | `gpt-5.6-sol` | 88 | ~11k | `2.1.111.0c3` |
| 05 | [dual-persona reviewer](./samples/05-cli-subagent-dual-reviewer-gpt-5.6-sol.system.txt) | CLI subagent | `gpt-5.6-sol` | 1 | ~6k | `2.1.207.fcf` |
| 06 | [completion auditor](./samples/06-completion-auditor.system.txt) | Goal audit | `gpt-5.6-sol` | 0 | ~1.8k | `2.1.111.402` |
| 07 | [persona subagent (elon)](./samples/07-cli-subagent-persona-gpt-5.6-sol.system.txt) | CLI subagent | `gpt-5.6-sol` | many | ~5–7k | `2.1.207.a97` |
| 08 | [Claude agent (haiku capture)](./samples/08-cli-or-sdk-cli-agent-claude-haiku.system.txt) | CLI/SDK-cli agent | `claude-haiku-…` | 146 | ~28k | `2.1.208.188` |

Tool name lists (not full schemas):  
[`01-…tools.txt`](./samples/01-cli-main-agent-grok-4.5.tools.txt) ·
[`04-…tools.txt`](./samples/04-sdk-slack-bot-gpt-5.6-sol.tools.txt)

```bash
# read one
less docs/system-prompts/samples/01-cli-main-agent-grok-4.5.system.txt
less docs/system-prompts/samples/02-security-monitor.system.txt
```

---

## What each sample is for

### 01 — Main CLI agent (grok)
Full Claude Code interactive harness as sent when `/model grok-4.5`.  
Sections: Harness · Communicating · Memory · Environment · Scratchpad ·
Context · Chrome. Environment may still claim Fable 5 — **routing truth is
llmux `group=grok`**, not that prose.

### 02 — Security monitor (~106k)
Auto-mode allow/block classifier. Output must start with
`<block>yes|no</block>…`. HARD vs SOFT BLOCK rules live here.  
Often runs as a **second request** after the agent intends a tool call.
On this host’s 2.5GB raw-io window: **0/163** `gpt-5.6-sol` `agent_full`
requests carried this family — do not assume gpt gets the same monitor path.

### 03 — SDK exec-summary (873 chars)
Forked “summarize the engineering session” system. **Not proven identical to
CLI `/compact`.** History is in `messages`, not system.

### 04 — SDK Slack bot on gpt-5.6-sol
soma-work style: `<system_prompt>` + `<workflow_prompt>` + persona + memory.
Same model id as CLI subagents, **different product**. Host L1 memory body
redacted in the sample.

### 05 — Dual-persona reviewer (gpt-5.6-sol)
`gpt56-reviewer` contract: Read-only ~5-call budget, zhuge + elon sections,
merged MUST-FIX.

### 06 — Completion auditor
Single-line JSON only:
`{"completed":bool,"reason":string,"remaining":string[]}`.

### 07 — Persona subagent (elon on gpt)
`cc_is_subagent=true` + generated persona body.

### 08 — Claude agent capture
Another harness shape (`cc_entrypoint=sdk-cli`) for comparison with 01.

---

## Usage fields (all of the above models)

Response bodies only carry Anthropic-shaped:

```json
{
  "input_tokens": 0,
  "output_tokens": 0,
  "cache_read_input_tokens": 0,
  "cache_creation_input_tokens": 0
}
```

**No** context-window size, remaining tokens, rate-limit remaining, or quota.
`/context` UI % is client-estimated (often from `[1m]` suffix).

```text
approx_used ≈ input + cache_read + cache_creation
```

---

## Operator rules (short)

1. **Open the sample file** for the family you are in — don’t guess from model id.  
2. Family fingerprint = system head / `cc_entrypoint` / tool count (see table).  
3. Trust llmux `group`/`account` over Environment “you are Fable 5”.  
4. Mid-200k client stop on gpt: `/model opus[1m]` → `/compact` →
   `/model gpt-5.6-sol[1m]` ([faq](../faq.md)).  
5. Re-verify samples when `cc_version` in the billing header changes.  
6. Do not commit unsanitized raw-io (90d retention often includes secrets).

---

## Satellite docs

| File | Role |
| --- | --- |
| [families.md](./families.md) | Fingerprints + section maps |
| [topology.md](./topology.md) | Dated counts + re-extract script |
| [operator-notes.md](./operator-notes.md) | Redirect → this README |

Sanitization applied to samples: `$HOME` path rewrite, session UUID wipe,
monitor user-id placeholder, SDK L1 memory body redacted, session `gitStatus`
snapshot redacted. Prompt **logic text is intact**.
