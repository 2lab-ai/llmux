//! `llmux env` — shell exports for the selected proxy endpoint.

use super::{resolve_endpoint, CliError, Endpoint, EnvArgs};

/// Emit eval-safe exports without launching a client or changing its settings.
pub async fn run(args: EnvArgs, remote: Option<String>) -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;
    print!("{}", render(&endpoint, args.codex)?);
    Ok(())
}

fn render(endpoint: &Endpoint, codex: bool) -> Result<String, CliError> {
    if !codex {
        let mut output = format!(
            "export ANTHROPIC_BASE_URL={}\n",
            shell_quote(&endpoint.base_url)
        );
        if let Some(key) = &endpoint.api_key {
            output.push_str(&format!("export ANTHROPIC_API_KEY={}\n", shell_quote(key)));
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
