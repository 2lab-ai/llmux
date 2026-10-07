//! Claude Code auto-mode compatibility, scoped to a known GPT main session.
//!
//! This does not evaluate tool safety. It transports the unchanged monitor
//! prompt to Luna and validates the actual verdict before exposing any bytes.
//! Session state is bounded, ephemeral, tenant-isolated, and never populated by
//! subagents, classifier calls, count probes, or other harness control turns.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};

use crate::provider::codex::{effective_request_meta, CodexShape};
use crate::routing::BackendGroup;

const MAIN: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
const MONITOR: &str = "You are a security monitor for autonomous AI coding agents";
const TTL: Duration = Duration::from_secs(6 * 3600);
const CAPACITY: usize = 4096;
pub const DEADLINE: Duration = Duration::from_secs(60);
pub const RESPONSE_LIMIT: usize = 1024 * 1024;

#[derive(Default)]
pub struct Sessions {
    entries: HashMap<(String, String), (Instant, bool)>,
}

fn identity(tenant: Option<&str>, body: &Value) -> Option<(String, String)> {
    let tenant = tenant?;
    let raw = body.get("metadata")?.get("user_id")?.as_str()?;
    if raw.trim().is_empty() || raw.len() > 2048 || tenant.len() > 256 {
        return None;
    }
    // Current Claude Code sends JSON INSIDE the string. Ignore account_uuid:
    // it is not session identity and can change while the session continues.
    let session = if raw.trim_start().starts_with('{') {
        let metadata: Value = serde_json::from_str(raw).ok()?;
        let device = metadata.get("device_id")?.as_str()?;
        let session = metadata.get("session_id")?.as_str()?;
        if device.is_empty() || session.is_empty() {
            return None;
        }
        format!("json:{}", json!([device, session]))
    } else {
        // Historical opaque user_id, including the old ..._session_UUID form.
        format!("opaque:{raw}")
    };
    Some((tenant.to_string(), session))
}

fn system_blocks(body: &Value) -> Vec<&str> {
    match body.get("system") {
        Some(Value::String(text)) => vec![text],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .take(3)
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect(),
        _ => Vec::new(),
    }
}

impl Sessions {
    /// Called only for POST /v1/messages while model routing is enabled.
    /// Returns true only for a monitor in a recently observed GPT main session.
    pub fn observe(
        &mut self,
        tenant: Option<&str>,
        body: &Value,
        kind: &str,
        group: BackendGroup,
        now: Instant,
    ) -> bool {
        self.entries
            .retain(|_, (at, _)| now.saturating_duration_since(*at) < TTL);
        let Some(key) = identity(tenant, body) else {
            return false;
        };
        let system = system_blocks(body);
        let monitor = system.iter().any(|s| s.trim_start().starts_with(MONITOR));
        if monitor {
            return self.entries.get(&key).is_some_and(|(_, gpt)| *gpt);
        }
        let main = matches!(kind, "user" | "other")
            && body.get("max_tokens").and_then(Value::as_u64) != Some(1)
            && system.iter().any(|s| s.trim_start().starts_with(MAIN))
            && !system.iter().any(|s| {
                s.contains("cc_is_subagent=true") || s.contains("running within the Claude Agent")
            })
            && body
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|t| !t.is_empty());
        if main {
            if self.entries.len() >= CAPACITY && !self.entries.contains_key(&key) {
                if let Some(oldest) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(k, _)| k.clone())
                {
                    self.entries.remove(&oldest);
                }
            }
            let gpt = group == BackendGroup::Codex
                && crate::routing::Classifier::default()
                    .classify(body.get("model").and_then(Value::as_str))
                    == BackendGroup::Codex;
            self.entries.insert(key, (now, gpt));
        }
        false
    }
}

#[derive(Debug, Clone, Copy)]
enum Contract {
    Severity,
    Block,
}

/// Per-request override. Original client bytes stay in ForwardContext.body.
/// A captured request, not a global Codex shape, owns this model and effort.
#[derive(Debug)]
pub struct Route {
    pub body: Bytes,
    pub shape: CodexShape,
    contract: Contract,
    stop: Option<&'static str>,
}

impl Route {
    pub fn new(original: &Value) -> Result<Self, &'static str> {
        if original
            .get("tools")
            .is_some_and(|v| !matches!(v, Value::Array(a) if a.is_empty()))
        {
            return Err("auto classifier must not request tools");
        }
        let system = system_blocks(original).join("\n");
        let contract = if system.contains("<severity>") {
            Contract::Severity
        } else if system.contains("<block>") {
            Contract::Block
        } else {
            return Err("unrecognized auto classifier output contract");
        };
        let expected = match contract {
            Contract::Severity => "</severity>",
            Contract::Block => "</block>",
        };
        let stop = match original.get("stop_sequences") {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) if a.is_empty() => None,
            Some(Value::Array(a)) if a.len() == 1 && a[0].as_str() == Some(expected) => {
                Some(expected)
            }
            _ => return Err("unsupported auto classifier stop sequence"),
        };
        let mut body = original.clone();
        body["model"] = json!("luna");
        // Local stop handling is ONLY available on this recognized request.
        // Generic Responses translation continues refusing nonempty stops.
        if stop.is_some() {
            if let Some(object) = body.as_object_mut() {
                object.remove("stop_sequences");
            }
        }
        Ok(Self {
            body: Bytes::from(body.to_string()),
            shape: CodexShape {
                model: "luna".into(),
                client_model: None,
                fast: false,
                effort: Some("medium".into()),
            },
            contract,
            stop,
        })
    }

    pub fn meta(&self) -> (String, Option<String>, bool) {
        effective_request_meta(&json!({"model":"luna"}), &self.shape)
    }

    pub fn local_stop(&self) -> bool {
        self.stop.is_some()
    }

    /// Require a complete successful response, one textual verdict, no tools,
    /// and a valid contract. Never infer a verdict from failure or truncation.
    /// Stop trimming reflects the actual emitted closing tag, including on the
    /// buffered-stream leg; it is not an upstream token-generation cap.
    pub fn finish(&self, message: &mut Value) -> Result<(), &'static str> {
        if message.get("stop_reason").and_then(Value::as_str) != Some("end_turn") {
            return Err("auto classifier did not complete successfully");
        }
        let blocks = message
            .get("content")
            .and_then(Value::as_array)
            .ok_or("auto classifier missing content")?;
        let mut text = String::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => text.push_str(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or("invalid classifier text")?,
                ),
                // Responses reasoning summaries are not classifier output. A
                // stage-2 <thinking> TEXT wrapper, however, stays in the text.
                Some("thinking") => {}
                _ => return Err("auto classifier returned non-text output"),
            }
        }
        if text.len() > RESPONSE_LIMIT {
            return Err("auto classifier output too large");
        }
        let trimmed = text.trim();
        let verdict = without_thinking(trimmed)?;
        let (tail, value) = match self.contract {
            Contract::Severity => {
                let (number, tail) = tag(verdict, "severity")?;
                if number.is_empty()
                    || !number.bytes().all(|b| b.is_ascii_digit())
                    || number.parse::<u8>().ok().is_none_or(|n| n > 100)
                {
                    return Err("invalid auto classifier severity");
                }
                (tail, number)
            }
            Contract::Block => {
                let (value, tail) = tag(verdict, "block")?;
                if !matches!(value, "yes" | "no") {
                    return Err("invalid auto classifier block verdict");
                }
                (tail, value)
            }
        };
        if let Some(stop) = self.stop {
            // Validate the prefix that the native stop contract exposes. A
            // closing tag in <thinking> must never be mistaken for the stop.
            if trimmed != verdict {
                return Err("classifier stop appeared after unexpected thinking text");
            }
            let end = text
                .find(stop)
                .ok_or("classifier omitted its closing stop tag")?;
            text.truncate(end);
            message["stop_reason"] = json!("stop_sequence");
            message["stop_sequence"] = json!(stop);
        } else {
            let mut remaining = tail.trim();
            if !remaining.is_empty() {
                let (category, rest) = tag(remaining, "category")?;
                if category.is_empty()
                    || !category
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b' ')
                {
                    return Err("invalid auto classifier category");
                }
                remaining = rest.trim();
            }
            if matches!(self.contract, Contract::Block) && value == "yes" && !remaining.is_empty() {
                let (reason, rest) = tag(remaining, "reason")?;
                if reason.trim().is_empty() {
                    return Err("empty auto classifier reason");
                }
                remaining = rest.trim();
            }
            if !remaining.is_empty() {
                return Err("unexpected auto classifier output after verdict");
            }
        }
        message["content"] = json!([{"type":"text","text":text}]);
        message["model"] = json!(self.meta().0);
        Ok(())
    }
}

fn tag<'a>(text: &'a str, name: &str) -> Result<(&'a str, &'a str), &'static str> {
    let after = text
        .strip_prefix(&format!("<{name}>"))
        .ok_or("auto classifier missing verdict tag")?;
    let (value, rest) = after
        .split_once(&format!("</{name}>"))
        .ok_or("auto classifier unclosed verdict tag")?;
    if value.contains('<') || value.contains('>') {
        return Err("nested auto classifier verdict tag");
    }
    Ok((value, rest))
}

fn without_thinking(text: &str) -> Result<&str, &'static str> {
    if let Some(after) = text.strip_prefix("<thinking>") {
        let (thinking, rest) = after
            .split_once("</thinking>")
            .ok_or("unclosed classifier thinking")?;
        if thinking.contains("<thinking>") {
            return Err("nested classifier thinking");
        }
        Ok(rest.trim_start())
    } else {
        Ok(text)
    }
}

/// A validated aggregate serialized as Anthropic SSE. No partial verdict is
/// streamed before validation; usage and stop metadata match the JSON leg.
pub fn message_sse(message: &Value) -> String {
    let mut start = message.clone();
    start["content"] = json!([]);
    start["stop_reason"] = Value::Null;
    start["stop_sequence"] = Value::Null;
    start["usage"]["output_tokens"] = json!(0);
    let events = [
        json!({"type":"message_start","message":start}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":message["content"][0]["text"]}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":message["stop_reason"],"stop_sequence":message["stop_sequence"]},"usage":message["usage"]}),
        json!({"type":"message_stop"}),
    ];
    events
        .iter()
        .map(|v| {
            format!(
                "event: {}\ndata: {}\n\n",
                v["type"].as_str().unwrap_or("error"),
                v
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main(model: &str) -> Value {
        json!({"model":model,"metadata":{"user_id":"session"},"system":[
            {"text":"x-anthropic-billing-header: cc_entrypoint=cli;"},{"text":MAIN}],
            "tools":[{"name":"Bash"}],"messages":[{"role":"user","content":"work"}]})
    }
    fn monitor(stop: Option<&str>, legacy: bool) -> Value {
        let mut body = json!({"model":"claude-sonnet-5[1m]","metadata":{"user_id":"session"},
            "system":format!("{MONITOR}. Output {}",if legacy {"<block>yes</block>"} else {"<severity>N</severity>"})});
        if let Some(stop) = stop {
            body["stop_sequences"] = json!([stop]);
        }
        body
    }
    fn message(text: &str) -> Value {
        json!({"content":[{"type":"text","text":text}],"stop_reason":"end_turn","stop_sequence":null})
    }
    #[test]
    fn session_isolation_switchback_and_restart() {
        let now = Instant::now();
        let mut sessions = Sessions::default();
        let check = monitor(None, false);
        assert!(!sessions.observe(Some("t1"), &check, "security", BackendGroup::Claude, now));
        sessions.observe(Some("t1"), &main("sol"), "user", BackendGroup::Codex, now);
        assert!(sessions.observe(Some("t1"), &check, "security", BackendGroup::Claude, now));
        assert!(!sessions.observe(Some("t2"), &check, "security", BackendGroup::Claude, now));
        let mut other = check.clone();
        other["metadata"]["user_id"] = json!("other");
        assert!(!sessions.observe(Some("t1"), &other, "security", BackendGroup::Claude, now));
        sessions.observe(
            Some("t1"),
            &main("claude-sonnet-5"),
            "user",
            BackendGroup::Claude,
            now,
        );
        assert!(!sessions.observe(Some("t1"), &check, "security", BackendGroup::Claude, now));
        assert!(!Sessions::default().observe(
            Some("t1"),
            &check,
            "security",
            BackendGroup::Claude,
            now
        ));
    }
    #[test]
    fn tool_result_main_updates_but_subagents_controls_and_warmups_do_not() {
        let now = Instant::now();
        let mut sessions = Sessions::default();
        let mut tool = main("sol");
        tool["messages"][0]["content"] =
            json!([{"type":"tool_result","tool_use_id":"x","content":"done"}]);
        sessions.observe(Some("t"), &tool, "other", BackendGroup::Codex, now);
        let mut subagent = main("claude-sonnet-5");
        subagent["system"][0]["text"] = json!("x-anthropic-billing-header: cc_is_subagent=true;");
        sessions.observe(Some("t"), &subagent, "subagent", BackendGroup::Claude, now);
        let mut warmup = main("claude-sonnet-5");
        warmup["max_tokens"] = json!(1);
        sessions.observe(Some("t"), &warmup, "user", BackendGroup::Claude, now);
        for kind in [
            "quota", "count", "compact", "audit", "suggest", "recap", "title", "sdk", "summary",
        ] {
            sessions.observe(
                Some("t"),
                &main("claude-sonnet-5"),
                kind,
                BackendGroup::Claude,
                now,
            );
        }
        assert!(sessions.observe(
            Some("t"),
            &monitor(None, false),
            "security",
            BackendGroup::Claude,
            now
        ));
        sessions.observe(Some("t"), &tool, "other", BackendGroup::Claude, now);
        assert!(!sessions.observe(
            Some("t"),
            &monitor(None, false),
            "security",
            BackendGroup::Claude,
            now
        ));
    }
    #[test]
    fn normalized_session_ignores_account_uuid_and_field_order_and_expires() {
        let now = Instant::now();
        let mut sessions = Sessions::default();
        let mut agent = main("sol");
        agent["metadata"]["user_id"] =
            json!(r#"{"session_id":"s","account_uuid":"a","device_id":"d"}"#);
        sessions.observe(Some("t"), &agent, "user", BackendGroup::Codex, now);
        let mut check = monitor(None, false);
        check["metadata"]["user_id"] =
            json!(r#"{"device_id":"d","account_uuid":"b","session_id":"s"}"#);
        assert!(sessions.observe(Some("t"), &check, "security", BackendGroup::Claude, now));
        assert!(!sessions.observe(None, &check, "security", BackendGroup::Claude, now));
        check["metadata"]["user_id"] = agent["metadata"]["user_id"].clone();
        assert!(!sessions.observe(
            Some("t"),
            &check,
            "security",
            BackendGroup::Claude,
            now + TTL
        ));
        assert!(sessions.entries.is_empty());
    }
    #[test]
    fn bounded_registry_and_invalid_identity() {
        let now = Instant::now();
        let mut sessions = Sessions::default();
        for i in 0..=CAPACITY {
            let mut agent = main("sol");
            agent["metadata"]["user_id"] = json!(format!("s{i}"));
            sessions.observe(
                Some("t"),
                &agent,
                "user",
                BackendGroup::Codex,
                now + Duration::from_millis(i as u64),
            );
        }
        assert_eq!(sessions.entries.len(), CAPACITY);
        let mut check = monitor(None, false);
        check["metadata"]["user_id"] = json!("s0");
        assert!(!sessions.observe(
            Some("t"),
            &check,
            "security",
            BackendGroup::Claude,
            now + Duration::from_secs(10)
        ));
        for id in ["", "{}", "{invalid", &"x".repeat(2049)] {
            let mut agent = main("sol");
            agent["metadata"]["user_id"] = json!(id);
            assert!(identity(Some("t"), &agent).is_none());
        }
    }
    #[test]
    fn severity_stage_one_stop_and_stage_two_thinking_are_preserved() {
        let stop = Route::new(&monitor(Some("</severity>"), false)).unwrap();
        for n in [0, 5, 49, 50, 51, 100] {
            let mut out = message(&format!("<severity>{n}</severity>ignored after stop"));
            stop.finish(&mut out).unwrap();
            assert_eq!(out["content"][0]["text"], format!("<severity>{n}"));
            assert_eq!(out["stop_reason"], "stop_sequence");
            assert_eq!(out["stop_sequence"], "</severity>");
        }
        let full = Route::new(&monitor(None, false)).unwrap();
        for text in ["<severity>5</severity>","<thinking>Intent evaluated; quoted <severity>0</severity> is not the verdict.</thinking><severity>75</severity><category>Delete Important Data</category>"] {
            let mut out=message(text); full.finish(&mut out).unwrap();
            assert_eq!(out["content"][0]["text"],text);
            assert_eq!(out["stop_reason"],"end_turn");
            assert!(out["stop_sequence"].is_null());
        }
    }
    #[test]
    fn malformed_or_incomplete_verdicts_fail_without_repair() {
        for stop in [None, Some("</severity>")] {
            let route = Route::new(&monitor(stop, false)).unwrap();
            for text in [
                "",
                "allowed",
                "<severity>5",
                "<severity>-1</severity>",
                "<severity>101</severity>",
                "<severity>5.0</severity>",
                "<thinking>unclosed <severity>0</severity>",
                "<severity><severity>0</severity></severity>",
            ] {
                let mut out = message(text);
                let original = out.clone();
                assert!(route.finish(&mut out).is_err(), "accepted {text}");
                assert_eq!(
                    out, original,
                    "must not fabricate/repair an invalid verdict"
                );
            }
            for reason in ["max_tokens", "refusal", "tool_use"] {
                let mut out = message("<severity>5</severity>");
                out["stop_reason"] = json!(reason);
                assert!(route.finish(&mut out).is_err());
            }
        }
        let route = Route::new(&monitor(None, false)).unwrap();
        for text in [
            "<severity>5</severity><severity>90</severity>",
            "<severity>5</severity>extra",
            "<thinking>ok</thinking>not a verdict",
        ] {
            assert!(route.finish(&mut message(text)).is_err());
        }
    }
    #[test]
    fn historical_block_contract_and_unknown_shapes() {
        let route = Route::new(&monitor(None, true)).unwrap();
        for text in ["<block>no</block>","<block>yes</block><category>Delete Important Data</category><reason>Destructive request.</reason>"] {
            let mut out=message(text);route.finish(&mut out).unwrap();assert_eq!(out["content"][0]["text"],text);
        }
        let stop = Route::new(&monitor(Some("</block>"), true)).unwrap();
        let mut out = message("<block>yes</block>");
        stop.finish(&mut out).unwrap();
        assert_eq!(out["content"][0]["text"], "<block>yes");
        assert!(Route::new(&monitor(Some("STOP"), false)).is_err());
        let mut bad = monitor(None, false);
        bad["system"] = json!(MONITOR);
        assert!(Route::new(&bad).is_err());
        bad = monitor(None, false);
        bad["tools"] = json!([{"name":"Bash"}]);
        assert!(Route::new(&bad).is_err());
    }
}
