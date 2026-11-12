//! Claude Code harness-request compatibility, scoped to a session whose main
//! model is served by Codex or Grok (`.prd/27-claude-code-harness-requests.md`).
//!
//! This does not evaluate tool safety. It transports the unchanged monitor
//! prompt to the main provider's monitor model and validates the actual verdict
//! before exposing any bytes. Session state is bounded, ephemeral,
//! tenant-isolated, and learned only from main turns and the session's own
//! warmup — never from subagents, classifier calls, count probes, or other
//! harness control turns.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};

use crate::provider::codex::{effective_request_meta, CodexShape};
use crate::provider::grok::GrokShape;
use crate::routing::BackendGroup;

pub(crate) const MAIN: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
/// Claude Code's identity sentence when it runs inside the Claude Agent SDK.
pub(crate) const MAIN_IN_SDK: &str =
    "You are Claude Code, Anthropic's official CLI for Claude, running within the Claude Agent SDK.";
/// A Claude Agent SDK host's identity sentence.
pub(crate) const SDK_AGENT: &str = "You are a Claude agent, built on Anthropic's Claude Agent SDK.";
const MONITOR: &str = "You are a security monitor for autonomous AI coding agents";
const TTL: Duration = Duration::from_secs(6 * 3600);
const CAPACITY: usize = 4096;
pub const DEADLINE: Duration = Duration::from_secs(60);
/// How long a session title of an unknown session waits for that session's
/// first main turn. Live 2026-11-12 (CLI 2.1.322): the title starts 70–350 ms
/// before the main turn, also after `/clear`, which sends no warmup.
pub const FIRST_MAIN_WAIT: Duration = Duration::from_secs(3);
pub const RESPONSE_LIMIT: usize = 1024 * 1024;

/// Per tenant/session: when the entry was written and the provider that serves
/// the session's main model (`Claude` when it is not a routed provider).
#[derive(Default)]
pub struct Sessions {
    entries: HashMap<(String, String), (Instant, BackendGroup)>,
}

/// The providers whose main sessions get harness requests rerouted.
pub fn is_routed(group: BackendGroup) -> bool {
    matches!(group, BackendGroup::Codex | BackendGroup::Grok)
}

/// The provider a main model belongs to. A custom routing rule that sends a
/// Claude-named model elsewhere does not make the session a Codex/Grok one:
/// both the live group and the default classification must agree.
fn main_provider(group: BackendGroup, model: Option<&str>) -> BackendGroup {
    if is_routed(group) && crate::routing::Classifier::default().classify(model) == group {
        group
    } else {
        BackendGroup::Claude
    }
}

fn is_main_identity(system: &[&str]) -> bool {
    system.iter().any(|s| s.trim_start().starts_with(MAIN))
        && !system.iter().any(|s| {
            s.contains("cc_is_subagent=true") || s.contains("running within the Claude Agent")
        })
}

fn has_tools(body: &Value) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|t| !t.is_empty())
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

pub(super) fn system_blocks(body: &Value) -> Vec<&str> {
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
    /// The routed provider (Codex or Grok) of this request's session, if known.
    /// Utility lookups never seed state. A bounded bootstrap hint is used only
    /// for an exact startup quota/title request, never over an observed entry.
    pub fn main_group(
        &self,
        tenant: Option<&str>,
        body: &Value,
        hint: Option<&str>,
    ) -> Option<BackendGroup> {
        if let Some(key) = identity(tenant, body) {
            if let Some((_, group)) = self.entries.get(&key) {
                return is_routed(*group).then_some(*group);
            }
        }
        let model = hint.filter(|model| model.len() <= 128)?;
        let model = model.strip_suffix("[1m]").unwrap_or(model);
        crate::catalog::catalog("", "", "")
            .iter()
            .find(|m| m.id == model || m.aliases.iter().any(|a| a == model))
            .map(|m| BackendGroup::from_kind(m.group))
            .filter(|group| is_routed(*group))
    }

    /// True when the request names a Claude Code CLI session (tenant +
    /// `metadata.user_id`, the CLI identity block) that no main turn or warmup
    /// has recorded yet. Agent SDK hosts never record a main turn (live
    /// 2026-11-12: 372 of 378 titles carried the SDK identity), so their
    /// requests never wait for one.
    pub fn awaiting_first_main(&self, tenant: Option<&str>, body: &Value) -> bool {
        is_main_identity(&system_blocks(body))
            && identity(tenant, body).is_some_and(|key| !self.entries.contains_key(&key))
    }

    fn insert(&mut self, key: (String, String), now: Instant, group: BackendGroup) {
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
        self.entries.insert(key, (now, group));
    }

    /// Called only for POST /v1/messages while model routing is enabled.
    /// Returns the session's routed provider only for a monitor request.
    ///
    /// A main turn always records the session's provider. A warmup — the
    /// main identity, `max_tokens: 1`, no tools, sent with the session's main
    /// model when a session starts — seeds an UNKNOWN session only: the
    /// title can arrive before the first main turn (live 2026-11-12: 255 ms).
    pub fn observe(
        &mut self,
        tenant: Option<&str>,
        body: &Value,
        _kind: &str,
        group: BackendGroup,
        now: Instant,
    ) -> Option<BackendGroup> {
        self.entries
            .retain(|_, (at, _)| now.saturating_duration_since(*at) < TTL);
        let key = identity(tenant, body)?;
        let system = system_blocks(body);
        let monitor = system.iter().any(|s| s.trim_start().starts_with(MONITOR));
        if monitor {
            return self
                .entries
                .get(&key)
                .map(|(_, group)| *group)
                .filter(|group| is_routed(*group));
        }
        let warmup = body.get("max_tokens").and_then(Value::as_u64) == Some(1);
        if !is_main_identity(&system) {
            return None;
        }
        let main_turn = !warmup && has_tools(body);
        let seed = warmup && !has_tools(body) && !self.entries.contains_key(&key);
        if main_turn || seed {
            let provider = main_provider(group, body.get("model").and_then(Value::as_str));
            self.insert(key, now, provider);
        }
        None
    }
}

/// A routed harness request's provider and its request-owned shape. The
/// shape names an alias (`terra`, `luna`, the Grok pin) that resolves on every
/// request, so a catalog or pin update moves the request with no config edit.
#[derive(Debug, Clone)]
pub enum Target {
    Codex(CodexShape),
    Grok(GrokShape),
}

impl Target {
    pub fn codex(model: &str, effort: Option<String>) -> Self {
        Self::Codex(CodexShape {
            model: model.into(),
            client_model: None,
            fast: false,
            effort,
        })
    }

    /// `pin` is the Grok provider's configured model: the body names the bare
    /// `grok` alias, which the provider resolves to this pin.
    pub fn grok(pin: &str, effort: Option<String>) -> Self {
        Self::Grok(GrokShape {
            model: pin.into(),
            client_model: None,
            effort,
        })
    }

    pub fn group(&self) -> BackendGroup {
        match self {
            Self::Codex(_) => BackendGroup::Codex,
            Self::Grok(_) => BackendGroup::Grok,
        }
    }

    /// The model the routed body names: the Codex alias, or bare `grok`.
    pub fn body_model(&self) -> &str {
        match self {
            Self::Codex(shape) => &shape.model,
            Self::Grok(_) => "grok",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Contract {
    Severity,
    Block,
    Utility,
}

/// Per-request override. Original client bytes stay in ForwardContext.body.
/// A captured request, not a global provider shape, owns this model and effort.
#[derive(Debug)]
pub struct Route {
    pub body: Bytes,
    pub target: Target,
    contract: Contract,
    stop: Option<&'static str>,
    pub title_format: Option<Value>,
    pub omitted_temperature: bool,
}

impl Route {
    pub fn new(original: &Value) -> Result<Self, &'static str> {
        Self::monitor(original, Target::codex("luna", Some("medium".into())))
    }

    pub fn monitor(original: &Value, target: Target) -> Result<Self, &'static str> {
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
            Contract::Utility => return Err("utility has no classifier stop"),
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
        body["model"] = json!(target.body_model());
        // Local stop handling is ONLY available on this recognized request.
        // Generic Responses translation continues refusing nonempty stops.
        if stop.is_some() {
            if let Some(object) = body.as_object_mut() {
                object.remove("stop_sequences");
            }
        }
        Ok(Self {
            body: Bytes::from(body.to_string()),
            target,
            contract,
            stop,
            title_format: None,
            omitted_temperature: false,
        })
    }

    pub fn utility(
        mut body: Value,
        target: Target,
        title_format: Option<Value>,
        omitted_temperature: bool,
    ) -> Self {
        body["model"] = json!(target.body_model());
        Self {
            body: Bytes::from(body.to_string()),
            target,
            contract: Contract::Utility,
            stop: None,
            title_format,
            omitted_temperature,
        }
    }

    pub fn is_monitor(&self) -> bool {
        !matches!(self.contract, Contract::Utility)
    }

    pub fn group(&self) -> BackendGroup {
        self.target.group()
    }

    /// `(upstream model, effort, fast)` this route sends — the activity row,
    /// usage booking and the model stamped into the client response.
    pub fn meta(&self) -> (String, Option<String>, bool) {
        let body = serde_json::from_slice(&self.body).unwrap_or(Value::Null);
        match &self.target {
            Target::Codex(shape) => effective_request_meta(&body, shape),
            Target::Grok(shape) => {
                let (model, effort) = crate::provider::grok::effective_request_meta(&body, shape);
                (model, effort, false)
            }
        }
    }

    pub fn local_stop(&self) -> bool {
        self.stop.is_some()
    }

    /// Require a complete successful response, one textual verdict, no tools,
    /// and a valid contract. Never infer a verdict from failure or truncation.
    /// Stop trimming reflects the actual emitted closing tag, including on the
    /// buffered-stream leg; it is not an upstream token-generation cap.
    pub fn finish(&self, message: &mut Value) -> Result<(), &'static str> {
        if !self.is_monitor() {
            message["model"] = json!(self.meta().0);
            return Ok(());
        }
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
            Contract::Utility => return Ok(()),
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
        assert!(sessions
            .observe(Some("t1"), &check, "security", BackendGroup::Claude, now)
            .is_none());
        sessions.observe(Some("t1"), &main("sol"), "user", BackendGroup::Codex, now);
        assert!(sessions
            .observe(Some("t1"), &check, "security", BackendGroup::Claude, now)
            .is_some());
        assert!(sessions
            .observe(Some("t2"), &check, "security", BackendGroup::Claude, now)
            .is_none());
        let mut other = check.clone();
        other["metadata"]["user_id"] = json!("other");
        assert!(sessions
            .observe(Some("t1"), &other, "security", BackendGroup::Claude, now)
            .is_none());
        sessions.observe(
            Some("t1"),
            &main("claude-sonnet-5"),
            "user",
            BackendGroup::Claude,
            now,
        );
        assert!(sessions
            .observe(Some("t1"), &check, "security", BackendGroup::Claude, now)
            .is_none());
        assert!(Sessions::default()
            .observe(Some("t1"), &check, "security", BackendGroup::Claude, now)
            .is_none());
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
            let mut control = main("claude-sonnet-5");
            control["tools"] = json!([]);
            sessions.observe(Some("t"), &control, kind, BackendGroup::Claude, now);
        }
        assert!(sessions
            .observe(
                Some("t"),
                &monitor(None, false),
                "security",
                BackendGroup::Claude,
                now
            )
            .is_some());
        sessions.observe(Some("t"), &tool, "other", BackendGroup::Claude, now);
        assert!(sessions
            .observe(
                Some("t"),
                &monitor(None, false),
                "security",
                BackendGroup::Claude,
                now
            )
            .is_none());
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
        assert!(sessions
            .observe(Some("t"), &check, "security", BackendGroup::Claude, now)
            .is_some());
        assert!(sessions
            .observe(None, &check, "security", BackendGroup::Claude, now)
            .is_none());
        check["metadata"]["user_id"] = agent["metadata"]["user_id"].clone();
        assert!(sessions
            .observe(
                Some("t"),
                &check,
                "security",
                BackendGroup::Claude,
                now + TTL
            )
            .is_none());
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
        assert!(sessions
            .observe(
                Some("t"),
                &check,
                "security",
                BackendGroup::Claude,
                now + Duration::from_secs(10)
            )
            .is_none());
        for id in ["", "{}", "{invalid", &"x".repeat(2049)] {
            let mut agent = main("sol");
            agent["metadata"]["user_id"] = json!(id);
            assert!(identity(Some("t"), &agent).is_none());
        }
    }
    fn warmup(model: &str) -> Value {
        json!({"model":model,"max_tokens":1,"metadata":{"user_id":"session"},"system":[
            {"text":"x-anthropic-billing-header: cc_entrypoint=cli;"},{"text":MAIN}],
            "messages":[{"role":"user","content":[{"type":"text","text":"Hi"}]}]})
    }
    #[test]
    fn warmup_seeds_only_an_unknown_session_and_a_main_turn_wins() {
        let now = Instant::now();
        let mut sessions = Sessions::default();
        let title = json!({"model":"claude-haiku-5-5","metadata":{"user_id":"session"}});
        // Live 2026-11-12: the title starts 255 ms before the first main turn.
        sessions.observe(
            Some("t"),
            &warmup("astra"),
            "user",
            BackendGroup::Codex,
            now,
        );
        assert_eq!(
            sessions.main_group(Some("t"), &title, None),
            Some(BackendGroup::Codex)
        );
        // A later warmup never overrides; a main turn always does.
        sessions.observe(
            Some("t"),
            &warmup("claude-opus-5-5"),
            "user",
            BackendGroup::Claude,
            now,
        );
        assert_eq!(
            sessions.main_group(Some("t"), &title, None),
            Some(BackendGroup::Codex)
        );
        sessions.observe(
            Some("t"),
            &main("claude-opus-5-5"),
            "user",
            BackendGroup::Claude,
            now,
        );
        assert_eq!(sessions.main_group(Some("t"), &title, None), None);
        // A Claude warmup still beats a stale GPT launch hint for its session.
        let mut fresh = Sessions::default();
        fresh.observe(
            Some("t"),
            &warmup("claude-opus-5-5"),
            "user",
            BackendGroup::Claude,
            now,
        );
        assert_eq!(fresh.main_group(Some("t"), &title, Some("sol")), None);
        // A warmup carrying tools, or from a subagent, is not a warmup.
        let mut other = Sessions::default();
        let mut tooled = warmup("astra");
        tooled["tools"] = json!([{"name":"Bash"}]);
        other.observe(Some("t"), &tooled, "user", BackendGroup::Codex, now);
        let mut subagent = warmup("astra");
        subagent["system"][0]["text"] = json!("x-anthropic-billing-header: cc_is_subagent=true;");
        other.observe(Some("t"), &subagent, "user", BackendGroup::Codex, now);
        assert!(other.entries.is_empty());
    }
    #[test]
    fn a_session_awaits_its_first_main_turn_until_one_is_recorded() {
        let now = Instant::now();
        let mut sessions = Sessions::default();
        let title = json!({"model":"claude-haiku-5-5","metadata":{"user_id":"session"},
            "system":[{"text":"x-anthropic-billing-header: cc_entrypoint=cli;"},{"text":MAIN},
                {"text":"You are naming a coding session…"}]});
        assert!(sessions.awaiting_first_main(Some("t"), &title));
        assert!(
            !sessions.awaiting_first_main(None, &title),
            "no identity: nothing to wait for"
        );
        // An Agent SDK host's title: its main turns are never recorded.
        let mut sdk = title.clone();
        sdk["system"][1]["text"] = json!(SDK_AGENT);
        assert!(!sessions.awaiting_first_main(Some("t"), &sdk));
        sessions.observe(
            Some("t"),
            &main("claude-opus-5-5"),
            "user",
            BackendGroup::Claude,
            now,
        );
        assert!(!sessions.awaiting_first_main(Some("t"), &title));
    }
    #[test]
    fn grok_main_session_routes_its_monitor_and_hints_resolve_by_catalog_group() {
        let now = Instant::now();
        let mut sessions = Sessions::default();
        sessions.observe(Some("t"), &main("grok"), "user", BackendGroup::Grok, now);
        assert_eq!(
            sessions.observe(
                Some("t"),
                &monitor(None, false),
                "security",
                BackendGroup::Claude,
                now
            ),
            Some(BackendGroup::Grok)
        );
        let unknown = json!({"model":"claude-haiku-5-5"});
        let empty = Sessions::default();
        assert_eq!(
            empty.main_group(Some("t"), &unknown, Some("grok")),
            Some(BackendGroup::Grok)
        );
        assert_eq!(
            empty.main_group(Some("t"), &unknown, Some("astra[1m]")),
            Some(BackendGroup::Codex)
        );
        assert_eq!(
            empty.main_group(Some("t"), &unknown, Some("claude-opus-5-5[1m]")),
            None
        );
        assert_eq!(empty.main_group(Some("t"), &unknown, Some("made-up")), None);
    }
    #[test]
    fn grok_route_names_the_bare_alias_and_owns_its_effort() {
        let route = Route::monitor(
            &monitor(Some("</severity>"), false),
            Target::grok("grok-4.7", Some("medium".into())),
        )
        .unwrap();
        assert_eq!(route.group(), BackendGroup::Grok);
        let body: Value = serde_json::from_slice(&route.body).unwrap();
        assert_eq!(body["model"], "grok");
        assert_eq!(
            route.meta(),
            ("grok-4.7".into(), Some("medium".into()), false)
        );
        let mut out = message("<severity>7</severity>");
        route.finish(&mut out).unwrap();
        assert_eq!(out["content"][0]["text"], "<severity>7");
        assert_eq!(out["model"], "grok-4.7");
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
