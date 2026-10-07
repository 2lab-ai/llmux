//! Narrow wire signatures for implicit Claude Code tasks. Display KIND is not
//! sufficient: a human can ask for a title inside a normal main-agent turn.
use super::auto_classifier::{system_blocks, Route, MAIN};
use crate::config::schema::ClaudeCodeConfig;
use serde_json::{json, Value};

pub const LAUNCH_MODEL_HEADER: &str = "x-llmux-claude-launch-model";
pub const LAUNCH_TIME_HEADER: &str = "x-llmux-claude-launch-time";

/// Bootstrap hints expire after two minutes and never accept future clocks.
pub fn fresh_launch(value: Option<&str>, now_ms: u64) -> bool {
    value
        .and_then(|v| v.parse::<u64>().ok())
        .and_then(|at| now_ms.checked_sub(at))
        .is_some_and(|age| age <= 120_000)
}

pub const TITLE_SYSTEM: &str =
    "You are naming a coding session so the user can pick it out of a long list of sessions.";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Task {
    Quota,
    Title,
    Helper,
}

fn no_tools(body: &Value) -> bool {
    body.get("tools")
        .is_none_or(|v| matches!(v, Value::Array(a) if a.is_empty()))
}

pub fn task(body: &Value, kind: &str) -> Option<Task> {
    let system = system_blocks(body);
    if kind == "quota"
        && no_tools(body)
        && system.iter().all(|s| s.trim().is_empty())
        && body
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|m| m.len() == 1 && m[0]["role"] == "user" && m[0]["content"] == "quota")
    {
        return Some(Task::Quota);
    }
    if no_tools(body) && system.iter().any(|s| s.starts_with(TITLE_SYSTEM)) {
        return Some(Task::Title);
    }
    // Genuine main execution (including a user quoting a control prompt) wins.
    if !no_tools(body)
        && system.iter().any(|s| s.starts_with(MAIN))
        && !system.iter().any(|s| {
            s.contains("cc_is_subagent=true") || s.contains("running within the Claude Agent")
        })
    {
        return None;
    }
    if system
        .iter()
        .any(|s| s.contains("cc_is_subagent=true") || s.contains("running within the Claude Agent"))
    {
        return Some(Task::Helper);
    }
    if no_tools(body)
        && matches!(
            kind,
            "summary" | "audit" | "compact" | "title" | "suggest" | "recap"
        )
    {
        return Some(Task::Helper);
    }
    None
}

pub fn target<'a>(body: &Value, config: &'a ClaudeCodeConfig) -> Option<&'a str> {
    let model = body.get("model")?.as_str()?.trim().to_ascii_lowercase();
    let model = model.strip_suffix("[1m]").unwrap_or(&model);
    let family = model.strip_prefix("claude-").unwrap_or(model);
    let mapping = &config.gpt_model_mapping;
    if family == "opus" || family.starts_with("opus-") {
        Some(&mapping.opus)
    } else if family == "sonnet" || family.starts_with("sonnet-") {
        Some(&mapping.sonnet)
    } else if family == "haiku" || family.starts_with("haiku-") {
        Some(&mapping.haiku)
    } else {
        None
    }
}

pub fn title_schema() -> Value {
    json!({"type":"object","properties":{"title":{"type":"string"}},"required":["title"],"additionalProperties":false})
}

pub fn route(original: &Value, task: Task, model: &str) -> Result<Route, &'static str> {
    let mut body = original.clone();
    body["model"] = json!(model);
    let mut omitted_temperature = false;
    let mut format = None;
    if task == Task::Title {
        match body.get("temperature") {
            Some(v) if v.as_f64() == Some(1.0) => {
                body.as_object_mut()
                    .ok_or("invalid title request")?
                    .remove("temperature");
                omitted_temperature = true;
            }
            None | Some(Value::Null) => {}
            _ => return Err("title compatibility only supports default temperature 1"),
        }
        if let Some(requested) = body.get("output_config").and_then(|v| v.get("format")) {
            if requested.get("type").and_then(Value::as_str) != Some("json_schema")
                || requested.get("schema") != Some(&title_schema())
            {
                return Err("unsupported session title output schema");
            }
            format = Some(
                json!({"type":"json_schema","name":"session_title","strict":true,"schema":title_schema()}),
            );
        }
    }
    // Utility effort is independent of a main agent's global ultra setting.
    // Worker/subagent effort preserves the client's explicit request.
    let effort = if task == Task::Helper {
        None
    } else {
        Some("low".into())
    };
    Ok(Route::utility(
        body,
        model,
        effort,
        format,
        omitted_temperature,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_timestamp_requires_fresh_nonfuture_value() {
        assert!(fresh_launch(Some("1000000"), 1_120_000));
        for value in [None, Some("bad"), Some("1120001"), Some("999999")] {
            assert!(!fresh_launch(value, 1_120_000));
        }
    }
}
