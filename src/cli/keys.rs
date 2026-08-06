//! `llmux key <new|list|suspend|resume|remove|rotate>` — downstream client-key
//! management (multi-tenant #22). Every subcommand talks to the daemon's
//! `/llmux/keys*` control endpoints; the control plane requires an ADMIN
//! credential even on loopback, which the local CLI satisfies automatically by
//! presenting the config's own `proxy.api_key` (the legacy admin credential).
//! With `--remote` the same commands manage a remote daemon using
//! `remote.api_key` — which must then be an admin-kind key.

use super::{resolve_endpoint, CliError, Endpoint, KeyArgs, KeyCommand};

pub async fn run(args: KeyArgs, remote: Option<String>) -> Result<(), CliError> {
    let config = crate::config::load_or_init()?;
    let endpoint = resolve_endpoint(remote.as_deref(), &config)?;
    match args.command {
        KeyCommand::New(new) => {
            let kind = if new.admin { "admin" } else { "default" };
            let body = serde_json::json!({
                "name": new.name,
                "email": new.email,
                "kind": kind,
            });
            let doc = post(&endpoint, "/llmux/keys/new", &body).await?;
            let key = doc
                .get("key")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| CliError::Message("daemon response carried no key".into()))?;
            println!("issued {} ({kind})", field(&doc, "id"));
            println!("  name:  {}", field(&doc, "name"));
            if let Some(email) = doc.get("email").and_then(serde_json::Value::as_str) {
                println!("  email: {email}");
            }
            println!("  key:   {key}");
            println!();
            println!("This key is shown ONCE and never stored — save it now.");
            println!("On the client machine, point llmux at this server:");
            println!("  llmux.json: {{ \"remote\": {{ \"host\": \"<this-host>\", \"api_key\": \"{key}\" }} }}");
            Ok(())
        }
        KeyCommand::List => {
            let doc = get(&endpoint, "/llmux/keys").await?;
            let empty = Vec::new();
            let keys = doc
                .get("keys")
                .and_then(serde_json::Value::as_array)
                .unwrap_or(&empty);
            if keys.is_empty() {
                println!("No client keys issued.");
                println!("Issue one with: llmux key new --name <label> [--email addr] [--admin]");
                return Ok(());
            }
            for key in keys {
                let suspended = key
                    .get("suspended")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let revoked = key.get("revoked_at_ms").is_some_and(|v| !v.is_null());
                let state = if revoked {
                    "revoked"
                } else if suspended {
                    "suspended"
                } else {
                    "active"
                };
                let email = key
                    .get("email")
                    .and_then(serde_json::Value::as_str)
                    .map(|e| format!(" <{e}>"))
                    .unwrap_or_default();
                println!(
                    "  {}  {}{}  ({}, {}, {}…)",
                    field(key, "id"),
                    field(key, "name"),
                    email,
                    field(key, "kind"),
                    state,
                    field(key, "key_prefix"),
                );
            }
            Ok(())
        }
        KeyCommand::Suspend(sel) => {
            let id = resolve_id(&endpoint, &sel.key).await?;
            let body = serde_json::json!({ "id": id, "suspended": true });
            post(&endpoint, "/llmux/keys/suspend", &body).await?;
            println!("suspended {id} — takes effect on the next request");
            Ok(())
        }
        KeyCommand::Resume(sel) => {
            let id = resolve_id(&endpoint, &sel.key).await?;
            let body = serde_json::json!({ "id": id, "suspended": false });
            post(&endpoint, "/llmux/keys/suspend", &body).await?;
            println!("resumed {id}");
            Ok(())
        }
        KeyCommand::Remove(sel) => {
            let id = resolve_id(&endpoint, &sel.key).await?;
            let body = serde_json::json!({ "id": id });
            post(&endpoint, "/llmux/keys/remove", &body).await?;
            println!("revoked {id} — the key no longer authenticates; its usage history keeps its name/email");
            Ok(())
        }
        KeyCommand::Rotate(sel) => {
            let id = resolve_id(&endpoint, &sel.key).await?;
            let body = serde_json::json!({ "id": id });
            let doc = post(&endpoint, "/llmux/keys/rotate", &body).await?;
            let secret = doc
                .get("key")
                .and_then(|k| k.get("key"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| CliError::Message("daemon response carried no key".into()))?;
            println!("rotated {id} — same attribution id, new secret (shown once):");
            println!("  key: {secret}");
            Ok(())
        }
    }
}

/// Resolve a `<id|name>` selector to a key id: an exact id match wins;
/// otherwise the name must match exactly ONE non-revoked key (0 or 2+ → a
/// loud error — mutations are id-only on the wire).
async fn resolve_id(endpoint: &Endpoint, selector: &str) -> Result<String, CliError> {
    let doc = get(endpoint, "/llmux/keys").await?;
    let empty = Vec::new();
    let keys = doc
        .get("keys")
        .and_then(serde_json::Value::as_array)
        .unwrap_or(&empty);
    if keys
        .iter()
        .any(|k| k.get("id").and_then(serde_json::Value::as_str) == Some(selector))
    {
        return Ok(selector.to_string());
    }
    let matches: Vec<&str> = keys
        .iter()
        .filter(|k| {
            k.get("revoked_at_ms").is_none_or(|v| v.is_null())
                && k.get("name").and_then(serde_json::Value::as_str) == Some(selector)
        })
        .filter_map(|k| k.get("id").and_then(serde_json::Value::as_str))
        .collect();
    match matches.as_slice() {
        [id] => Ok((*id).to_string()),
        [] => Err(CliError::Message(format!(
            "no client key with id or name {selector:?} — see `llmux key list`"
        ))),
        many => Err(CliError::Message(format!(
            "name {selector:?} matches {} keys ({}) — use the id",
            many.len(),
            many.join(", ")
        ))),
    }
}

fn field<'a>(doc: &'a serde_json::Value, name: &str) -> &'a str {
    doc.get(name)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-")
}

fn client() -> Result<reqwest::Client, CliError> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|err| CliError::Message(format!("http client init failed: {err}")))
}

async fn get(endpoint: &Endpoint, path: &str) -> Result<serde_json::Value, CliError> {
    let mut request = client()?.get(format!("{}{path}", endpoint.base_url));
    if let Some(key) = &endpoint.api_key {
        request = request.header("x-api-key", key);
    }
    decode(request.send().await, endpoint, path).await
}

async fn post(
    endpoint: &Endpoint,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, CliError> {
    let mut request = client()?
        .post(format!("{}{path}", endpoint.base_url))
        .json(body);
    if let Some(key) = &endpoint.api_key {
        request = request.header("x-api-key", key);
    }
    decode(request.send().await, endpoint, path).await
}

async fn decode(
    response: Result<reqwest::Response, reqwest::Error>,
    endpoint: &Endpoint,
    path: &str,
) -> Result<serde_json::Value, CliError> {
    let response = response.map_err(|err| {
        if err.is_connect() || err.is_timeout() {
            CliError::Message(format!(
                "no llmux server at {} — start it with `llmux server` or `llmux run`",
                endpoint.base_url
            ))
        } else {
            CliError::Message(format!("request to {path} failed: {err}"))
        }
    })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        // Surface the daemon's own error message when it sent one.
        let detail = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v.pointer("/error/message")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or(body);
        let hint = if status.as_u16() == 401 || status.as_u16() == 403 {
            " (key management needs an admin credential — locally that is proxy.api_key in llmux.json; remotely set remote.api_key to an admin key)"
        } else {
            ""
        };
        return Err(CliError::Message(format!(
            "{path} returned {status}: {detail}{hint}"
        )));
    }
    serde_json::from_str(&body)
        .map_err(|err| CliError::Message(format!("{path} returned unparseable JSON: {err}")))
}
