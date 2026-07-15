//! Message-kind classification for the activity feed (TUI UI-3 U1).
//!
//! One request body → a compact KIND label ("user" / "compact" / "security" /
//! …) plus a short EXCERPT of the human-relevant input, so the activity log
//! can say *what* each request was, not just that it happened. Classification
//! runs ONCE at forward entry on the already-buffered body (same pattern as
//! `routing::model_from_body` / `user_id_from_body`) and rides the
//! `RequestFinished` event; it never gates routing.
//!
//! The signatures are the wire fingerprints catalogued in
//! `docs/system-prompts/families.md` (captured from live `raw-io.jsonl`,
//! 2026-07-14/15): the harness *family* is identified by the system-prompt
//! contract, not the model id. Unknown shapes fall back to `user` (a plain
//! user turn) or `other` — the classifier must never fail the request.

/// Cap on the stored excerpt (chars). The collapsed activity row shows ~a
/// dozen chars; the click-expanded row shows one line as wide as the
/// terminal — 400 chars covers any realistic width without bloating the
/// activity channel / dashboard document.
pub const EXCERPT_MAX_CHARS: usize = 400;

/// Classified request kind + input excerpt. `kind` is a short stable token
/// (≤8 chars) rendered directly by the TUI; `excerpt` is the cleaned text of
/// the newest human-relevant input, `None` when the body carries none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub kind: &'static str,
    pub excerpt: Option<String>,
}

/// Classify one inbound request. `path` is the request path (for the
/// `count_tokens` endpoint class); `body` is the buffered JSON body.
/// Infallible: unparseable bodies classify as `other` with no excerpt.
pub fn classify(path: &str, body: &[u8]) -> Classified {
    let value: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let system = value.as_ref().map(system_text).unwrap_or_default();
    let last_user = value.as_ref().and_then(last_user_text).unwrap_or_default();
    let max_tokens = value
        .as_ref()
        .and_then(|v| v.get("max_tokens"))
        .and_then(|t| t.as_u64());
    let excerpt = clean_excerpt(&last_user);

    // `count_tokens` first: Claude Code fires these constantly as context
    // probes — the body looks like a user turn but the request is not one.
    let kind = if path.contains("count_tokens") {
        "count"
    } else {
        kind_from_signatures(&system, &last_user, max_tokens)
    };
    Classified { kind, excerpt }
}

/// The family fingerprints (docs/system-prompts/families.md), most specific
/// first. `system` = concatenated system text; `last_user` = the newest
/// user-role text block; `max_tokens` = the body's output cap (control
/// probes pin it to 1 — review MUST-FIX 1: the text alone must not reroute
/// a legitimate prompt that happens to say "quota").
fn kind_from_signatures(system: &str, last_user: &str, max_tokens: Option<u64>) -> &'static str {
    // Control families identified by the system contract.
    if system.contains("security monitor for autonomous AI coding agents") {
        return "security";
    }
    if system.contains("executive summaries of engineering work sessions") {
        return "summary";
    }
    if system.contains("goal completion auditor") {
        return "audit";
    }
    // Control turns identified by the instruction riding the user slot.
    if last_user.contains("create a detailed summary of the conversation")
        || system.contains("create a detailed summary of the conversation")
    {
        return "compact";
    }
    if last_user.contains("5-10 word title") {
        return "title";
    }
    if last_user.starts_with("[SUGGESTION MODE") {
        return "suggest";
    }
    if last_user.starts_with("Based on the conversation transcript above") {
        return "audit";
    }
    // Claude Code's per-session rate-limit status probe: a bare "quota" user
    // turn AND `max_tokens: 1` (both pinned from the raw-io capture,
    // 2026-07-15). Not an llmux probe (ours sends "." outside the forward
    // path). The kind changes routing (single-attempt, no park), so BOTH
    // signature halves are required — a real prompt that merely says
    // "quota" keeps full failover.
    if last_user.trim() == "quota" && max_tokens == Some(1) {
        return "quota";
    }
    // Claude Code's return-recap control turn, fired when a session resumes
    // ("The user stepped away and is coming back.") — harness scaffolding,
    // not typed input.
    if last_user.trim_start().starts_with("The user stepped away") {
        return "recap";
    }
    // Execution families: subagent / SDK-host / main CLI — all carry a real
    // input turn, so they read as flavored `user` kinds.
    if system.contains("cc_is_subagent=true") || system.contains("running within the Claude Agent")
    {
        return "subagent";
    }
    if system.contains("built on Anthropic's Claude Agent SDK") {
        return "sdk";
    }
    if !last_user.is_empty() {
        return "user";
    }
    "other"
}

/// Concatenate the system prompt text: a plain string, or the text of the
/// first few blocks of a block array (the billing header rides block 0, the
/// real contract usually block 1 — both are searched).
fn system_text(body: &serde_json::Value) -> String {
    match body.get("system") {
        Some(serde_json::Value::String(s)) => s.chars().take(4096).collect(),
        Some(serde_json::Value::Array(blocks)) => {
            let mut out = String::new();
            for block in blocks.iter().take(3) {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    out.extend(text.chars().take(2048));
                    out.push('\n');
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// The newest human-relevant input: walk `messages` backwards for the last
/// `role == "user"` entry, then take its best text — for block arrays, the
/// last text block that is NOT harness scaffolding (`<system-reminder>` hook
/// context, `<transcript>` payloads), falling back to any text block.
/// Tool-result-only turns yield an empty string.
fn last_user_text(body: &serde_json::Value) -> Option<String> {
    let messages = body.get("messages")?.as_array()?;
    let content = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))?
        .get("content")?;
    match content {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(blocks) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect();
            let human = texts
                .iter()
                .rev()
                .find(|t| {
                    let t = t.trim_start();
                    !t.starts_with("<system-reminder>") && !t.starts_with("<transcript>")
                })
                .copied();
            human.or_else(|| texts.last().copied()).map(str::to_string)
        }
        _ => None,
    }
}

/// Collapse whitespace runs to single spaces and cap at
/// [`EXCERPT_MAX_CHARS`] (char-boundary safe). Empty → `None`.
fn clean_excerpt(text: &str) -> Option<String> {
    let cleaned: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        return None;
    }
    Some(cleaned.chars().take(EXCERPT_MAX_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(system: &str, user: &str) -> Vec<u8> {
        serde_json::json!({
            "model": "claude-fable-5",
            "system": [
                {"type": "text", "text": "x-anthropic-billing-header: cc_version=2.1.209; cc_entrypoint=cli;"},
                {"type": "text", "text": system},
            ],
            "messages": [{"role": "user", "content": user}],
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn classifies_the_live_wire_families() {
        // Fingerprints exactly as captured in docs/system-prompts/families.md.
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (
                body(
                    "You are Claude Code, Anthropic's official CLI for Claude.",
                    "저기 있잖아 그거 고쳐줘",
                ),
                "user",
            ),
            (
                body(
                    "You are a security monitor for autonomous AI coding agents.",
                    "<transcript>\n...</transcript>",
                ),
                "security",
            ),
            (
                body(
                    "You are Claude Code, Anthropic's official CLI for Claude, running within the Claude Agent SDK.",
                    "Explore the repo",
                ),
                "subagent",
            ),
            (
                body(
                    "You are a Claude agent, built on Anthropic's Claude Agent SDK.",
                    "slack message",
                ),
                "sdk",
            ),
            (
                body(
                    "You generate executive summaries of engineering work sessions from conversation history.",
                    "history...",
                ),
                "summary",
            ),
            (
                body("You are a goal completion auditor.", "check"),
                "audit",
            ),
            (
                body(
                    "You are Claude Code, Anthropic's official CLI for Claude.",
                    "Your task is to create a detailed summary of the conversation so far.",
                ),
                "compact",
            ),
            (
                body(
                    "You are Claude Code, Anthropic's official CLI for Claude.",
                    "Please write a 5-10 word title for the following conversation:",
                ),
                "title",
            ),
            (
                body(
                    "You are Claude Code, Anthropic's official CLI for Claude.",
                    "[SUGGESTION MODE: Suggest what the user might naturally type next]",
                ),
                "suggest",
            ),
            (
                // Claude Code's per-session rate-limit probe (raw-io
                // 2026-07-15): bare "quota" turn + max_tokens 1.
                serde_json::json!({
                    "model": "claude-fable-5",
                    "max_tokens": 1,
                    "messages": [{"role": "user", "content": "quota"}],
                })
                .to_string()
                .into_bytes(),
                "quota",
            ),
            (
                // A REAL prompt that merely says "quota" (no max_tokens: 1
                // signature) must stay a plain user turn — the quota kind
                // changes routing (review MUST-FIX 1).
                body(
                    "You are Claude Code, Anthropic's official CLI for Claude.",
                    "quota",
                ),
                "user",
            ),
            (
                body(
                    "You are Claude Code, Anthropic's official CLI for Claude.",
                    "The user stepped away and is coming back.",
                ),
                "recap",
            ),
        ];
        for (b, want) in cases {
            assert_eq!(classify("/v1/messages", &b).kind, want);
        }
    }

    #[test]
    fn count_tokens_endpoint_wins_over_body_shape() {
        let b = body(
            "You are Claude Code, Anthropic's official CLI for Claude.",
            "hello",
        );
        assert_eq!(
            classify("/v1/messages/count_tokens?beta=true", &b).kind,
            "count"
        );
    }

    #[test]
    fn excerpt_prefers_human_text_over_system_reminder_blocks() {
        let b = serde_json::json!({
            "system": "You are Claude Code, Anthropic's official CLI for Claude.",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "고쳐줘   빨리\n\n제발"},
                {"type": "text", "text": "<system-reminder>hook noise</system-reminder>"},
            ]}],
        })
        .to_string()
        .into_bytes();
        let c = classify("/v1/messages", &b);
        assert_eq!(c.kind, "user");
        assert_eq!(c.excerpt.as_deref(), Some("고쳐줘 빨리 제발"));
    }

    #[test]
    fn tool_result_only_turn_has_no_excerpt_and_unparseable_is_other() {
        let b = serde_json::json!({
            "system": "You are Claude Code, Anthropic's official CLI for Claude.",
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "ok"},
            ]}],
        })
        .to_string()
        .into_bytes();
        let c = classify("/v1/messages", &b);
        assert_eq!(c.excerpt, None);
        assert_eq!(c.kind, "other");
        assert_eq!(classify("/v1/messages", b"not json").kind, "other");
    }

    #[test]
    fn excerpt_caps_at_char_boundary() {
        let long = "가".repeat(EXCERPT_MAX_CHARS + 50);
        let b = body(
            "You are Claude Code, Anthropic's official CLI for Claude.",
            &long,
        );
        let c = classify("/v1/messages", &b);
        assert_eq!(c.excerpt.unwrap().chars().count(), EXCERPT_MAX_CHARS);
    }
}
