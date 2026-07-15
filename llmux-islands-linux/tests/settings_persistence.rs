#[path = "../src/settings.rs"]
mod settings;

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use llmux_islands_core::ReleaseChannel;
use settings::{ApiKey, LocalSettings, SaveOutcome, SettingsErrorKind, SettingsStore};

static TEST_ID: AtomicU64 = AtomicU64::new(0);
static ENVIRONMENT_LOCK: Mutex<()> = Mutex::new(());

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "llmux-islands-settings-{label}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct EnvironmentRestore {
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
}

impl EnvironmentRestore {
    fn capture() -> Self {
        Self {
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            home: std::env::var_os("HOME"),
        }
    }
}

impl Drop for EnvironmentRestore {
    fn drop(&mut self) {
        match &self.xdg_config_home {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match &self.home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn defaults_match_the_linux_shell_policy() {
    let settings = LocalSettings::default();

    assert_eq!(settings.selected_screen_id, "");
    assert_eq!(settings.sound_id, "message-new-instant");
    assert!(settings.show_fable_weekly);
    assert_eq!(settings.endpoint, "http://127.0.0.1:3456");
    assert!(settings.api_key.is_none());
    assert_eq!(settings.release_channel, ReleaseChannel::Stable);
    settings
        .validate()
        .expect("default settings should validate");
}

#[test]
fn discovery_prefers_absolute_xdg_then_falls_back_to_home() {
    let _lock = ENVIRONMENT_LOCK.lock().expect("environment lock");
    let _restore = EnvironmentRestore::capture();
    let root = TestDir::new("discovery");

    std::env::set_var("XDG_CONFIG_HOME", root.path().join("xdg"));
    std::env::remove_var("HOME");
    assert_eq!(
        SettingsStore::discover().expect("XDG settings path").path(),
        root.path().join("xdg/llmux/islands.json")
    );

    std::env::set_var("XDG_CONFIG_HOME", "relative-path-is-ignored");
    std::env::set_var("HOME", root.path().join("home"));
    assert_eq!(
        SettingsStore::discover()
            .expect("HOME settings path")
            .path(),
        root.path().join("home/.config/llmux/islands.json")
    );

    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("HOME");
    let error = match SettingsStore::discover() {
        Ok(_) => panic!("missing configuration home must fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), SettingsErrorKind::ConfigHomeUnavailable);
}

#[test]
fn private_atomic_round_trip_is_idempotent() {
    let root = TestDir::new("round-trip");
    let store = SettingsStore::from_config_home(root.path());
    let settings = LocalSettings {
        selected_screen_id: "DP-2".to_string(),
        sound_id: "complete".to_string(),
        show_fable_weekly: false,
        endpoint: "https://daemon.example.test:8443".to_string(),
        api_key: Some(ApiKey::new("sk-round-trip-secret")),
        release_channel: ReleaseChannel::Preview,
    };

    assert_eq!(
        store.save(&settings).expect("first save"),
        SaveOutcome::Written
    );
    assert_eq!(store.load().expect("load"), settings);

    let document_metadata = fs::metadata(store.path()).expect("settings metadata");
    let document_inode = document_metadata.ino();
    assert_eq!(document_metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        fs::metadata(store.path().parent().expect("settings parent"))
            .expect("settings directory metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    assert_eq!(
        store.save(&settings).expect("idempotent save"),
        SaveOutcome::Unchanged
    );
    assert_eq!(
        fs::metadata(store.path()).expect("settings metadata").ino(),
        document_inode,
        "an unchanged save must not replace the settings file"
    );

    let entries: Vec<_> = fs::read_dir(store.path().parent().expect("settings parent"))
        .expect("settings directory")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect();
    assert_eq!(entries, vec!["islands.json"]);
}

#[test]
fn invalid_endpoint_is_rejected_before_any_write() {
    let root = TestDir::new("invalid-endpoint");
    let store = SettingsStore::from_config_home(root.path());
    let settings = LocalSettings {
        endpoint: "file:///tmp/llmux.sock".to_string(),
        ..LocalSettings::default()
    };

    let error = store.save(&settings).expect_err("invalid endpoint");

    assert_eq!(error.kind(), SettingsErrorKind::InvalidEndpoint);
    assert!(!store.path().parent().expect("settings parent").exists());
}

#[test]
fn remote_endpoint_can_persist_without_a_key_after_explicit_clearing() {
    let root = TestDir::new("missing-key");
    let store = SettingsStore::from_config_home(root.path());
    let settings = LocalSettings {
        endpoint: "https://daemon.example.test:8443".to_string(),
        ..LocalSettings::default()
    };

    assert_eq!(
        store.save(&settings).expect("persist cleared key"),
        SaveOutcome::Written
    );
    let loaded = store.load().expect("load unauthenticated remote settings");
    assert_eq!(loaded.endpoint, settings.endpoint);
    assert!(loaded.api_key.is_none());
}

#[test]
fn remote_plaintext_http_is_rejected_before_credentials_can_be_persisted() {
    let root = TestDir::new("insecure-remote");
    let store = SettingsStore::from_config_home(root.path());
    let settings = LocalSettings {
        endpoint: "http://daemon.example.test:3456".to_string(),
        api_key: Some(ApiKey::new("sk-must-not-cross-plaintext")),
        ..LocalSettings::default()
    };

    let error = store
        .save(&settings)
        .expect_err("remote HTTP must fail closed");

    assert_eq!(error.kind(), SettingsErrorKind::InsecureEndpoint);
    assert!(!store.path().parent().expect("settings parent").exists());
}

#[test]
fn debug_and_validation_errors_never_expose_secrets() {
    let root = TestDir::new("redaction");
    let store = SettingsStore::from_config_home(root.path());
    let api_key = "sk-debug-redaction-secret";
    let endpoint_secret = "endpoint-password-secret";
    let settings = LocalSettings {
        endpoint: format!("http://user:{endpoint_secret}@daemon.example.test"),
        api_key: Some(ApiKey::new(api_key)),
        ..LocalSettings::default()
    };

    let settings_debug = format!("{settings:?}");
    let key_debug = format!("{:?}", settings.api_key.as_ref().expect("key"));
    let error = store
        .save(&settings)
        .expect_err("credential-bearing endpoint");
    let error_text = format!("{error:?} {error}");

    for rendered in [&settings_debug, &key_debug, &error_text] {
        assert!(!rendered.contains(api_key));
        assert!(!rendered.contains(endpoint_secret));
    }
    assert!(key_debug.contains("REDACTED"));
}

#[test]
fn corrupt_document_fails_safely_without_echoing_contents() {
    let root = TestDir::new("corrupt");
    let store = SettingsStore::from_config_home(root.path());
    let corrupt_secret = "sk-corrupt-document-secret";
    fs::create_dir_all(store.path().parent().expect("settings parent"))
        .expect("settings directory");
    fs::write(store.path(), format!("{{not-json:{corrupt_secret}}}"))
        .expect("corrupt settings fixture");
    fs::set_permissions(
        store.path().parent().expect("settings parent"),
        fs::Permissions::from_mode(0o700),
    )
    .expect("private settings directory");
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o600))
        .expect("private settings file");

    let error = store.load().expect_err("corrupt settings");
    let rendered = format!("{error:?} {error}");

    assert_eq!(error.kind(), SettingsErrorKind::CorruptDocument);
    assert!(!rendered.contains(corrupt_secret));
}

#[test]
fn load_rejects_permissive_or_symlinked_secret_documents() {
    let root = TestDir::new("unsafe-document");
    let store = SettingsStore::from_config_home(root.path());
    let parent = store.path().parent().expect("settings parent");
    fs::create_dir_all(parent).expect("settings directory");
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .expect("private settings directory");
    fs::write(store.path(), b"{}\n").expect("settings document");
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644))
        .expect("permissive settings document");

    let error = store
        .load()
        .expect_err("permissive settings must fail closed");
    assert_eq!(error.kind(), SettingsErrorKind::UnsafeDocument);

    fs::remove_file(store.path()).expect("remove permissive document");
    let target = root.path().join("target.json");
    fs::write(&target, b"{}\n").expect("symlink target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("private target");
    symlink(&target, store.path()).expect("settings symlink");

    let error = store.load().expect_err("settings symlink must fail closed");
    assert_eq!(error.kind(), SettingsErrorKind::UnsafeDocument);
}

#[test]
fn save_rejects_a_symlinked_settings_parent_without_touching_its_target() {
    let root = TestDir::new("symlink-parent");
    let config_home = root.path().join("config");
    let outside = root.path().join("outside");
    fs::create_dir_all(&config_home).expect("config home");
    fs::create_dir_all(&outside).expect("outside directory");
    symlink(&outside, config_home.join("llmux")).expect("settings parent symlink");
    let store = SettingsStore::from_config_home(&config_home);

    let error = store
        .save(&LocalSettings::default())
        .expect_err("symlinked parent must fail closed");

    assert_eq!(error.kind(), SettingsErrorKind::UnsafeDocument);
    assert!(fs::read_dir(&outside)
        .expect("outside directory")
        .next()
        .is_none());
}
