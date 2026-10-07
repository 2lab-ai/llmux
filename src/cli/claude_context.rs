//! Conservative launch context for the quota request sent BEFORE the first
//! main turn. This is not a complete Claude settings implementation. Unknown
//! policy/agent/resume sources deliberately produce no hint.
use crate::proxy::internal_requests::{LAUNCH_MODEL_HEADER, LAUNCH_TIME_HEADER};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let mut result = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == name {
            result = iter.next().map(String::as_str);
        } else if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
            result = Some(value);
        }
    }
    result
}
fn flag(args: &[String], names: &[&str]) -> bool {
    args.iter().take_while(|a| a.as_str() != "--").any(|a| {
        names
            .iter()
            .any(|n| a == n || a.starts_with(&format!("{n}=")))
    })
}
fn known(model: &str) -> Option<String> {
    let model = model.trim().to_ascii_lowercase();
    if model.len() > 128 {
        return None;
    }
    let base = model.strip_suffix("[1m]").unwrap_or(&model);
    crate::catalog::catalog("", "", "")
        .iter()
        .any(|entry| entry.id == model || entry.aliases.iter().any(|a| a == base))
        .then_some(model)
}

fn model_env(value: &Value) -> bool {
    value
        .get("env")
        .and_then(Value::as_object)
        .is_some_and(|env| {
            env.keys().any(|key| {
                key == "ANTHROPIC_MODEL"
                    || (key.starts_with("ANTHROPIC_DEFAULT_") && key.ends_with("_MODEL"))
            })
        })
}

fn global_env_uncertain(user_dir: &Path, global_fallback: &Path) -> bool {
    let legacy = user_dir.join(".config.json");
    let path = if legacy.exists() {
        legacy.as_path()
    } else {
        global_fallback
    };
    match read_settings(path) {
        Ok(Some(value)) => model_env(&value),
        Ok(None) => false,
        Err(()) => true,
    }
}

/// Caller supplies settings in native precedence: user, project, local, flag.
/// Policy and agent-derived selection are not guessed. Explicit --model still
/// works with safe-mode / setting-sources, which only suppress implicit lookup.
fn select(
    args: &[String],
    env_model: Option<&str>,
    settings: &[Value],
    uncertain_policy: bool,
) -> Option<String> {
    if uncertain_policy
        || settings
            .iter()
            .any(|v| v.get("availableModels").is_some() || model_env(v))
    {
        return None;
    }
    if let Some(model) = option(args, "--model") {
        return known(model);
    }
    if flag(
        args,
        &[
            "--resume",
            "-r",
            "--continue",
            "-c",
            "--agent",
            "--agents",
            "--routine",
            "--fork-session",
        ],
    ) {
        return None;
    }
    if settings.iter().any(|v| v.get("agent").is_some()) {
        return None;
    }
    if let Some(model) = env_model {
        return known(model);
    }
    if flag(
        args,
        &[
            "--safe-mode",
            "--setting-sources",
            "--settings-sources",
            "--restricted",
        ],
    ) {
        return None;
    }
    settings
        .iter()
        .rev()
        .find_map(|v| v.get("model"))
        .and_then(Value::as_str)
        .and_then(known)
}

fn read_settings(path: &Path) -> Result<Option<Value>, ()> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
        Ok(meta) if meta.len() > 1024 * 1024 => Err(()),
        Ok(_) => {
            let text = std::fs::read_to_string(path).map_err(|_| ())?;
            let value: Value = serde_json::from_str(&text).map_err(|_| ())?;
            if !value.is_object() {
                return Err(());
            }
            Ok(Some(value))
        }
    }
}

fn resolve(
    args: &[String],
    env_model: Option<&str>,
    user_dir: &Path,
    cwd: &Path,
    policy_paths: &[PathBuf],
) -> Option<String> {
    let project = cwd
        .ancestors()
        .find(|p| p.join(".git").exists())
        .unwrap_or(cwd);
    let mut settings = Vec::new();
    for path in [
        user_dir.join("settings.json"),
        project.join(".claude/settings.json"),
        project.join(".claude/settings.local.json"),
    ] {
        if let Some(value) = read_settings(&path).ok()? {
            settings.push(value);
        }
    }
    if let Some(raw) = option(args, "--settings") {
        let value = if raw.trim_start().starts_with('{') {
            serde_json::from_str(raw).ok()?
        } else {
            read_settings(&cwd.join(raw)).ok()??
        };
        settings.push(value);
    }
    // Managed configuration can also be delivered remotely. Local policy
    // carriers disable bootstrap conservatively; remote-only policy is not inferred.
    let policy = policy_paths.iter().any(|p| p.exists());
    select(args, env_model, &settings, policy)
}

fn native_alias(model: String, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    let base = model.strip_suffix("[1m]").unwrap_or(&model);
    let var = match base {
        "opus" => "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "sonnet" => "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "haiku" => "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "fable" => "ANTHROPIC_DEFAULT_FABLE_MODEL",
        _ => return Some(model),
    };
    match lookup(var) {
        Some(value) => known(&value),
        None => Some(model),
    }
}

fn nonstandard_oauth_profile(
    env: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> bool {
    // Claude Code 2.1.292's native profile selector uses this URL to choose
    // .claude-custom-oauth.json. Normal CLAUDE_CODE_OAUTH_TOKEN authentication
    // does not change settings provenance and must retain bootstrap context.
    env.into_iter()
        .any(|(key, value)| key == "CLAUDE_CODE_CUSTOM_OAUTH_URL" && !value.is_empty())
}

pub(super) fn launch_model(args: &[String], exports: &[(&str, String)]) -> Option<String> {
    let user_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".claude")))?;
    let global_fallback = if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() {
        user_dir.join(".claude.json")
    } else {
        dirs::home_dir()?.join(".claude.json")
    };
    // Nonstandard OAuth profiles can select a different global config carrier.
    if nonstandard_oauth_profile(std::env::vars_os())
        || global_env_uncertain(&user_dir, &global_fallback)
    {
        return None;
    }
    let policies = [
        PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json"),
        PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.d"),
        PathBuf::from("/etc/claude-code/managed-settings.json"),
        user_dir.join("managed-settings.json"),
    ];
    let selected = resolve(
        args,
        std::env::var("ANTHROPIC_MODEL").ok().as_deref(),
        &user_dir,
        &std::env::current_dir().ok()?,
        &policies,
    )?;
    native_alias(selected, |var| {
        exports
            .iter()
            .find(|(name, _)| *name == var)
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var(var).ok())
    })
}

/// Preserve unrelated custom headers; replace rather than duplicate the
/// reserved hint, including removing a stale inherited hint when unknown.
pub(super) fn custom_headers(existing: Option<&str>, model: Option<&str>, now_ms: u64) -> String {
    let mut lines: Vec<String> = existing
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            !line.split_once(':').is_some_and(|(name, _)| {
                name.trim().eq_ignore_ascii_case(LAUNCH_MODEL_HEADER)
                    || name.trim().eq_ignore_ascii_case(LAUNCH_TIME_HEADER)
            })
        })
        .map(str::to_string)
        .collect();
    if let Some(model) = model.and_then(known) {
        lines.push(format!("{LAUNCH_MODEL_HEADER}: {model}"));
        lines.push(format!("{LAUNCH_TIME_HEADER}: {now_ms}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }
    #[test]
    fn launch_model_precedence_and_ambiguous_sources() {
        let settings = [json!({"model":"sol[1m]"}), json!({"model":"opus"})];
        assert_eq!(select(&[], None, &settings, false).as_deref(), Some("opus"));
        assert_eq!(
            select(&[], Some("terra"), &settings, false).as_deref(),
            Some("terra")
        );
        assert_eq!(
            select(
                &args(&["--model=sol", "--safe-mode", "--setting-sources", ""]),
                None,
                &settings,
                false
            )
            .as_deref(),
            Some("sol")
        );
        assert_eq!(
            select(&args(&["--model", "luna"]), Some("terra"), &settings, false).as_deref(),
            Some("luna")
        );
        for argv in [
            args(&["--resume", "s"]),
            args(&["--continue"]),
            args(&["--agent", "reviewer"]),
            args(&["--safe-mode"]),
            args(&["--setting-sources", ""]),
        ] {
            assert!(select(&argv, None, &settings, false).is_none());
        }
        assert!(select(&args(&["--model", "sol"]), None, &settings, true).is_none());
        assert!(select(
            &args(&["--model", "sol"]),
            None,
            &[json!({"availableModels":["opus"]})],
            false
        )
        .is_none());
        assert!(select(
            &[],
            Some("sol"),
            &[json!({"model":"sol","env":{"ANTHROPIC_MODEL":"opus"}})],
            false
        )
        .is_none());
        assert!(select(
            &args(&["--model", "sonnet"]),
            None,
            &[json!({"env":{"ANTHROPIC_DEFAULT_SONNET_MODEL":"sol"}})],
            false
        )
        .is_none());
        assert_eq!(
            select(&args(&["--", "--model", "luna"]), None, &settings, false).as_deref(),
            Some("opus")
        );
        assert!(select(&args(&["--model", "default"]), None, &settings, false).is_none());
    }
    #[test]
    fn launch_model_standard_settings_file_and_override() {
        let dir = std::env::temp_dir().join(format!("llmux-context-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(dir.join("project/.claude")).unwrap();
        std::fs::create_dir_all(dir.join("user")).unwrap();
        std::fs::write(dir.join("user/settings.json"), r#"{"model":"sol[1m]"}"#).unwrap();
        let load = |a: &[String]| resolve(a, None, &dir.join("user"), &dir.join("project"), &[]);
        assert_eq!(load(&[]).as_deref(), Some("sol[1m]"));
        std::fs::write(
            dir.join("project/.claude/settings.local.json"),
            r#"{"model":"sonnet"}"#,
        )
        .unwrap();
        assert_eq!(load(&[]).as_deref(), Some("sonnet"));
        assert_eq!(
            load(&args(&["--settings", r#"{"model":"luna"}"#])).as_deref(),
            Some("luna")
        );
        let global = dir.join("user/.claude.json");
        std::fs::write(&global, r#"{"env":{"ANTHROPIC_MODEL":"opus"}}"#).unwrap();
        assert!(global_env_uncertain(&dir.join("user"), &global));
        std::fs::write(
            dir.join("user/.config.json"),
            r#"{"env":{"UNRELATED":"kept"}}"#,
        )
        .unwrap();
        assert!(!global_env_uncertain(&dir.join("user"), &global));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn launch_model_oauth_token_keeps_gpt_bootstrap_custom_profile_does_not() {
        let settings = [json!({"model":"sol"})];
        for name in ["CLAUDE_CODE_OAUTH_TOKEN", "UNRELATED_VARIABLE"] {
            let uncertain = nonstandard_oauth_profile([(name.into(), "synthetic-fixture".into())]);
            let model = select(&[], None, &settings, uncertain);
            assert_eq!(
                model.as_deref(),
                Some("sol"),
                "ordinary auth must preserve launch context: {name}"
            );
            assert!(custom_headers(None, model.as_deref(), 1000)
                .contains("x-llmux-claude-launch-model: sol"));
        }
        assert!(!nonstandard_oauth_profile([(
            "CLAUDE_CODE_CUSTOM_OAUTH_URL".into(),
            "".into()
        )]));
        let uncertain = nonstandard_oauth_profile([(
            "CLAUDE_CODE_CUSTOM_OAUTH_URL".into(),
            "https://example.invalid".into(),
        )]);
        assert!(select(&[], None, &settings, uncertain).is_none());
    }
    #[test]
    fn launch_model_native_alias_exports() {
        assert_eq!(
            native_alias("sonnet[1m]".into(), |_| Some("sol".into())).as_deref(),
            Some("sol")
        );
        assert_eq!(
            native_alias("sol".into(), |_| Some("opus".into())).as_deref(),
            Some("sol")
        );
        assert!(native_alias("haiku".into(), |_| Some("unrecognized".into())).is_none());
    }
    #[test]
    fn launch_model_headers_preserve_unrelated_and_clear_stale_hint() {
        let old = "X-Custom: keep\nx-llmux-claude-launch-model: opus\nX-Other: retained";
        assert_eq!(
            custom_headers(Some(old), Some("sol"), 1000),
            "X-Custom: keep\nX-Other: retained\nx-llmux-claude-launch-model: sol\nx-llmux-claude-launch-time: 1000"
        );
        assert_eq!(
            custom_headers(Some(old), None, 1000),
            "X-Custom: keep\nX-Other: retained"
        );
        assert!(
            !custom_headers(None, Some("sol\nAuthorization: bad"), 1000).contains("Authorization")
        );
    }
}
