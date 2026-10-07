//! Private local-control credential handoff. Provider credentials never leave the CLI.
use super::CliError;
use crate::config::Config;
use clap::Args;
use std::io::IsTerminal;

#[derive(Debug, Args)]
pub struct ConnectionArgs {
    #[arg(long)]
    pub port: u16,
}

pub fn connection(args: ConnectionArgs, remote: Option<String>) -> Result<(), CliError> {
    if std::io::stdout().is_terminal() {
        return Err(CliError::Message(
            "This private connection handoff requires a pipe.".into(),
        ));
    }
    // Read the existing path without init/adoption writes. Never serialize Config.
    let config = crate::config::load_path(&crate::config::config_path()?)?;
    let output = handoff(&config, args.port, remote.as_deref())?;
    println!("{output}");
    Ok(())
}

fn handoff(
    config: &Config,
    port: u16,
    remote: Option<&str>,
) -> Result<serde_json::Value, CliError> {
    if remote.is_some() || config.remote.host.is_some() {
        return Err(CliError::Message("Local connection unavailable: llmux is configured for a remote daemon. Use the app's remote connection settings.".into()));
    }
    if port == 0 || port != config.proxy.port {
        return Err(CliError::Message(
            "Local connection port does not match the configured llmux daemon.".into(),
        ));
    }
    let key = config
        .proxy
        .api_key
        .as_deref()
        .filter(|key| !key.is_empty() && key.len() <= 4096)
        .ok_or_else(|| {
            CliError::Message(
                "Local connection unavailable. Start the configured llmux daemon first.".into(),
            )
        })?;
    Ok(serde_json::json!({"endpoint":format!("http://127.0.0.1:{port}"),"api_key":key}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handoff_contains_only_local_endpoint_and_control_key() {
        let mut config = Config::default();
        config.proxy.api_key = Some("synthetic-control".into());
        let out = handoff(&config, config.proxy.port, None).unwrap();
        assert_eq!(out.as_object().unwrap().len(), 2);
        assert_eq!(out["api_key"], "synthetic-control");
        assert_eq!(
            out["endpoint"],
            format!("http://127.0.0.1:{}", config.proxy.port)
        );
    }
    #[test]
    fn handoff_refuses_remote_wrong_port_and_missing_key_without_echoing_secrets() {
        let mut config = Config::default();
        config.proxy.api_key = Some("synthetic-control".into());
        for result in [
            handoff(&config, 0, None),
            handoff(&config, 1, None),
            handoff(&config, config.proxy.port, Some("secret-host")),
        ] {
            let error = result.unwrap_err().to_string();
            assert!(!error.contains("synthetic-control"));
            assert!(!error.contains("secret-host"));
        }
        config.remote.host = Some("secret-host".into());
        assert!(handoff(&config, config.proxy.port, None).is_err());
        config.remote.host = None;
        config.proxy.api_key = None;
        assert!(handoff(&config, config.proxy.port, None).is_err());
    }
}
