//! The Codex frontend of `llmux run --codex`: spawn `codex` with
//! session-scoped `-c` provider flags pointing it at the llmux Responses
//! ingress and (unless opted out) a private model catalog file built from the
//! llmux catalog, so the user's own Codex config and auth stay untouched.

use std::convert::Infallible;

use super::{fetch_catalog, row_description, spawn, CatalogRow, Frontend, SessionFile};
use crate::cli::{CliError, Endpoint};

/// Codex owns its model-specific coding prompts. Reuse its local catalog
/// metadata when available and overlay only llmux's model identity/capacity.
/// Only the models array is read; cache identity/account fields never leave it.
fn codex_catalog(models: &[CatalogRow], cached: Option<&serde_json::Value>) -> serde_json::Value {
    use serde_json::json;
    let native = cached.and_then(|v| v["models"].as_array());
    // The template for rows Codex has no entry for (claude, grok, …): a
    // current codex model's, else the first entry. It was keyed on `gpt-5.5`
    // until 2026-11-19, which the live Codex list stopped carrying when the
    // ChatGPT backend retired it (#309), so every such row lost its template.
    let fallback = native.and_then(|a| {
        a.iter()
            .find(|m| m["slug"] == crate::provider::codex::CODEX_MODEL)
            .or_else(|| a.first())
    });
    let mut entries = Vec::new();
    for (priority, row) in models.iter().enumerate() {
        let base_id = row.id.strip_suffix("[1m]").unwrap_or(&row.id);
        let template = native
            .and_then(|a| a.iter().find(|m| m["slug"] == base_id))
            .or(fallback);
        let mut entry=template.cloned().unwrap_or(json!({
            "shell_type":"unified_exec","visibility":"list","supported_in_api":true,"upgrade":null,
            "model_messages":{"instructions_template":"You are Codex, a coding assistant working with the user in their workspace. Read repository instructions before changes. Use the provided tools to inspect the actual state, make focused changes, and verify results. Preserve unrelated user work. Keep tool execution in the client sandbox and obey its permission decisions. Explain outcomes and remaining limitations clearly."},
            "support_verbosity":false,"default_verbosity":null,"apply_patch_tool_type":"freeform",
            "truncation_policy":{"mode":"bytes","limit":10000},"experimental_supported_tools":[],
            "tool_mode":"direct","supports_search_tool":false,"use_responses_lite":false
        }));
        entry["slug"] = json!(row.id);
        entry["display_name"] = json!(row.name);
        entry["description"] = json!(row_description(row));
        entry["priority"] = json!(priority);
        entry["visibility"] = json!("list");
        entry["supported_in_api"] = json!(true);
        entry["context_window"] = json!(row.max_context);
        entry["max_context_window"] = json!(row.max_context);
        entry["supported_reasoning_levels"] = json!(row
            .efforts
            .iter()
            .map(|effort| json!({"effort":effort,"description":effort}))
            .collect::<Vec<_>>());
        entry["default_reasoning_level"] = json!(row
            .efforts
            .iter()
            .find(|e| e.as_str() == "medium")
            .or(row.efforts.first()));
        if row.group != "codex" {
            entry["additional_speed_tiers"] = json!([]);
            entry["service_tiers"] = json!([]);
            entry["default_service_tier"] = serde_json::Value::Null;
            entry["tool_mode"] = json!("direct");
            entry["supports_search_tool"] = json!(false);
            entry["use_responses_lite"] = json!(false);
            entry["supports_reasoning_effort_updates"] = json!(false);
            entry["supports_experimental_context"] = json!(false);
        }
        entries.push(entry.clone());
        for alias in &row.aliases {
            let mut alias_entry = entry.clone();
            alias_entry["slug"] = json!(alias);
            alias_entry["visibility"] = json!("hide");
            entries.push(alias_entry);
        }
    }
    json!({"models":entries})
}

fn user_codex_catalog(args: &[String]) -> bool {
    args.iter().any(|a| a.contains("model_catalog_json="))
}

async fn codex_catalog_file(
    endpoint: &Endpoint,
    args: &[String],
    no_picker: bool,
) -> Option<SessionFile> {
    if no_picker || user_codex_catalog(args) {
        return None;
    }
    let rows = match fetch_catalog(&endpoint.base_url, endpoint.api_key.as_deref()).await {
        Ok(rows) if !rows.is_empty() => rows,
        Ok(_) => return None,
        Err(reason) => {
            eprintln!("warning: model picker not injected: {reason}");
            return None;
        }
    };
    let cache_path = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".codex")))
        .map(|p| p.join("models_cache.json"));
    let cached = cache_path
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let catalog = codex_catalog(&rows, cached.as_ref()).to_string();
    match SessionFile::create("llmux-codex-models", catalog.as_bytes()) {
        Ok(file) => Some(file),
        Err(_) => {
            eprintln!("warning: could not create Codex model catalog");
            None
        }
    }
}

/// Session-scoped provider flags keep the user's Codex config/auth untouched.
pub(super) async fn launch(
    endpoint: &Endpoint,
    args: &[String],
    no_picker: bool,
    default_model: &str,
) -> Result<Infallible, CliError> {
    let mut command = tokio::process::Command::new(Frontend::Codex.binary());
    // clap's subcommand config list replaces its parent's list. Put the
    // provider flags in the actual client command's argument scope.
    let args = if args.first().is_some_and(|a| {
        matches!(
            a.as_str(),
            "exec" | "e" | "review" | "resume" | "fork" | "app-server" | "mcp-server"
        )
    }) {
        command.arg(&args[0]);
        &args[1..]
    } else {
        args
    };

    let q = |value: &str| serde_json::Value::String(value.to_string()).to_string();
    let settings = [
        "model_provider=\"llmux\"".to_string(),
        format!("model={}", q(default_model)),
        "model_providers.llmux.name=\"llmux\"".into(),
        format!(
            "model_providers.llmux.base_url={}",
            q(&format!("{}/v1", endpoint.base_url.trim_end_matches('/')))
        ),
        "model_providers.llmux.wire_api=\"responses\"".into(),
        "model_providers.llmux.requires_openai_auth=false".into(),
        "model_providers.llmux.supports_websockets=false".into(),
    ];
    for setting in settings {
        command.arg("-c").arg(setting);
    }
    let catalog = codex_catalog_file(endpoint, args, no_picker).await;
    if let Some(file) = &catalog {
        command.arg("-c").arg(format!(
            "model_catalog_json={}",
            q(&file.path().to_string_lossy())
        ));
    }
    // This key belongs only to the selected llmux endpoint.
    if let Some(key) = &endpoint.api_key {
        command.env("LLMUX_CODEX_API_KEY", key);
        command
            .arg("-c")
            .arg("model_providers.llmux.env_key=\"LLMUX_CODEX_API_KEY\"");
    } else {
        command.env_remove("LLMUX_CODEX_API_KEY");
    }
    command.args(args);
    spawn(command, Frontend::Codex, catalog.into_iter().collect()).await
}

#[cfg(test)]
mod tests {
    use super::super::tests::row;
    use super::*;

    /// #309: rows Codex has no entry for take a CURRENT codex model's
    /// template (the default pin's), not the retired `gpt-5.5`'s, and fall back
    /// to the first entry when the pin is missing too.
    #[test]
    fn codex_catalog_template_follows_a_live_codex_model() {
        let rows = vec![row(
            "claude-opus-5-5[1m]",
            "Opus",
            &[],
            Some(1_000_000),
            "claude",
        )];
        let cached = serde_json::json!({"models": [
            {"slug": "gpt-6.2-sol", "base_instructions": "first"},
            {"slug": crate::provider::codex::CODEX_MODEL, "base_instructions": "pin"}
        ]});
        let out = codex_catalog(&rows, Some(&cached));
        assert_eq!(out["models"][0]["base_instructions"], "pin");
        let cached =
            serde_json::json!({"models": [{"slug": "gpt-6.2-sol", "base_instructions": "first"}]});
        let out = codex_catalog(&rows, Some(&cached));
        assert_eq!(out["models"][0]["base_instructions"], "first");
    }

    /// The Codex picker over the REAL catalog (2026-11-21): every Gemini 4
    /// thinking level is its own slug (`gemini-4-neon-xhigh`, Antigravity's
    /// own picker shape) with an empty reasoning menu, like the 3.8 Flash
    /// rows; an alias (`neon`, …) rides as a hidden entry for its `-high`
    /// row.
    #[test]
    fn codex_catalog_offers_gemini_4_levels_from_the_real_catalog() {
        let entries = crate::catalog::catalog(
            "grok-4.7",
            "gpt-6.2-sol",
            crate::catalog::OPENROUTER_DEFAULT_PIN,
        );
        let doc = serde_json::json!({ "models": entries }).to_string();
        let rows = serde_json::from_str::<super::super::CatalogResponse>(&doc)
            .expect("the served catalog parses as CLI catalog rows")
            .models;
        let out = codex_catalog(&rows, None);
        let models = out["models"].as_array().expect("models");
        let by_slug = |slug: &str| {
            models
                .iter()
                .find(|m| m["slug"] == slug)
                .unwrap_or_else(|| panic!("{slug} in the codex catalog"))
        };
        let levels = |slug: &str| -> Vec<String> {
            by_slug(slug)["supported_reasoning_levels"]
                .as_array()
                .expect("levels")
                .iter()
                .map(|l| l["effort"].as_str().expect("effort").to_string())
                .collect()
        };
        // One slug per level id, in Antigravity's order, each with an empty
        // menu: the level is the model, as on the 3.8 Flash rows.
        let gemini_4: Vec<&str> = models
            .iter()
            .filter(|m| m["visibility"] == "list")
            .filter_map(|m| m["slug"].as_str())
            .filter(|slug| slug.starts_with("gemini-4-"))
            .collect();
        assert_eq!(
            gemini_4,
            [
                "gemini-4-neon-high",
                "gemini-4-neon-xhigh",
                "gemini-4-neon-medium",
                "gemini-4-neon-low",
                "gemini-4-hydrogen-high",
                "gemini-4-hydrogen-xhigh",
                "gemini-4-hydrogen-medium",
                "gemini-4-hydrogen-low",
                "gemini-4-argon-high",
                "gemini-4-argon-medium",
                "gemini-4-argon-low",
                "gemini-4-carbon-high",
                "gemini-4-carbon-medium",
                "gemini-4-carbon-low",
            ]
        );
        for slug in &gemini_4 {
            assert!(levels(slug).is_empty(), "{slug}: the level is in the id");
        }
        assert_eq!(by_slug("gemini-4-neon-xhigh")["context_window"], 524_288);
        assert_eq!(by_slug("gemini-4-argon-low")["context_window"], 1_048_576);
        assert_eq!(
            by_slug("gemini-4-neon-high")["display_name"],
            "Gemini 4 Neon (High)"
        );
        assert!(levels("gemini-3.8-flash-high[1m]").is_empty());
        for (alias, owner) in [
            ("neon", "gemini-4-neon-high"),
            ("argon", "gemini-4-argon-high"),
            ("carbon", "gemini-4-carbon-high"),
            ("hydrogen", "gemini-4-hydrogen-high"),
        ] {
            let entry = by_slug(alias);
            assert_eq!(entry["visibility"], "hide", "{alias}");
            assert_eq!(entry["display_name"], by_slug(owner)["display_name"]);
            assert!(levels(alias).is_empty(), "{alias}");
        }
    }
}
