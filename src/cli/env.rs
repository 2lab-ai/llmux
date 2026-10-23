//! `llmux env` — shell exports for the selected proxy endpoint.

use super::{resolve_endpoint, CliError, Endpoint, EnvArgs};

/// Emit eval-safe exports without launching a client or changing its settings.
pub async fn run(args: EnvArgs, remote: Option<String>) -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;
    print!(
        "{}",
        render(&endpoint, args.codex, |var| std::env::var_os(var).is_some())?
    );
    Ok(())
}

/// `already_set` reports whether a variable is already in the caller's
/// environment (the same skip rule as `run`'s alias exports); it is a
/// parameter so tests need no process-global env mutation.
fn render(
    endpoint: &Endpoint,
    codex: bool,
    already_set: impl Fn(&str) -> bool,
) -> Result<String, CliError> {
    if !codex {
        let mut output = format!(
            "export ANTHROPIC_BASE_URL={}\n",
            shell_quote(&endpoint.base_url)
        );
        if let Some(key) = &endpoint.api_key {
            output.push_str(&format!("export ANTHROPIC_API_KEY={}\n", shell_quote(key)));
        }
        // The `ANTHROPIC_DEFAULT_*_MODEL` exports `llmux run` sets, so that
        // Claude Code's NATIVE `opus` / `fable` / `sonnet` / `haiku` resolve
        // to the catalog's `[1m]` ids (see `super::run::ALIAS_ENV`). Offline:
        // `env` never starts the daemon or fetches `/llmux/models`, so the
        // values come from THIS binary's compiled catalog — `run` reads the
        // daemon's. A var the caller already exports is left alone.
        for &(alias, var) in super::run::ALIAS_ENV {
            if already_set(var) {
                continue;
            }
            if let Some(id) = crate::catalog::resolve_claude_alias(alias) {
                output.push_str(&format!("export {var}={}\n", shell_quote(id)));
            }
        }
        return Ok(output);
    }

    // Never accidentally reuse a caller's unrelated OpenAI credential. Both
    // exports are emitted together, or the command fails before printing any.
    let key = endpoint.api_key.as_deref().filter(|key| !key.trim().is_empty()).ok_or_else(|| {
        let field = if endpoint.remote { "remote.api_key" } else { "proxy.api_key" };
        CliError::Message(format!(
            "llmux env --codex requires {field} for the selected endpoint; configure an llmux client key first"
        ))
    })?;
    let base_url = format!("{}/v1", endpoint.base_url.trim_end_matches('/'));
    // JSON string syntax also quotes these values for Codex's TOML overrides.
    // Shell quoting then protects the complete argument (including newlines).
    let provider_url = format!(
        "model_providers.llmux_env.base_url={}",
        serde_json::Value::String(base_url.clone())
    );
    let command = [
        "model_provider=\"llmux_env\"",
        "model_providers.llmux_env.name=\"llmux\"",
        &provider_url,
        "model_providers.llmux_env.env_key=\"OPENAI_API_KEY\"",
        "model_providers.llmux_env.wire_api=\"responses\"",
        "model_providers.llmux_env.requires_openai_auth=false",
        "model_providers.llmux_env.supports_websockets=false",
    ]
    .map(|setting| format!(" -c {}", shell_quote(setting)))
    .concat();
    Ok(format!(
        "export OPENAI_BASE_URL={}\nexport OPENAI_API_KEY={}\n\
         # Codex also needs explicit provider selection; exports alone do not override ChatGPT login or profiles.\n\
         # After eval, run (choose a model with --model):\n\
         # codex{command}\n\
         # For subcommands, place exec/review/resume immediately after codex, before these -c flags.\n\
         # This does not start the daemon. For automatic startup and the llmux model picker: llmux run --codex\n",
        shell_quote(&base_url),
        shell_quote(key),
    ))
}

/// POSIX shell word quoting; preserve the existing simple export format.
fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Endpoint {
        Endpoint {
            base_url: "http://127.0.0.1:3456".into(),
            api_key: Some("lmk-test".into()),
            remote: false,
            host: "127.0.0.1".into(),
            port: 3456,
        }
    }

    /// Plain `env` prints the same four alias exports `run` sets, from this
    /// binary's catalog, `[1m]` ids shell-quoted, after the endpoint exports.
    #[test]
    fn render_exports_the_compiled_catalog_alias_owners() {
        let output = render(&endpoint(), false, |_| false).expect("render");
        assert_eq!(
            output,
            "export ANTHROPIC_BASE_URL=http://127.0.0.1:3456\n\
             export ANTHROPIC_API_KEY=lmk-test\n\
             export ANTHROPIC_DEFAULT_OPUS_MODEL='claude-opus-5-5[1m]'\n\
             export ANTHROPIC_DEFAULT_FABLE_MODEL='claude-fable-5-1[1m]'\n\
             export ANTHROPIC_DEFAULT_SONNET_MODEL='claude-sonnet-5-5[1m]'\n\
             export ANTHROPIC_DEFAULT_HAIKU_MODEL='claude-haiku-5-5[1m]'\n"
        );
    }

    /// The caller's own export wins: that var is omitted, the rest still print.
    #[test]
    fn render_omits_an_alias_var_already_set() {
        let output = render(&endpoint(), false, |var| {
            var == "ANTHROPIC_DEFAULT_HAIKU_MODEL"
        })
        .expect("render");
        assert!(
            !output.contains("ANTHROPIC_DEFAULT_HAIKU_MODEL"),
            "{output}"
        );
        for var in [
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_FABLE_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
        ] {
            assert!(
                output.contains(&format!("export {var}=")),
                "{var}: {output}"
            );
        }
    }

    /// `--codex` prints only the OpenAI exports and the commented command —
    /// the alias exports (and the env predicate) do not touch it.
    #[test]
    fn render_codex_branch_carries_no_claude_alias_exports() {
        let unset = render(&endpoint(), true, |_| false).expect("render");
        let set = render(&endpoint(), true, |_| true).expect("render");
        assert_eq!(unset, set);
        assert!(!unset.contains("ANTHROPIC_"), "{unset}");
        assert!(
            unset.starts_with(
                "export OPENAI_BASE_URL=http://127.0.0.1:3456/v1\nexport OPENAI_API_KEY=lmk-test\n"
            ),
            "{unset}"
        );
    }
}
