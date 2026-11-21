//! The Antigravity CLI frontend of `llmux run --agy`: spawn `agy` in its
//! official LLM-gateway mode (genai wire) pointed at the llmux Gemini API
//! ingress. Everything is session-scoped environment — agy's own settings,
//! sign-in and home stay untouched (docs/agy-frontend/README.md).

use std::convert::Infallible;
use std::ffi::OsString;
use std::path::Path;

use super::{spawn, Frontend};
use crate::cli::{CliError, Endpoint};

/// Variables that would select another agy backend, or replace the gateway's
/// model discovery with a fixed list — always removed from agy's environment.
pub(crate) const REMOVED: &[&str] = &[
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_GEMINI_BASE_URL",
    "CLOUD_CODE_URL",
    "AGY_ADC_AUTH",
    "AGY_LLM_GATEWAY_MODELS",
];

/// `AGY_LLM_GATEWAY_*` variables a user may keep: transport settings for
/// reaching llmux itself (a custom CA, mTLS, an outbound proxy). Every other
/// `AGY_LLM_GATEWAY_*` the user set is removed (extra headers, a model list).
const KEPT_GATEWAY: &[&str] = &[
    "AGY_LLM_GATEWAY_CA_CERT",
    "AGY_LLM_GATEWAY_CLIENT_CERT",
    "AGY_LLM_GATEWAY_CLIENT_KEY",
    "AGY_LLM_GATEWAY_PROXY_URL",
];

/// Admin (MDM) settings files that win over the environment.
const ADMIN_SETTINGS: &[&str] = &[
    "/Library/Application Support/Antigravity/admin_settings.json",
    "/etc/antigravity/admin_settings.json",
];

/// What `agy` is started with: variables to set, variables to remove.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct GatewayEnv {
    pub(crate) set: Vec<(&'static str, String)>,
    pub(crate) remove: Vec<String>,
}

/// agy refuses plain `http://` to a host that is not loopback; fail before
/// anything starts, with the two ways out.
pub(crate) fn check_endpoint(endpoint: &Endpoint) -> Result<(), CliError> {
    if !endpoint.base_url.starts_with("http://") || is_loopback(&endpoint.host) {
        return Ok(());
    }
    Err(CliError::Message(format!(
        "agy refuses plain http to a non-loopback host ({}:{}); reach the remote llmux through \
         an ssh tunnel (`ssh -L {port}:localhost:{port} <host>`, then `llmux --remote \
         127.0.0.1:{port} run --agy`) or point agy at an https endpoint yourself \
         (`llmux env --agy` shows the variables)",
        endpoint.host,
        endpoint.port,
        port = endpoint.port,
    )))
}

/// `localhost`, `127.0.0.0/8` or `::1` — the hosts agy accepts over http.
fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The model agy starts on, as an id agy's model list carries: a catalog
/// alias (`opus`) resolves to its row's id, a `[1m]` suffix is dropped (the
/// discovery ids carry none).
pub(crate) fn model_id(default_model: &str) -> String {
    let rows = crate::catalog::catalog("", "", "");
    crate::proxy::genai::discovery::default_id(&rows, default_model)
        .unwrap_or_else(|| default_model.trim().to_string())
}

/// The gateway environment for `endpoint`: the ingress URL, the genai wire,
/// the endpoint's key (removed when it has none — a keyless loopback client
/// is `local`), the subagent tiers on `model`, and the removals ([`REMOVED`],
/// plus every inherited `AGY_LLM_GATEWAY_*` this does not set and
/// [`KEPT_GATEWAY`] does not keep).
pub(crate) fn gateway_env(
    endpoint: &Endpoint,
    model: &str,
    inherited: impl IntoIterator<Item = OsString>,
) -> GatewayEnv {
    let mut set: Vec<(&'static str, String)> = vec![
        (
            "AGY_LLM_GATEWAY_URL",
            endpoint.base_url.trim_end_matches('/').to_string(),
        ),
        ("AGY_LLM_GATEWAY_WIRE_PROTOCOL", "genai".to_string()),
    ];
    let key = endpoint
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    if let Some(key) = key {
        set.push(("AGY_LLM_GATEWAY_API_KEY", key.to_string()));
    }
    for tier in [
        "AGY_LLM_GATEWAY_TIER_PRO",
        "AGY_LLM_GATEWAY_TIER_FLASH",
        "AGY_LLM_GATEWAY_TIER_FLASH_LITE",
    ] {
        set.push((tier, model.to_string()));
    }
    let mut remove: Vec<String> = REMOVED.iter().map(|name| name.to_string()).collect();
    if key.is_none() {
        remove.push("AGY_LLM_GATEWAY_API_KEY".to_string());
    }
    let mut stray: Vec<String> = inherited
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .filter(|name| name.starts_with("AGY_LLM_GATEWAY_"))
        .filter(|name| !KEPT_GATEWAY.contains(&name.as_str()))
        .filter(|name| !set.iter().any(|(set, _)| set == name))
        .filter(|name| !remove.contains(name))
        .collect();
    stray.sort();
    remove.extend(stray);
    GatewayEnv { set, remove }
}

/// `--model <model>` unless the agy arguments name a model or start with a
/// subcommand (`agy models`, `agy mcp …`), where a root flag does not belong.
pub(crate) fn model_args(args: &[String], model: &str) -> Vec<String> {
    let names_model = args
        .iter()
        .any(|arg| arg == "--model" || arg.starts_with("--model="));
    let subcommand = args.first().is_some_and(|arg| !arg.starts_with('-'));
    if names_model || subcommand {
        Vec::new()
    } else {
        vec!["--model".to_string(), model.to_string()]
    }
}

/// Spawn `agy` against `endpoint` with the gateway environment, `--model`
/// when the arguments name none, and the user's arguments.
pub(super) async fn launch(
    endpoint: &Endpoint,
    args: &[String],
    default_model: &str,
) -> Result<Infallible, CliError> {
    check_endpoint(endpoint)?;
    for path in ADMIN_SETTINGS {
        if Path::new(path).exists() {
            eprintln!(
                "warning: {path} exists — Antigravity admin settings override the environment, so \
                 a gateway or model list configured there wins over llmux"
            );
        }
    }
    let model = model_id(default_model);
    let env = gateway_env(endpoint, &model, std::env::vars_os().map(|(name, _)| name));
    let mut command = tokio::process::Command::new(Frontend::Agy.binary());
    for name in &env.remove {
        command.env_remove(name);
    }
    for (name, value) in &env.set {
        command.env(name, value);
    }
    command.args(model_args(args, &model));
    command.args(args);
    spawn(command, Frontend::Agy, Vec::new()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(host: &str, key: Option<&str>, remote: bool) -> Endpoint {
        Endpoint {
            base_url: format!("http://{host}:3456"),
            api_key: key.map(str::to_string),
            remote,
            host: host.to_string(),
            port: 3456,
        }
    }

    #[test]
    fn the_gateway_env_sets_the_ingress_and_removes_other_backends() {
        let inherited = [
            "PATH",
            "AGY_LLM_GATEWAY_HEADERS",
            "AGY_LLM_GATEWAY_CA_CERT",
            "AGY_LLM_GATEWAY_URL",
            "AGY_LLM_GATEWAY_MODELS",
        ]
        .map(OsString::from);
        let env = gateway_env(
            &endpoint("localhost", Some("lm-key"), false),
            "gpt-5.6-sol",
            inherited,
        );
        assert_eq!(
            env.set,
            vec![
                ("AGY_LLM_GATEWAY_URL", "http://localhost:3456".to_string()),
                ("AGY_LLM_GATEWAY_WIRE_PROTOCOL", "genai".to_string()),
                ("AGY_LLM_GATEWAY_API_KEY", "lm-key".to_string()),
                ("AGY_LLM_GATEWAY_TIER_PRO", "gpt-5.6-sol".to_string()),
                ("AGY_LLM_GATEWAY_TIER_FLASH", "gpt-5.6-sol".to_string()),
                ("AGY_LLM_GATEWAY_TIER_FLASH_LITE", "gpt-5.6-sol".to_string()),
            ]
        );
        for removed in [
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "GOOGLE_GEMINI_BASE_URL",
            "CLOUD_CODE_URL",
            "AGY_ADC_AUTH",
            "AGY_LLM_GATEWAY_MODELS",
            "AGY_LLM_GATEWAY_HEADERS",
        ] {
            assert!(env.remove.iter().any(|r| r == removed), "{removed}");
        }
        assert!(!env.remove.iter().any(|r| r == "AGY_LLM_GATEWAY_CA_CERT"));
        assert!(!env.remove.iter().any(|r| r == "AGY_LLM_GATEWAY_URL"));
        assert!(!env.remove.iter().any(|r| r == "AGY_LLM_GATEWAY_API_KEY"));
    }

    /// agy sends no key when the variable is unset; a keyless endpoint
    /// leaves it unset (removed), never empty.
    #[test]
    fn the_key_is_set_only_when_the_endpoint_has_one() {
        for key in [None, Some(""), Some("  ")] {
            let env = gateway_env(&endpoint("127.0.0.1", key, false), "m", []);
            assert!(!env
                .set
                .iter()
                .any(|(name, _)| *name == "AGY_LLM_GATEWAY_API_KEY"));
            assert!(env
                .remove
                .iter()
                .any(|name| name == "AGY_LLM_GATEWAY_API_KEY"));
        }
    }

    #[test]
    fn plain_http_to_a_remote_host_is_refused_up_front() {
        for host in ["localhost", "127.0.0.1", "127.8.9.1", "::1", "[::1]"] {
            assert!(
                check_endpoint(&endpoint(host, None, true)).is_ok(),
                "{host}"
            );
        }
        for host in ["gateway.example", "10.0.0.5", "192.168.1.2"] {
            let Err(CliError::Message(message)) = check_endpoint(&endpoint(host, Some("k"), true))
            else {
                panic!("{host} must be refused");
            };
            assert!(message.contains("ssh -L 3456:localhost:3456"), "{message}");
            assert!(message.contains(host), "{message}");
        }
    }

    #[test]
    fn a_user_model_or_subcommand_suppresses_the_default() {
        let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert_eq!(model_args(&args(&[]), "m"), args(&["--model", "m"]));
        assert_eq!(
            model_args(&args(&["-p", "hi"]), "m"),
            args(&["--model", "m"])
        );
        assert!(model_args(&args(&["--model", "x", "-p", "hi"]), "m").is_empty());
        assert!(model_args(&args(&["--model=x"]), "m").is_empty());
        assert!(model_args(&args(&["models"]), "m").is_empty());
    }

    #[test]
    fn the_model_is_an_id_agy_lists() {
        assert_eq!(model_id("gpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(model_id("opus"), "claude-opus-5-5");
        assert_eq!(model_id("gpt-6.2-sol[1m]"), "gpt-6.2-sol");
        assert_eq!(model_id("not-in-catalog"), "not-in-catalog");
    }
}
