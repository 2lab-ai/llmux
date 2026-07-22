# Evidence appendix — system prompt topology

**Captured:** 2026-07-14  
**Host logs:** `~/.local/state/llmux/{raw-io,activity,codex-trace}.jsonl`  
**Models in focus:** `grok-4.5`, `gpt-5.6-sol`, Claude assist paths  

This page is a **dated evidence appendix**. Operator decisions live in
[README.md](./README.md). Wire contracts live in [families.md](./families.md).

Full raw extracts are **not** committed (secrets). Re-extract with the command
at the bottom.

---

## 0. Frame

```text
Claude Code / Agent SDK  →  builds family (system+tools+messages)
                         →  llmux :3456  →  claude:* | codex:* | grok:*
```

Prompt **family** is chosen by harness role. Model is the executor.

---

## 1. Volume snapshot (`activity.jsonl`, this host, capture day)

| model | request count (approx) |
| --- | ---: |
| claude-opus-4-8 | 5091 |
| claude-fable-5 | 4346 |
| **gpt-5.6-sol** | **1863** |
| claude-opus-4-7 | 938 |
| claude-haiku-4-5-20251001 | 315 |
| gpt-5.5 | 188 |
| **grok-4.5** | **130** |

gpt-5.6-sol is a first-class production path (codex group) on this host.

---

## 2. Kind classification heuristic (reproducible)

Used when scanning raw-io request bodies:

| kind | rule |
| --- | --- |
| `agent_full` | `len(tools) ≥ 20` |
| `agent_sub` | `1 ≤ len(tools) < 20` |
| `monitor` | system contains `security monitor` / `HARD BLOCK` |
| `compact` | system or early user text matches exec-summary / compact cues |
| `completion_audit` | system contains `goal completion auditor` |
| `short` | tools=0 and system &lt; ~800 chars and not above |
| `other` | remainder |

### 2.1 Last 2.5GB raw-io (all models)

Denominator: **4501** request bodies with parseable JSON in that window.

| kind | count |
| --- | ---: |
| agent_full | 2025 |
| monitor | 1522 |
| agent_sub | 684 |
| compact | 35 |
| short | 80 |
| completion_audit | 11 |
| other | 52 |

### 2.2 gpt-5.6-sol subset (same window)

Denominator: **266** gpt-5.6-sol bodies; of which **163** `agent_full`.

| kind | count |
| --- | ---: |
| agent_full | 163 |
| agent_sub | 89 |
| completion_audit | 9 |
| compact | 2 |
| short | 3 |
| **monitor (106k family)** | **0 / 163 agent_full** |

Absence claim is **window-scoped** (one host, last 2.5GB). Follow-up Claude
models after gpt turns are common in activity; exact skip/delegate policy for
monitor on codex is **unresolved** (do not invent permission-mode causation).

---

## 3. Host-split samples (do not cross-compare as same family)

| Sample | model | host / entrypoint | family | sys chars | tools | sample `cc_version` |
| --- | --- | --- | --- | ---: | ---: | --- |
| CLI main | `grok-4.5` | `cli` | Main CLI agent | ~16.4k | 203 | `2.1.207.4d8` |
| SDK bot | `gpt-5.6-sol` | `sdk-ts` | SDK / Slack bot | ~13.4k | 88 | `2.1.111.0c3` |
| CLI main | `claude-haiku-4-5-…` | `cli` / `sdk-cli` | Main / SDK-cli hybrid | ~30.7k | 146 | `2.1.208.188` |
| Monitor | `grok-4.5` / `claude-opus-4-8` | `cli` | Security monitor | ~106k | 0 | `2.1.207.899` / `2.1.208.65f` |
| Exec-summary | `gpt-5.6-sol` | `sdk-ts` | SDK exec-summary | 873 | 0 | `2.1.111.0c3` |
| CLI subagent reviewer | `gpt-5.6-sol` | `cli` + subagent | Dual-persona reviewer | ~7k | 1 | `2.1.207.fcf` |

**Invalid comparison (fixed):** treating the gpt SDK bot row as “gpt main CLI
agent.” Pure CLI-main on gpt-5.6-sol was **not captured** in this window.

---

## 4. Usage metadata (both grok and gpt-5.6-sol)

### Present

```json
{
  "cache_creation_input_tokens": 0,
  "cache_read_input_tokens": 169472,
  "input_tokens": 24487,
  "output_tokens": 236
}
```

### Absent in scanned response bodies

`context_window`, remaining tokens, rate-limit remaining/reset, account quota.

SSE events seen: `message_start`, `content_block_*`, `message_delta`,
`message_stop`.

---

## 5. Multi-call sequences (observed patterns)

```text
# grok interactive
agent  grok-4.5     family=Main CLI
monitor grok|claude family=Security monitor
→ local tool execution

# gpt-5.6-sol long session
agent  gpt-5.6-sol  family=SDK bot | subagent | (CLI-main uncaptured)
assist claude-*     compact assist / other
agent  gpt-5.6-sol  continue
```

Operator recovery recipe for client mid-200k stop: [README §4](./README.md).

---

## 6. Residuals (open)

1. Exact policy: when does Claude Code skip the 106k monitor for codex/gpt?  
2. Pure CLI-main `gpt-5.6-sol` harness sample (non-subagent, non-sdk-ts).  
3. CLI `/compact` system body distinct from SDK exec-summary.  
4. Whether non-Messages grok API paths ever return non-Anthropic usage fields
   (out of scope for Claude Code proxy mode).

---

## Re-extract {#re-extract}

Scan a tail of raw-io and print family fingerprint + `cc_version` (no full dump):

```bash
python3 - <<'PY'
import json, os, re
path = os.path.expanduser("~/.local/state/llmux/raw-io.jsonl")
size = os.path.getsize(path)
start = max(0, size - 200_000_000)  # last ~200MB
want = os.environ.get("MODEL_SUBSTR", "")  # e.g. gpt-5.6-sol or grok-4.5

def sys_text(body):
    s = body.get("system")
    if isinstance(s, str): return s
    if isinstance(s, list):
        return "\n".join((b.get("text") or "") for b in s if isinstance(b, dict))
    return ""

def family(st, n_tools):
    low = st.lower()
    if "security monitor" in low: return "monitor"
    if "goal completion auditor" in low: return "completion_audit"
    if "executive summaries of engineering work" in low: return "sdk_exec_summary"
    if "cc_is_subagent=true" in st or "cc_is_subagent=true" in low: return "cli_subagent"
    if "you are claude code" in low and n_tools >= 20: return "cli_main"
    if "cc_entrypoint=sdk-ts" in st: return "sdk_bot"
    if n_tools >= 20: return "agent_full_other"
    if n_tools >= 1: return "agent_sub_other"
    return "other"

with open(path, "rb") as f:
    f.seek(start)
    if start: f.readline()
    for raw in f:
        try: o = json.loads(raw)
        except Exception: continue
        model = o.get("model") or ""
        if want and want not in model: continue
        try: body = json.loads(o.get("request_body") or "")
        except Exception: continue
        st = sys_text(body)
        n_tools = len(body.get("tools") or []) if isinstance(body.get("tools"), list) else 0
        ver = re.search(r"cc_version=([^;]+)", st)
        print(o.get("id"), model, family(st, n_tools), f"tools={n_tools}", f"sys={len(st)}",
              f"cc_version={ver.group(1) if ver else '?'}", st[:80].replace("\n", " "))
PY
```

Set `MODEL_SUBSTR=gpt-5.6-sol` (or `grok-4.5`) to filter. Re-run after Claude
Code upgrades when `cc_version` drifts.
