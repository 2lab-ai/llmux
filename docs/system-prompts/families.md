# Prompt families (wire contracts)

Each family is a **different system-prompt contract**. Model swap reuses the
family; it does not invent a new one.

Samples: live `raw-io.jsonl` on **2026-07-14**. Pin `cc_version` per family —
**re-verify section maps when that version changes.**

Rough: sizes below are **characters** (~4 chars ≈ 1 token English, not exact).

---

## Fingerprint table (start here)

| Fingerprint in system (or header) | Family | class |
| --- | --- | --- |
| `You are Claude Code…` w/o `cc_is_subagent` | Main CLI agent | execution |
| `cc_is_subagent=true` | CLI subagent | execution |
| `cc_entrypoint=sdk-ts` + Slack/workflow rules | SDK / Slack bot | execution |
| `You are a security monitor for autonomous AI coding agents` | Security monitor | control |
| `executive summaries of engineering work sessions` | SDK exec-summary | control |
| CLI `/compact` summarize-transcript instruction | CLI compact | control (**uncaptured**) |
| `goal completion auditor` | Completion auditor | control |
| `You ARE the external reviewer, running on GPT-5.6` | Dual-persona reviewer | control |

Operator spine: [README.md](./README.md). Evidence: [topology.md](./topology.md).

---

## Execution families

### 1. Main CLI agent

**When:** user chat loop in Claude Code terminal / desktop.

**Header:**

```text
x-anthropic-billing-header: cc_version=…; cc_entrypoint=cli;
You are Claude Code, Anthropic's official CLI for Claude.
```

**Sample pin:** `cc_version=2.1.207.4d8` · model `grok-4.5` · ~16k chars · 203 tools.

**Sections (that sample):**

1. Security / dual-use preamble  
2. `# Harness`  
3. `# Communicating with the user`  
4. `# Session-specific guidance`  
5. `# Memory`  
6. `# Environment` — **identity string may still say Fable 5 when routed elsewhere**  
7. `# Scratchpad Directory`  
8. `# Context management`  
9. `# Claude in Chrome browser automation`  
10. Trailing `gitStatus:` snapshot  

**Tools:** full core set + session MCP servers.

| Model | Observed |
| --- | --- |
| `grok-4.5` | Full harness; Anthropic-shaped SSE |
| `claude-fable-5` / opus | Same family; wording drifts with `cc_version` |
| `gpt-5.6-sol` pure CLI-main | **Not pinned** in the 2026-07-14 window (see topology residuals) |

---

### 2. CLI subagent (`Task` / `Agent`)

**When:** parent delegates a bounded worker.

**Header:**

```text
cc_entrypoint=cli; cc_is_subagent=true;
You are Claude Code, … running within the Claude Agent SDK.
```

Then a **persona or task brief** (elon, zhuge, explore, reviewer, …).

**Sample pins:**

| Role | cc_version | tools | ~chars |
| --- | --- | ---: | ---: |
| Dual-persona reviewer | `2.1.207.fcf` | 1 (`Read`) | ~7k |
| Persona worker | `2.1.207.a97` | 1–197 | ~5–8k |

Often includes an explicit tool budget (“~5 tool calls”).

---

### 3. SDK / Slack bot (soma-work style)

**When:** Agent SDK host (not Claude Code CLI main loop).

**Header:**

```text
cc_entrypoint=sdk-ts; cch=…;
You are a Claude agent, built on Anthropic's Claude Agent SDK.
```

**Sample pin:** `cc_version=2.1.111.0c3` · `gpt-5.6-sol` · ~13k + memory · 88 tools.

**Structure:**

```text
<system_prompt>   Basic Rules, GH workflows, subordinate MCP, Slack formatting
</system_prompt>
<workflow_prompt> Default workflow, skills list
</workflow_prompt>
# Facts / Repository
<persona> … </persona>
MEMORY block (L1 char budget)
## Your Personal Skills
```

Same model id can serve CLI subagent **or** this family — do not conflate.

---

## Control families

### 4. Security monitor (auto-mode classifier)

**When:** before a risky tool call, harness may classify allow/block.

**Header:**

```text
You are a security monitor for autonomous AI coding agents.
```

**Sample pin:** `cc_version=2.1.207.899` (grok-adjacent) / `2.1.208.65f` · ~106k chars · tools 0 · `max_tokens` ~2112.

**Sections:** Context · Threat Model · Input · Default Rule · Scope · User Intent
Rule · Evaluation Rules · Environment · Definitions · **HARD BLOCK** ·
**SOFT BLOCK** · ALLOW exceptions · Classification Process · Output Format ·
Session Context.

**Output contract:**

```text
<block>yes</block><category>…</category><reason>[Rule Name] …</reason>
# or
<block>no</block>
```

**Routing:** agent model ≠ monitor model is normal. Grok agent turns are often
followed by Claude or grok monitor. **gpt-5.6-sol:** 0/163 `agent_full` in the
2.5GB window carried this family — do not assume monitor ran.

---

### 5a. SDK executive-summary (captured)

**When:** SDK host forks a session to produce an engineering work summary
(soma-work style). **Not proven identical to CLI `/compact`.**

**Sample pin:** `cc_version=2.1.111.0c3` · `gpt-5.6-sol` · **873 chars** · tools 0 ·
`max_tokens` 32000.

```text
You generate executive summaries of engineering work sessions from conversation history.
… Never fabricate. Never hedge. You have NO tools or API access.
Respond with the summary only — no preamble, no markdown fences…
```

History is in `messages` (can be tens of kB), not in system.

---

### 5b. CLI `/compact` (uncaptured)

**When:** user runs `/compact` in Claude Code CLI.

**Status:** no dedicated CLI `/compact` system body was pinned in the
2026-07-14 extract set. Operator recovery still uses the CLI path (see
[README §4](./README.md)). Do **not** size CLI compact prompts from §5a alone.

---

### 6. Completion auditor

**When:** long-running goal harness checks true completion.

**Sample pin:** `cc_version=2.1.111.402` · `gpt-5.6-sol` · ~1.8k · tools 0.

**Contract — single-line JSON only:**

```json
{"completed": boolean, "reason": string, "remaining": string[]}
```

---

### 7. Dual-persona external reviewer

**When:** orchestrator hands absolute paths + change-sets to `gpt56-reviewer`.

**Sample pin:** `cc_version=2.1.207.fcf` · `gpt-5.6-sol` · ~7k · tools 1 (`Read`).

Must emit **gpt56-zhuge** then **gpt56-elon**, then merged MUST-FIX /
Nice-to-have / Missing evidence. Hard tool budget is part of the contract.

---

## What not to do

- Ship full system dumps into git (raw-io retains secrets ~90d).  
- Treat model id as family identity.  
- Use SDK exec-summary text as if it were CLI `/compact`.  
- Compare CLI main vs SDK bot side-by-side as “same family.”
