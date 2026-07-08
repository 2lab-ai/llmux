//! `llmux env` — print shell exports for pointing Claude Code at the
//! proxy (the whole integration contract: `ANTHROPIC_BASE_URL`).

use super::{resolve_endpoint, CliError, EnvArgs};

/// Print `export ANTHROPIC_BASE_URL=http://<host>:<port>` (plus the proxy api
/// key when configured) for eval-style use: `eval "$(llmux env)"`.
///
/// Honors `--remote` / `remote.host`: with a remote configured this prints the
/// remote's URL and `x-api-key`. For the LOCAL daemon `llmux run` deliberately
/// does NOT export `ANTHROPIC_API_KEY` (leaving it unset keeps Claude Code in
/// subscription mode); it is printed here for clients that must authenticate to
/// the proxy from off-host.
pub async fn run(args: EnvArgs, remote: Option<String>) -> Result<(), CliError> {
    let EnvArgs {} = args;
    let config = crate::config::load_or_init()?;
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;
    println!("export ANTHROPIC_BASE_URL={}", endpoint.base_url);
    if let Some(api_key) = &endpoint.api_key {
        println!("export ANTHROPIC_API_KEY={api_key}");
    }
    Ok(())
}
