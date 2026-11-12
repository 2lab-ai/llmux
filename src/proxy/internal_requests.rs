//! Narrow wire signatures for implicit Claude Code tasks (harness requests)
//! and the per-provider tier policy that serves them in a Codex or Grok main
//! session (`.prd/27-claude-code-harness-requests.md`). Display KIND is not
//! sufficient: a human can ask for a title inside a normal main-agent turn.
use super::auto_classifier::{system_blocks, Route, Target, MAIN};
use crate::config::schema::ClaudeCodeConfig;
use crate::routing::BackendGroup;
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

/// Claude Code's notification status classifier (captured 2026-11-12, CLI
/// 2.1.322): decides whether a walked-away agent is done, working or blocked.
pub const STATUS_SYSTEM: &str =
    "A user kicked off a Claude Code agent to do a coding task and walked away.";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Task {
    Quota,
    Title,
    /// The notification status classifier.
    Status,
    Helper,
}

impl Task {
    /// Fixed short-output tasks: they always run at low effort.
    fn is_utility(self) -> bool {
        !matches!(self, Self::Helper)
    }
}

/// The Claude tier a harness request names. Only these three family names are
/// generic tiers; `fable` and concrete non-Claude ids are explicit choices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tier {
    Opus,
    Sonnet,
    Haiku,
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
    if no_tools(body) && system.iter().any(|s| s.starts_with(STATUS_SYSTEM)) {
        return Some(Task::Status);
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

/// The tier named by the request's `model` (`claude-haiku-5-5`, `sonnet`,
/// `claude-opus-5-5[1m]`, …).
pub fn tier(body: &Value) -> Option<Tier> {
    let model = body.get("model")?.as_str()?.trim().to_ascii_lowercase();
    let model = model.strip_suffix("[1m]").unwrap_or(&model);
    let family = model.strip_prefix("claude-").unwrap_or(model);
    let is = |name: &str| family == name || family.starts_with(&format!("{name}-"));
    if is("opus") {
        Some(Tier::Opus)
    } else if is("sonnet") {
        Some(Tier::Sonnet)
    } else if is("haiku") {
        Some(Tier::Haiku)
    } else {
        None
    }
}

/// Effort ladder used to cap a Grok helper turn (lowest first).
const EFFORTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

fn grok_cap(tier: Tier) -> &'static str {
    match tier {
        Tier::Opus => "high",
        Tier::Sonnet => "medium",
        Tier::Haiku => "low",
    }
}

/// The client's effort, lowered to `cap` (the cap when the client sent none).
fn capped(body: &Value, cap: &'static str) -> String {
    let rank = |e: &str| EFFORTS.iter().position(|x| *x == e);
    let requested = body
        .get("output_config")
        .and_then(|c| c.get("effort"))
        .and_then(Value::as_str)
        .map(|e| e.trim().to_ascii_lowercase());
    match requested {
        Some(e) if rank(&e).is_some_and(|r| Some(r) < rank(cap)) => e,
        _ => cap.to_string(),
    }
}

/// Where a harness request goes in a session whose main model is served by
/// `main`. Codex uses its smaller models (`gpt_model_mapping`, default
/// `terra`/`terra`/`luna`); Grok has one line, so it keeps the `grok` pin and
/// lowers effort. `None` = not routed (a non-tier model, or a Claude main).
pub fn target(
    body: &Value,
    task: Task,
    main: BackendGroup,
    config: &ClaudeCodeConfig,
    grok_pin: &str,
) -> Option<Target> {
    let tier = tier(body)?;
    match main {
        BackendGroup::Codex => {
            let mapping = &config.gpt_model_mapping;
            let model = match tier {
                Tier::Opus => &mapping.opus,
                Tier::Sonnet => &mapping.sonnet,
                Tier::Haiku => &mapping.haiku,
            };
            // Worker/subagent effort preserves the client's explicit request.
            Some(Target::codex(
                model,
                task.is_utility().then(|| "low".to_string()),
            ))
        }
        BackendGroup::Grok => {
            let effort = if task.is_utility() {
                "low".to_string()
            } else {
                capped(body, grok_cap(tier))
            };
            Some(Target::grok(grok_pin, Some(effort)))
        }
        _ => None,
    }
}

/// The security monitor's target in a `main` session: Codex uses
/// `auto_classifier_model` (default `luna`), Grok its pin; both at medium.
/// Like every harness request, only a monitor naming a Claude tier is
/// rerouted — an explicit non-tier choice (`fable`, a GPT/Grok id) is kept.
pub fn monitor_target(
    body: &Value,
    main: BackendGroup,
    config: &ClaudeCodeConfig,
    grok_pin: &str,
) -> Option<Target> {
    tier(body)?;
    let effort = Some("medium".to_string());
    match main {
        BackendGroup::Codex => Some(Target::codex(&config.auto_classifier_model, effort)),
        BackendGroup::Grok => Some(Target::grok(grok_pin, effort)),
        _ => None,
    }
}

pub fn title_schema() -> Value {
    json!({"type":"object","properties":{"title":{"type":"string"}},"required":["title"],"additionalProperties":false})
}

pub fn route(original: &Value, task: Task, target: Target) -> Result<Route, &'static str> {
    let mut body = original.clone();
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
    // Utility effort is independent of a main agent's global ultra setting;
    // the target carries it (see `target`).
    Ok(Route::utility(body, target, format, omitted_temperature))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grok_effort(target: &Target) -> Option<&str> {
        match target {
            Target::Grok(shape) => shape.effort.as_deref(),
            Target::Codex(_) => None,
        }
    }

    #[test]
    fn status_classifier_is_a_utility_task() {
        let body = json!({"model":"claude-haiku-5-5","system":[
            {"text":"x-anthropic-billing-header: cc_version=2.1.322;"},
            {"text":format!("{STATUS_SYSTEM} Read the tail…")}],
            "messages":[{"role":"user","content":"Current state: working"}]});
        assert_eq!(task(&body, "status"), Some(Task::Status));
        let mut tooled = body.clone();
        tooled["tools"] = json!([{"name":"Bash"}]);
        assert_eq!(task(&tooled, "user"), None);
    }

    #[test]
    fn tiers_come_from_claude_family_names_only() {
        for (model, tier_) in [
            ("claude-haiku-5-5", Some(Tier::Haiku)),
            ("haiku", Some(Tier::Haiku)),
            ("claude-sonnet-5[1m]", Some(Tier::Sonnet)),
            ("claude-opus-5-5", Some(Tier::Opus)),
            ("claude-fable-5-2", None),
            ("astra", None),
            ("grok", None),
            ("opusplan-x", None),
        ] {
            assert_eq!(tier(&json!({ "model": model })), tier_, "{model}");
        }
    }

    #[test]
    fn codex_uses_terra_and_luna_aliases_by_default() {
        let config = ClaudeCodeConfig::default();
        for (model, task_, alias, effort) in [
            ("claude-haiku-5-5", Task::Title, "luna", Some("low")),
            ("claude-haiku-5-5", Task::Status, "luna", Some("low")),
            ("claude-sonnet-5-5", Task::Helper, "terra", None),
            ("claude-opus-5-5", Task::Helper, "terra", None),
        ] {
            let body = json!({ "model": model, "output_config": {"effort": "max"} });
            let Some(Target::Codex(shape)) =
                target(&body, task_, BackendGroup::Codex, &config, "grok-4.7")
            else {
                panic!("{model} must route to codex");
            };
            assert_eq!(shape.model, alias, "{model}");
            assert_eq!(shape.effort.as_deref(), effort, "{model}");
        }
        let Some(Target::Codex(shape)) = monitor_target(
            &json!({"model":"claude-sonnet-5[1m]"}),
            BackendGroup::Codex,
            &config,
            "grok-4.7",
        ) else {
            panic!("codex monitor");
        };
        assert_eq!(
            (shape.model.as_str(), shape.effort.as_deref()),
            ("luna", Some("medium"))
        );
    }

    #[test]
    fn grok_keeps_its_pin_and_lowers_effort_by_tier() {
        let config = ClaudeCodeConfig::default();
        for (model, task_, client, expected) in [
            ("claude-haiku-5-5", Task::Title, Some("medium"), "low"),
            ("claude-haiku-5-5", Task::Quota, None, "low"),
            ("claude-haiku-5-5", Task::Helper, Some("xhigh"), "low"),
            ("claude-sonnet-5-5", Task::Helper, Some("max"), "medium"),
            ("claude-sonnet-5-5", Task::Helper, Some("low"), "low"),
            ("claude-opus-5-5", Task::Helper, Some("ultra"), "high"),
            ("claude-opus-5-5", Task::Helper, Some("medium"), "medium"),
            ("claude-opus-5-5", Task::Helper, None, "high"),
            ("claude-opus-5-5", Task::Helper, Some("bogus"), "high"),
        ] {
            let mut body = json!({ "model": model });
            if let Some(client) = client {
                body["output_config"] = json!({ "effort": client });
            }
            let routed =
                target(&body, task_, BackendGroup::Grok, &config, "grok-4.7").expect("grok route");
            assert_eq!(routed.group(), BackendGroup::Grok);
            assert_eq!(routed.body_model(), "grok");
            assert_eq!(grok_effort(&routed), Some(expected), "{model} {client:?}");
        }
        let monitor = monitor_target(
            &json!({"model":"claude-sonnet-5"}),
            BackendGroup::Grok,
            &config,
            "grok-4.7",
        )
        .unwrap();
        assert_eq!(grok_effort(&monitor), Some("medium"));
        for main in [
            BackendGroup::Claude,
            BackendGroup::Gemini,
            BackendGroup::OpenRouter,
        ] {
            assert!(target(&json!({"model":"haiku"}), Task::Title, main, &config, "g").is_none());
            assert!(
                monitor_target(&json!({"model":"claude-sonnet-5"}), main, &config, "g").is_none()
            );
        }
        assert!(target(
            &json!({"model":"claude-fable-5-2"}),
            Task::Helper,
            BackendGroup::Grok,
            &config,
            "g"
        )
        .is_none());
        // A monitor naming a non-tier model is an explicit choice: kept.
        for model in ["claude-fable-5-2", "gpt-6-astra", "grok-4.6"] {
            for main in [BackendGroup::Codex, BackendGroup::Grok] {
                assert!(
                    monitor_target(&json!({ "model": model }), main, &config, "g").is_none(),
                    "{model} in {main} session"
                );
            }
        }
    }
    #[test]
    fn launch_timestamp_requires_fresh_nonfuture_value() {
        assert!(fresh_launch(Some("1000000"), 1_120_000));
        for value in [None, Some("bad"), Some("1120001"), Some("999999")] {
            assert!(!fresh_launch(value, 1_120_000));
        }
    }
}
