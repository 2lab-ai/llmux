//! Load/save of `~/.config/llmux.json` — atomic replacement, 0600 permissions,
//! and reload-before-mutation updates. Atomic replacement prevents torn files;
//! it does not serialize overlapping cross-process writers.

pub mod migrate;
pub mod schema;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub use schema::{
    default_domain_abbrev, default_fable_weekly_max, AccountConfig, AccountCredential,
    AccountLimits, CodexConfig, Config, EventBanner, IdleProbeConfig, ProxyConfig, QuotaDisplay,
    RawIoConfig, RemoteConfig, RoutingConfig, SchedulerConfig, SchedulerMode, Upsert,
    DEFAULT_CODEX_TOKEN_URL, DEFAULT_MAX_REQUEST_BYTES, DEFAULT_PORT, DEFAULT_UPSTREAM,
};

/// Environment variable overriding the config file location.
pub const CONFIG_ENV: &str = "LLMUX_CONFIG";

/// Prefix of auto-generated proxy api keys.
const API_KEY_PREFIX: &str = "lm-";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("config parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("unsupported config version {0}")]
    UnsupportedVersion(u32),
    #[error("could not determine config directory")]
    NoConfigDir,
    #[error("invalid import data: {0}")]
    Invalid(String),
}

fn io_err(path: &Path, source: std::io::Error) -> ConfigError {
    ConfigError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Resolve the config path: `$LLMUX_CONFIG` if set, else
/// `$XDG_CONFIG_HOME/llmux.json`, else `~/.config/llmux.json`.
///
/// Deliberately NOT `dirs::config_dir()`: on macOS that is
/// `~/Library/Application Support`, but the contract (FR2, teamclaude
/// compatibility) is `~/.config` on every Unix platform.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    if let Some(path) = std::env::var_os(CONFIG_ENV) {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    xdg_config_dir()
        .map(|dir| dir.join("llmux.json"))
        .ok_or(ConfigError::NoConfigDir)
}

/// `$XDG_CONFIG_HOME` when set and non-empty, else `~/.config`.
pub(crate) fn xdg_config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg));
        }
    }
    dirs::home_dir().map(|home| home.join(".config"))
}

/// One-time `teamagent` → `llmux` config adoption. When the config is resolved
/// from the DEFAULT location (no `$LLMUX_CONFIG` override) and `llmux.json` does
/// not yet exist but the previous tool's `teamagent.json` sits beside it, copy
/// it across so a renamed install keeps its accounts. The original is left in
/// place as a fallback — copy, never move.
///
/// TODO(remove after public uptake): drop once installs have migrated.
fn adopt_legacy_config_if_needed() -> Result<(), ConfigError> {
    if std::env::var_os(CONFIG_ENV).is_some_and(|v| !v.is_empty()) {
        return Ok(()); // explicit override path: never adopt implicitly
    }
    match xdg_config_dir() {
        Some(dir) => adopt_legacy_in_dir(&dir),
        None => Ok(()),
    }
}

/// Byte-for-byte copy `teamagent.json` → `llmux.json` in `dir`, but only when
/// the new file is absent and the legacy one is present. Idempotent.
fn adopt_legacy_in_dir(dir: &Path) -> Result<(), ConfigError> {
    let new = dir.join("llmux.json");
    let legacy = dir.join("teamagent.json");
    if new.exists() || !legacy.exists() {
        return Ok(());
    }
    let raw = fs::read(&legacy).map_err(|e| io_err(&legacy, e))?;
    fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let tmp = dir.join(format!(
        ".llmux.json.tmp.{}.{}",
        std::process::id(),
        ulid::Ulid::new()
    ));
    if let Err(err) = write_tmp_and_rename(&tmp, &new, &raw) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    tracing::info!(
        from = %legacy.display(),
        to = %new.display(),
        "adopted legacy teamagent.json into llmux.json"
    );
    Ok(())
}

/// Load the config from [`config_path`]. A missing file yields
/// `Config::default()` (first run); nothing is written — use
/// [`load_or_init`] to also create the file with a fresh api key.
pub fn load() -> Result<Config, ConfigError> {
    adopt_legacy_config_if_needed()?;
    load_path(&config_path()?)
}

/// [`load`] against an explicit path.
pub fn load_path(path: &Path) -> Result<Config, ConfigError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(err) => return Err(io_err(path, err)),
    };
    let mut config: Config = serde_json::from_str(&raw)?;
    if config.version != 1 {
        return Err(ConfigError::UnsupportedVersion(config.version));
    }
    // Idle-probe always-on migration (#45). Pre-#45 builds serialized the OLD
    // conservative default triple — enabled=false, cooldown=3600, sweep=0 — for
    // every user who never touched the block, so a live config carrying EXACTLY
    // that triple is indistinguishable from "unset". Treat it as unset and adopt
    // the new always-on defaults, otherwise the stale serialized value would
    // pin probing off forever. Any OTHER combination is an operator's explicit
    // choice and is kept verbatim: an operator opting out post-upgrade sets
    // enabled=false with a non-default cooldown or a non-zero sweep, which no
    // longer matches this triple.
    const LEGACY_IDLE_PROBE_DEFAULT: IdleProbeConfig = IdleProbeConfig {
        enabled: false,
        per_account_cooldown_secs: 3600,
        sweep_secs: 0,
        // Field postdates the pre-#45 era; absent from old files, it always
        // deserializes to its serde default — include that value here so the
        // legacy triple still reads as "unset".
        stale_after_secs: 900,
    };
    // Same migration for the #45-era always-on default (enabled, 3600, 3600):
    // users who never touched the block carry exactly it, and would otherwise
    // stay pinned to the hourly cadence instead of the 15-min cold-refresh
    // defaults (Z 2026-07-15).
    const LEGACY_IDLE_PROBE_HOURLY_DEFAULT: IdleProbeConfig = IdleProbeConfig {
        enabled: true,
        per_account_cooldown_secs: 3600,
        sweep_secs: 3600,
        stale_after_secs: 900,
    };
    if config.proxy.idle_probe == LEGACY_IDLE_PROBE_DEFAULT
        || config.proxy.idle_probe == LEGACY_IDLE_PROBE_HOURLY_DEFAULT
    {
        config.proxy.idle_probe = IdleProbeConfig::default();
    }
    // Demo mode: swap account identities for stable fakes at the source so every
    // surface (dashboard, logs, status) shows the alias. Credentials are keyed
    // by token/uuid, not name, so they keep working.
    if crate::demo::enabled() {
        for account in &mut config.accounts {
            account.name = crate::demo::alias(&account.name);
        }
    }
    Ok(config)
}

/// Load the config, creating it on first run: when the file does not exist,
/// a default config with a freshly generated proxy api key is written
/// (mode 0600) and returned.
pub fn load_or_init() -> Result<Config, ConfigError> {
    adopt_legacy_config_if_needed()?;
    load_or_init_path(&config_path()?)
}

/// [`load_or_init`] against an explicit path.
pub fn load_or_init_path(path: &Path) -> Result<Config, ConfigError> {
    if path.exists() {
        return load_path(path);
    }
    let mut config = Config::default();
    config.proxy.api_key = Some(generate_api_key());
    save_path(path, &config)?;
    tracing::info!(path = %path.display(), "created config");
    Ok(config)
}

/// Atomically persist `config` (write temp file mode 0600 in the same
/// directory, fsync, then rename over the target).
pub fn save(config: &Config) -> Result<(), ConfigError> {
    save_path(&config_path()?, config)
}

/// [`save`] against an explicit path.
pub fn save_path(path: &Path, config: &Config) -> Result<(), ConfigError> {
    // Demo mode loads aliased account names; never let those reach disk.
    if crate::demo::enabled() {
        tracing::debug!("LLMUX_DEMO_MODE: config save suppressed");
        return Ok(());
    }
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => {
            fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
            dir.to_path_buf()
        }
        _ => PathBuf::from("."),
    };

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("llmux.json");
    let tmp = dir.join(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        ulid::Ulid::new()
    ));

    let mut data = serde_json::to_vec_pretty(config)?;
    data.push(b'\n');

    let result = write_tmp_and_rename(&tmp, path, &data);
    if result.is_err() {
        // Best-effort cleanup of the orphaned temp file.
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_tmp_and_rename(tmp: &Path, path: &Path, data: &[u8]) -> Result<(), ConfigError> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(tmp).map_err(|e| io_err(tmp, e))?;
    file.write_all(data).map_err(|e| io_err(tmp, e))?;
    file.sync_all().map_err(|e| io_err(tmp, e))?;
    drop(file);

    fs::rename(tmp, path).map_err(|e| io_err(path, e))?;

    // Best-effort directory fsync so the rename itself is durable.
    if let Some(dir) = path.parent() {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Reload-before-mutation update: reads the file immediately before applying
/// `mutate`, then saves by atomic replacement. Prefer this over editing a
/// long-lived snapshot because it narrows the stale-state window and prevents
/// partial files.
///
/// This is not a cross-process transaction: there is no writer lock or
/// compare-and-swap between the read and rename. Overlapping callers can both
/// read the same version and the later rename can overwrite the earlier
/// mutation. Callers should therefore avoid simultaneous config writes.
pub fn update<F>(mutate: F) -> Result<Config, ConfigError>
where
    F: FnOnce(&mut Config),
{
    update_path(&config_path()?, mutate)
}

/// [`update`] against an explicit path.
pub fn update_path<F>(path: &Path, mutate: F) -> Result<Config, ConfigError>
where
    F: FnOnce(&mut Config),
{
    let mut config = load_path(path)?;
    mutate(&mut config);
    save_path(path, &config)?;
    Ok(config)
}

/// Generate a proxy API key: `lm-` + 32 random bytes, base64url (no pad).
pub fn generate_api_key() -> String {
    use base64::Engine as _;
    format!(
        "{API_KEY_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes_32())
    )
}

/// 32 bytes of entropy. `/dev/urandom` on Unix (the only tier-1 targets are
/// macOS/Linux); falls back to hashing rand-backed ULIDs + time + pid, which
/// still carries ≥256 bits of entropy through SHA-256.
fn random_bytes_32() -> [u8; 32] {
    #[cfg(unix)]
    {
        use std::io::Read as _;
        if let Ok(mut f) = fs::File::open("/dev/urandom") {
            let mut buf = [0u8; 32];
            if f.read_exact(&mut buf).is_ok() {
                return buf;
            }
        }
    }
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    for _ in 0..4 {
        hasher.update(ulid::Ulid::new().to_bytes());
    }
    if let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.update(elapsed.as_nanos().to_le_bytes());
    }
    hasher.update(std::process::id().to_le_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Self-cleaning unique temp dir (no tempfile dev-dependency).
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "llmux-test-{}-{}",
                std::process::id(),
                ulid::Ulid::new()
            ));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn grok_account_fixture(name: &str, subject: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Grok {
                subject: subject.to_string(),
                access_token: "at-g".to_string(),
                refresh_token: "rt-g".to_string(),
                expires_at_ms: 1_900_000_000_000,
                token_endpoint: "https://auth.x.ai/token".to_string(),
                last_refresh_ms: Some(1_750_000_000_000),
            },
        }
    }

    // ---- C7: grok credential serde round-trip + upsert dedup ----
    #[test]
    fn c7_grok_credential_round_trips_and_dedups() {
        let account = grok_account_fixture("grok:a@b.c", "sub-1");
        let json = serde_json::to_string(&account).expect("serialize");
        assert!(json.contains(r#""type":"grok""#), "{json}");
        let back: AccountConfig = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, account);

        let mut config = Config::default();
        assert_eq!(config.upsert_account(account.clone()), Upsert::Added);
        // Same name → update, not duplicate.
        assert_eq!(config.upsert_account(account), Upsert::Updated);
        assert_eq!(config.accounts.len(), 1);
        assert_eq!(config.accounts[0].credential.kind(), "grok");
        assert_eq!(config.accounts[0].credential.account_uuid(), Some("sub-1"));
    }

    // ---- C17: pre-grok config parses and round-trips VALUE-stable ----
    // (Not byte-stable: the new binary serializes the default `grok` section;
    // old binaries ignore unknown sections. The additive guarantee is
    // semantic — no field lost, no meaning changed.)
    #[test]
    fn c17_pre_grok_config_round_trips_without_grok_fields() {
        // A config written before grok existed: no `grok` section, no
        // `routing.grok_models`, codex + oauth accounts only.
        let raw = r#"{
  "version": 1,
  "routing": { "enabled": true, "default_group": "claude" },
  "accounts": [
    { "name": "a", "type": "apikey", "api_key": "sk-1" }
  ]
}"#;
        let config: Config = serde_json::from_str(raw).expect("pre-grok config parses");
        assert_eq!(config.grok.default_model, "grok-4.5", "defaults fill in");
        assert!(config.routing.grok_models.is_empty());
        // Round-trip: serialize → parse → identical value (additive-only).
        let round: Config =
            serde_json::from_str(&serde_json::to_string(&config).expect("serialize"))
                .expect("round trip");
        assert_eq!(round, config);
    }

    pub(crate) fn oauth_account(name: &str, uuid: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Oauth {
                account_uuid: uuid.to_string(),
                access_token: format!("at-{name}"),
                refresh_token: format!("rt-{name}"),
                expires_at_ms: 1_750_000_000_000,
                tier: None,
                last_refresh_ms: None,
            },
        }
    }

    fn apikey_account(name: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Apikey {
                api_key: format!("sk-ant-api03-{name}"),
            },
        }
    }

    fn codex_account(name: &str, account_id: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            credential: AccountCredential::Codex {
                account_id: account_id.to_string(),
                access_token: format!("at-{name}"),
                refresh_token: format!("rt-{name}"),
                expires_at_ms: 1_750_000_000_000,
                last_refresh_ms: None,
            },
        }
    }

    #[test]
    fn missing_file_loads_default() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let config = load_path(&path).expect("load");
        assert_eq!(config, Config::default());
        assert!(!path.exists(), "plain load must not create the file");
    }

    #[test]
    fn load_or_init_creates_file_with_api_key_and_0600() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let config = load_or_init_path(&path).expect("init");

        let key = config.proxy.api_key.as_deref().expect("api key generated");
        assert!(key.starts_with("lm-"), "prefix: {key}");
        // 32 bytes -> 43 base64url chars, no padding.
        assert_eq!(key.len(), 3 + 43, "key length: {key}");
        assert!(path.exists());

        // Second init must NOT regenerate the key.
        let again = load_or_init_path(&path).expect("reload");
        assert_eq!(again.proxy.api_key, config.proxy.api_key);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        }
    }

    #[test]
    fn partial_file_fills_defaults() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        fs::write(&path, r#"{ "proxy": { "port": 9999 } }"#).expect("write");

        let config = load_path(&path).expect("load");
        assert_eq!(config.version, 1);
        assert_eq!(config.proxy.port, 9999);
        assert_eq!(config.proxy.api_key, None);
        // Additive: a proxy block without `forward_idle_timeout_secs` (every
        // config written before issue #29) loads the 120s default.
        assert_eq!(config.proxy.forward_idle_timeout_secs, 120);
        assert_eq!(config.upstream, schema::DEFAULT_UPSTREAM);
        assert!((config.scheduler.five_hour_max - 0.90).abs() < f64::EPSILON);
        assert!((config.scheduler.seven_day_max - 0.99).abs() < f64::EPSILON);
        assert_eq!(config.scheduler.usage_poll_secs, 300);
        assert_eq!(config.scheduler.usage_max_age_secs, 600);
        assert_eq!(config.scheduler.refresh_ahead_secs, 7 * 3600);
        assert!(config.accounts.is_empty());
    }

    #[test]
    fn adopts_legacy_teamagent_config_as_byte_copy() {
        let dir = TempDir::new();
        let legacy = dir.path().join("teamagent.json");
        let new = dir.path().join("llmux.json");
        let body = r#"{ "version": 1, "proxy": { "port": 9999, "api_key": "ta-keep-me" } }"#;
        fs::write(&legacy, body).expect("write legacy");

        adopt_legacy_in_dir(dir.path()).expect("adopt");

        assert!(new.exists(), "llmux.json created");
        assert!(legacy.exists(), "legacy left in place (copy, not move)");
        assert_eq!(
            fs::read(&new).expect("read new"),
            body.as_bytes(),
            "byte-for-byte copy preserves the stored api key"
        );
        let config = load_path(&new).expect("load adopted");
        assert_eq!(config.proxy.port, 9999);
        assert_eq!(config.proxy.api_key.as_deref(), Some("ta-keep-me"));
    }

    #[test]
    fn adopt_is_idempotent_and_never_overwrites() {
        let dir = TempDir::new();
        let legacy = dir.path().join("teamagent.json");
        let new = dir.path().join("llmux.json");
        fs::write(&legacy, r#"{ "version": 1, "proxy": { "port": 1111 } }"#).expect("legacy");
        fs::write(&new, r#"{ "version": 1, "proxy": { "port": 2222 } }"#).expect("new");

        // llmux.json already exists → adoption is a no-op, must not clobber it.
        adopt_legacy_in_dir(dir.path()).expect("adopt");
        assert_eq!(load_path(&new).expect("load").proxy.port, 2222);
    }

    #[test]
    fn adopt_is_noop_without_a_legacy_file() {
        let dir = TempDir::new();
        adopt_legacy_in_dir(dir.path()).expect("adopt");
        assert!(!dir.path().join("llmux.json").exists());
    }

    #[test]
    fn round_trip_preserves_all_fields() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");

        let mut config = Config::default();
        config.proxy.port = 4000;
        config.proxy.api_key = Some("lm-test".into());
        config.upstream = "https://example.test".into();
        config.scheduler.five_hour_max = 0.5;
        config.email_anonymous = true;
        config.accounts.push(oauth_account("a@x.com", "uuid-a"));
        config.accounts.push(apikey_account("api-1"));
        config.accounts.push(codex_account("cx@x.com", "acct-cx"));
        config.codex.upstream = "https://codex.test/backend".into();
        config.codex.token_url = "https://codex.test/oauth/token".into();

        save_path(&path, &config).expect("save");
        let loaded = load_path(&path).expect("load");
        assert_eq!(loaded, config);
    }

    #[test]
    fn codex_credential_serializes_with_type_codex_and_defaults_apply() {
        let json = serde_json::to_value(codex_account("cx", "acct-1")).expect("json");
        assert_eq!(json["type"], "codex");
        assert_eq!(json["account_id"], "acct-1");

        // A config without a codex section gets the production defaults.
        let config: Config = serde_json::from_str(r#"{ "version": 1 }"#).expect("parse");
        assert_eq!(config.codex.upstream, schema::DEFAULT_CODEX_UPSTREAM);
        assert_eq!(config.codex.token_url, schema::DEFAULT_CODEX_TOKEN_URL);
    }

    #[test]
    fn routing_config_is_additive_and_defaults_to_enabled() {
        // A config written before routing existed (no `routing` key) loads
        // with routing ON — model→group routing is now the default so that
        // `gpt-5.5` reaches a codex account instead of being forwarded
        // verbatim to Anthropic. The other fields keep their safe defaults
        // (default_group=claude, on_empty_group=error ⇒ a missing group 404s
        // cleanly rather than misrouting).
        let config: Config =
            serde_json::from_str(r#"{ "version": 1 }"#).expect("old config parses");
        assert!(config.routing.enabled, "routing defaults to enabled");
        assert_eq!(config.routing.default_group, "claude");
        assert_eq!(config.routing.on_empty_group, "error");
        assert!(config.routing.claude_models.is_empty());
        assert!(config.routing.codex_models.is_empty());

        // An explicit routing block round-trips through save→load.
        let raw = r#"{
            "version": 1,
            "routing": {
                "enabled": true,
                "codex_models": ["gpt-", "~codex"],
                "default_group": "codex",
                "on_empty_group": "fallback"
            }
        }"#;
        let config: Config = serde_json::from_str(raw).expect("routing config parses");
        assert!(config.routing.enabled);
        assert_eq!(config.routing.codex_models, vec!["gpt-", "~codex"]);
        assert_eq!(config.routing.default_group, "codex");
        assert_eq!(config.routing.on_empty_group, "fallback");
        let reparsed: Config =
            serde_json::from_str(&serde_json::to_string(&config).expect("serialize"))
                .expect("re-parse");
        assert_eq!(reparsed.routing, config.routing);
    }

    #[test]
    fn raw_io_config_is_additive_with_a_configurable_max_body_bytes() {
        // A config written before `max_body_bytes` existed (no key, or no
        // `raw_io` block at all) loads with the 8 MiB default — decoupled from
        // the 8 KiB debug body-log cap, so full streamed responses are retained.
        let config: Config =
            serde_json::from_str(r#"{ "version": 1 }"#).expect("old config parses");
        assert!(config.raw_io.enabled, "capture defaults on");
        assert_eq!(config.raw_io.retention_days, 90);
        assert_eq!(
            config.raw_io.max_body_bytes,
            crate::proxy::raw_io::RESPONSE_CAP_BYTES,
            "max_body_bytes defaults to RESPONSE_CAP_BYTES (8 MiB), not the 8 KiB debug cap"
        );

        // An explicit max_body_bytes override is respected and round-trips.
        let raw = r#"{
            "version": 1,
            "raw_io": { "enabled": true, "retention_days": 30, "max_body_bytes": 1048576 }
        }"#;
        let config: Config = serde_json::from_str(raw).expect("raw_io config parses");
        assert_eq!(config.raw_io.max_body_bytes, 1_048_576);
        assert_eq!(config.raw_io.retention_days, 30);
        let reparsed: Config =
            serde_json::from_str(&serde_json::to_string(&config).expect("serialize"))
                .expect("re-parse");
        assert_eq!(reparsed.raw_io, config.raw_io);
    }

    #[test]
    fn pricing_overrides_are_additive_and_parse_per_model() {
        // A config written before Feature D (no `pricing` key) loads with an
        // empty override map — the built-in default rate table is used.
        let config: Config =
            serde_json::from_str(r#"{ "version": 1 }"#).expect("old config parses");
        assert!(config.pricing.is_empty());

        // An explicit pricing block round-trips and is keyed by model slug.
        let raw = r#"{
            "version": 1,
            "pricing": {
                "gpt-5.5": { "input": 9.99, "output": 0.0, "cache_read": 0.0, "cache_creation": 0.0 }
            }
        }"#;
        let config: Config = serde_json::from_str(raw).expect("pricing config parses");
        let price = config.pricing.get("gpt-5.5").expect("override present");
        assert_eq!(price.input, 9.99);
        let reparsed: Config =
            serde_json::from_str(&serde_json::to_string(&config).expect("serialize"))
                .expect("re-parse");
        assert_eq!(reparsed.pricing, config.pricing);
    }

    #[test]
    fn email_anonymous_is_additive_and_round_trips() {
        // A config written before the field existed (no `email_anonymous` key)
        // loads with masking OFF — old configs load unchanged (SSOT E1).
        let config: Config =
            serde_json::from_str(r#"{ "version": 1 }"#).expect("old config parses");
        assert!(!config.email_anonymous, "defaults to false");

        // An explicit value round-trips through save→load.
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let config = Config {
            email_anonymous: true,
            ..Default::default()
        };
        save_path(&path, &config).expect("save");
        let loaded = load_path(&path).expect("load");
        assert!(loaded.email_anonymous, "explicit true persists");

        // The setter's read-merge-write shape: update() flips ONLY this field
        // and preserves the rest of the fresh on-disk state.
        update_path(&path, |c| c.email_anonymous = false).expect("update");
        assert!(!load_path(&path).expect("reload").email_anonymous);
    }

    #[test]
    fn event_banners_are_additive_and_parse_when_present() {
        // A config written before the `events` block existed — and one still
        // carrying the removed singular `event` key — both load with an empty
        // list (the orphan key is ignored, nothing reserved on screen).
        let old: Config = serde_json::from_str(r#"{ "version": 1 }"#).expect("old config parses");
        assert!(old.events.is_empty(), "absent block → empty");
        let orphan: Config =
            serde_json::from_str(r#"{ "version": 1, "event": { "label": "x", "until": "y" } }"#)
                .expect("orphan event key ignored");
        assert!(orphan.events.is_empty(), "removed singular key dropped");

        // A present list parses each entry verbatim (accepting the compact form).
        let raw = r#"{
            "version": 1,
            "events": [
                { "id": "20260712-fable5", "from": "202607080000", "to": "202607130000",
                  "content": "Fable 5 Available until 7/12" }
            ]
        }"#;
        let config: Config = serde_json::from_str(raw).expect("events config parses");
        assert_eq!(config.events.len(), 1);
        let event = &config.events[0];
        assert_eq!(event.id, "20260712-fable5");
        assert_eq!(event.from, "202607080000");
        assert_eq!(event.to, "202607130000");
        assert_eq!(event.content, "Fable 5 Available until 7/12");

        // An empty list is omitted on write (byte-compatible until one is set)
        // and a populated list round-trips.
        assert!(
            serde_json::to_value(&old)
                .expect("json")
                .get("events")
                .is_none(),
            "empty list omitted on write"
        );
        let round: Config =
            serde_json::from_str(&serde_json::to_string(&config).expect("serialize"))
                .expect("re-parse");
        assert_eq!(round.events, config.events);
    }

    #[test]
    fn codex_accounts_dedup_by_account_id_and_update_tokens() {
        let mut config = Config::default();
        config.accounts.push(codex_account("codex-old", "acct-1"));

        // Re-import with the same account_id replaces, never duplicates.
        let outcome = config.upsert_account(codex_account("cx@x.com", "acct-1"));
        assert_eq!(outcome, Upsert::Updated);
        assert_eq!(config.accounts.len(), 1);
        assert_eq!(config.accounts[0].name, "cx@x.com");

        // Refreshed codex tokens persist through the shared updater.
        assert!(config.update_oauth_tokens("acct-1", "at-new", Some("rt-new"), 99, 77));
        match &config.accounts[0].credential {
            AccountCredential::Codex {
                access_token,
                refresh_token,
                expires_at_ms,
                last_refresh_ms,
                ..
            } => {
                assert_eq!(access_token, "at-new");
                assert_eq!(refresh_token, "rt-new");
                assert_eq!(*expires_at_ms, 99);
                assert_eq!(*last_refresh_ms, Some(77), "refresh stamps the timestamp");
            }
            other => panic!("unexpected credential {other:?}"),
        }
    }

    #[test]
    fn last_refresh_ms_is_additive_and_round_trips() {
        // Pre-upgrade config (no last_refresh_ms anywhere) loads unchanged.
        let raw = r#"{
            "version": 1,
            "accounts": [
                { "name": "a@x.com", "type": "oauth", "account_uuid": "uuid-a",
                  "access_token": "at", "refresh_token": "rt", "expires_at_ms": 42 },
                { "name": "cx", "type": "codex", "account_id": "acct-1",
                  "access_token": "at", "refresh_token": "rt", "expires_at_ms": 42 }
            ]
        }"#;
        let config: Config = serde_json::from_str(raw).expect("old config parses");
        assert_eq!(config.accounts[0].credential.last_refresh_ms(), None);
        assert_eq!(config.accounts[1].credential.last_refresh_ms(), None);

        // None is omitted on write (the file stays byte-compatible until
        // the first refresh actually happens).
        let json = serde_json::to_value(&config.accounts[0]).expect("json");
        assert!(
            json.get("last_refresh_ms").is_none(),
            "None omitted: {json}"
        );

        // A stamped refresh round-trips through save/load.
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        let mut config = config;
        assert!(config.update_oauth_tokens("uuid-a", "at-new", None, 99, 88));
        save_path(&path, &config).expect("save");
        let loaded = load_path(&path).expect("load");
        assert_eq!(loaded.accounts[0].credential.last_refresh_ms(), Some(88));
        assert_eq!(loaded, config);
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        fs::write(&path, r#"{ "version": 2 }"#).expect("write");
        match load_path(&path) {
            Err(ConfigError::UnsupportedVersion(2)) => {}
            other => panic!("expected UnsupportedVersion(2), got {other:?}"),
        }
    }

    #[test]
    fn update_two_writers_preserve_each_others_accounts() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        save_path(&path, &Config::default()).expect("seed");

        // Both "processes" hold the same stale snapshot, then write
        // through update(): each upsert is re-applied to fresh disk state,
        // so neither write clobbers the other.
        let _stale_a = load_path(&path).expect("stale a");
        let _stale_b = load_path(&path).expect("stale b");

        update_path(&path, |c| {
            c.upsert_account(oauth_account("a@x.com", "uuid-a"));
        })
        .expect("writer a");
        update_path(&path, |c| {
            c.upsert_account(oauth_account("b@x.com", "uuid-b"));
        })
        .expect("writer b");

        let merged = load_path(&path).expect("load");
        let names: Vec<_> = merged.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["a@x.com", "b@x.com"]);
    }

    #[test]
    fn upsert_matches_uuid_over_name() {
        let mut config = Config::default();
        config.accounts.push(oauth_account("old-name", "uuid-a"));
        config.accounts.push(apikey_account("api-1"));

        // Same uuid, new name -> replaces in place (re-login rename).
        let outcome = config.upsert_account(oauth_account("new@x.com", "uuid-a"));
        assert_eq!(outcome, Upsert::Updated);
        assert_eq!(config.accounts.len(), 2);
        assert_eq!(config.accounts[0].name, "new@x.com");

        // Unknown uuid, unknown name -> appended.
        let outcome = config.upsert_account(oauth_account("c@x.com", "uuid-c"));
        assert_eq!(outcome, Upsert::Added);
        assert_eq!(config.accounts.len(), 3);

        // No uuid -> falls back to name match.
        let outcome = config.upsert_account(apikey_account("api-1"));
        assert_eq!(outcome, Upsert::Updated);
        assert_eq!(config.accounts.len(), 3);
    }

    #[test]
    fn update_oauth_tokens_preserves_refresh_on_none() {
        let mut config = Config::default();
        config.accounts.push(oauth_account("a@x.com", "uuid-a"));

        assert!(config.update_oauth_tokens("uuid-a", "at-new", None, 42, 41));
        match &config.accounts[0].credential {
            AccountCredential::Oauth {
                access_token,
                refresh_token,
                expires_at_ms,
                last_refresh_ms,
                ..
            } => {
                assert_eq!(access_token, "at-new");
                assert_eq!(refresh_token, "rt-a@x.com", "refresh preserved");
                assert_eq!(*expires_at_ms, 42);
                assert_eq!(*last_refresh_ms, Some(41), "refresh stamps the timestamp");
            }
            other => panic!("unexpected credential {other:?}"),
        }

        // Match by name too; unknown identity is reported.
        assert!(config.update_oauth_tokens("a@x.com", "at-2", Some("rt-2"), 43, 42));
        assert!(!config.update_oauth_tokens("nobody", "at", None, 0, 0));
    }

    #[test]
    fn remove_account_by_name() {
        let mut config = Config::default();
        config.accounts.push(oauth_account("a@x.com", "uuid-a"));
        assert!(config.remove_account("a@x.com"));
        assert!(!config.remove_account("a@x.com"));
        assert!(config.accounts.is_empty());
    }

    #[test]
    fn atomic_save_leaves_no_temp_files() {
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        save_path(&path, &Config::default()).expect("save");
        save_path(&path, &Config::default()).expect("overwrite");

        let entries: Vec<_> = fs::read_dir(dir.path())
            .expect("read dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("llmux.json")]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        }
    }

    #[test]
    fn idle_probe_defaults_are_always_on() {
        // Issue #45: the probe is ALWAYS-ON out of the box — a cold account
        // (no 5h/7d window) gets a 1-token probe without any operator opt-in.
        // Both the Rust default and a config file missing the block entirely
        // must load enabled with the current 15-minute sweep/cooldown.
        let defaults = IdleProbeConfig::default();
        assert!(defaults.enabled, "probing on by default");
        assert_eq!(defaults.per_account_cooldown_secs, 900);
        assert_eq!(defaults.sweep_secs, 900, "15-min sweep by default");
        assert_eq!(
            defaults.stale_after_secs, 900,
            "stale windows re-probe by default (Z 2026-07-15 cold-refresh)"
        );

        let parsed: IdleProbeConfig = serde_json::from_str("{}").expect("empty block parses");
        assert_eq!(parsed, defaults, "missing fields load always-on");
    }

    #[test]
    fn legacy_idle_probe_default_triple_upgrades_to_always_on() {
        // A config written by a pre-#45 build for a user who never touched the
        // block carries EXACTLY the old conservative triple. That is
        // indistinguishable from "unset", so load_path must treat it as unset
        // and adopt the new always-on defaults (else it would pin probing off).
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        fs::write(
            &path,
            r#"{ "version": 1, "proxy": { "idle_probe": {
                "enabled": false, "per_account_cooldown_secs": 3600, "sweep_secs": 0 } } }"#,
        )
        .expect("write");

        let loaded = load_path(&path).expect("load");
        assert_eq!(
            loaded.proxy.idle_probe,
            IdleProbeConfig::default(),
            "legacy triple upgrades to always-on"
        );
        assert!(loaded.proxy.idle_probe.enabled);
        assert_eq!(loaded.proxy.idle_probe.sweep_secs, 900);
    }

    #[test]
    fn legacy_idle_probe_hourly_default_upgrades_to_cold_refresh() {
        // A config written by a #45-era build for a user who never touched the
        // block carries EXACTLY the old always-on hourly quadruple — also
        // indistinguishable from "unset", so it adopts the 15-min cold-refresh
        // defaults (Z 2026-07-15) instead of pinning the hourly cadence.
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        fs::write(
            &path,
            r#"{ "version": 1, "proxy": { "idle_probe": {
                "enabled": true, "per_account_cooldown_secs": 3600, "sweep_secs": 3600 } } }"#,
        )
        .expect("write");

        let loaded = load_path(&path).expect("load");
        assert_eq!(loaded.proxy.idle_probe, IdleProbeConfig::default());
        assert_eq!(loaded.proxy.idle_probe.sweep_secs, 900);
    }

    #[test]
    fn explicit_idle_probe_opt_out_survives_load_unchanged() {
        // enabled=false BUT with a non-default cooldown → an operator's explicit
        // post-upgrade opt-out, not the legacy triple. It must load verbatim.
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        fs::write(
            &path,
            r#"{ "version": 1, "proxy": { "idle_probe": {
                "enabled": false, "per_account_cooldown_secs": 7200, "sweep_secs": 0 } } }"#,
        )
        .expect("write");

        let loaded = load_path(&path).expect("load");
        assert_eq!(
            loaded.proxy.idle_probe,
            IdleProbeConfig {
                enabled: false,
                per_account_cooldown_secs: 7200,
                sweep_secs: 0,
                stale_after_secs: 900,
            },
            "an explicit non-default opt-out is kept verbatim"
        );
    }

    #[test]
    fn missing_idle_probe_block_loads_always_on_via_load_path() {
        // A config with no idle_probe (or no proxy) block at all loads the
        // always-on defaults through the serde defaults — no migration needed.
        let dir = TempDir::new();
        let path = dir.path().join("llmux.json");
        fs::write(&path, r#"{ "version": 1 }"#).expect("write");

        let loaded = load_path(&path).expect("load");
        assert_eq!(loaded.proxy.idle_probe, IdleProbeConfig::default());
        assert!(loaded.proxy.idle_probe.enabled);
    }

    #[test]
    fn config_path_env_override() {
        // Only this test touches LLMUX_CONFIG; every other test uses
        // the *_path variants, so no env race across the parallel runner.
        std::env::set_var(CONFIG_ENV, "/tmp/llmux-override.json");
        let path = config_path().expect("path");
        std::env::remove_var(CONFIG_ENV);
        assert_eq!(path, PathBuf::from("/tmp/llmux-override.json"));
    }
}
