//! `llmux env` — shell exports for the selected proxy endpoint.

use super::run::{agy, Frontend};
use super::{resolve_endpoint, CliError, Endpoint, EnvArgs};

/// Emit eval-safe exports without launching a client or changing its settings.
pub async fn run(args: EnvArgs, remote: Option<String>) -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;
    print!(
        "{}",
        render(
            &endpoint,
            Frontend::from_env_args(&args),
            &config.agy_default_model()
        )?
    );
    Ok(())
}

/// The exports for `frontend`, one renderer per client frontend.
/// `agy_model` is the model `--agy` starts on.
fn render(endpoint: &Endpoint, frontend: Frontend, agy_model: &str) -> Result<String, CliError> {
    match frontend {
        Frontend::ClaudeCode => Ok(render_claude(endpoint)),
        Frontend::Codex => render_codex(endpoint),
        Frontend::Agy => {
            agy::check_endpoint(endpoint)?;
            let model = agy::model_id(agy_model);
            Ok(render_agy(
                &agy::gateway_env(endpoint, &model, std::env::vars_os().map(|(name, _)| name)),
                &model,
            ))
        }
    }
}

/// The gateway exports `run --agy` sets, the `unset` line for what it
/// removes, and the launch line.
fn render_agy(env: &agy::GatewayEnv, model: &str) -> String {
    let mut output = String::new();
    for (name, value) in &env.set {
        output.push_str(&format!("export {name}={}\n", shell_quote(value)));
    }
    output.push_str(&format!("unset {}\n", env.remove.join(" ")));
    output.push_str(&format!(
        "# After eval, run: agy --model {}\n\
         # This does not start the daemon. For automatic startup: llmux run --agy\n",
        shell_quote(model)
    ));
    output
}

fn render_claude(endpoint: &Endpoint) -> String {
    let mut output = format!(
        "export ANTHROPIC_BASE_URL={}\n",
        shell_quote(&endpoint.base_url)
    );
    if let Some(key) = &endpoint.api_key {
        output.push_str(&format!("export ANTHROPIC_API_KEY={}\n", shell_quote(key)));
    }
    output
}

fn render_codex(endpoint: &Endpoint) -> Result<String, CliError> {
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

    /// Plain `env` prints the endpoint exports only. It deliberately does NOT
    /// print the `ANTHROPIC_DEFAULT_*_MODEL` alias exports: `run` skips a var
    /// already in the environment as the user's own, so llmux-written values
    /// left in a shell would shadow a later catalog's alias move.
    #[test]
    fn render_prints_only_the_endpoint_exports() {
        assert_eq!(
            render(&endpoint(), Frontend::ClaudeCode, "m").expect("render"),
            "export ANTHROPIC_BASE_URL=http://127.0.0.1:3456\n\
             export ANTHROPIC_API_KEY=lmk-test\n"
        );
    }

    #[test]
    fn render_agy_prints_the_gateway_exports_and_the_unset_list() {
        let output = render(&endpoint(), Frontend::Agy, "opus").expect("render");
        assert!(
            output.starts_with(
                "export AGY_LLM_GATEWAY_URL=http://127.0.0.1:3456\n\
                 export AGY_LLM_GATEWAY_WIRE_PROTOCOL=genai\n\
                 export AGY_LLM_GATEWAY_API_KEY=lmk-test\n\
                 export AGY_LLM_GATEWAY_TIER_PRO=claude-opus-5-5\n"
            ),
            "{output}"
        );
        assert!(
            output.contains("\nunset GEMINI_API_KEY GOOGLE_API_KEY GOOGLE_GEMINI_BASE_URL CLOUD_CODE_URL AGY_ADC_AUTH AGY_LLM_GATEWAY_MODELS"),
            "{output}"
        );
        assert!(output.contains("agy --model claude-opus-5-5"), "{output}");
        assert!(!output.contains("ANTHROPIC_") && !output.contains("OPENAI_"));
        let remote = Endpoint {
            base_url: "http://gateway.example:3456".into(),
            remote: true,
            host: "gateway.example".into(),
            ..endpoint()
        };
        assert!(render(&remote, Frontend::Agy, "opus").is_err());
    }

    #[test]
    fn render_codex_branch_prints_openai_exports_only() {
        let output = render(&endpoint(), Frontend::Codex, "m").expect("render");
        assert!(!output.contains("ANTHROPIC_"), "{output}");
        assert!(
            output.starts_with(
                "export OPENAI_BASE_URL=http://127.0.0.1:3456/v1\nexport OPENAI_API_KEY=lmk-test\n"
            ),
            "{output}"
        );
    }
}
