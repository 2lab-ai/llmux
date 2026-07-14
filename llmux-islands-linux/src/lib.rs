//! Linux integration for the platform-neutral llmux Islands core.

use std::{collections::BTreeSet, env, fmt, time::Duration};

use llmux_islands_core::{
    Action, ClientConfig, ClientError, DaemonClient, DeriveOptions, Effect, EventDraft,
    LocalSettingsChange, MaintenanceCommand, Navigation, OpenReason, OperationRequest,
    Presentation, Provider, RefreshSource, ReleaseChannel, SecretString, UiState,
};
use serde_json::{json, Value};

pub mod desktop;
pub mod maintenance;
pub mod platform;
pub mod settings;
pub mod snapshot;

pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:3456";
pub const DASHBOARD_POLL_INTERVAL: Duration = Duration::from_secs(10);
pub const LOGIN_POLL_INTERVAL: Duration = Duration::from_secs(2);
pub const LOGIN_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// Checked-in daemon document used only by deterministic tests and snapshots.
pub const DASHBOARD_FIXTURE: &str = include_str!("../fixtures/dashboard.json");

/// Validated controller configuration. Credentials stay in `SecretString` and
/// only the presence bit enters `DeriveOptions`/`UiState`.
pub struct ControllerConfig {
    endpoint: String,
    api_key: Option<SecretString>,
    remote: bool,
    presentation: Presentation,
}

impl ControllerConfig {
    pub fn from_env(presentation: Presentation) -> Result<Self, ClientError> {
        let stored = settings::SettingsStore::discover()
            .and_then(|store| store.load())
            .unwrap_or_default();
        let endpoint = match env::var("LLMUX_URL") {
            Ok(endpoint) if !endpoint.trim().is_empty() => endpoint,
            _ if !stored.endpoint.trim().is_empty() => stored.endpoint.clone(),
            _ => DEFAULT_ENDPOINT.to_string(),
        };
        let api_key = match env::var("LLMUX_API_KEY") {
            Ok(api_key) if !api_key.trim().is_empty() => Some(api_key),
            _ => stored
                .api_key
                .as_ref()
                .map(|api_key| api_key.expose_secret().to_string()),
        };
        Self::from_values(&endpoint, api_key, presentation)
    }

    pub fn from_values(
        endpoint: &str,
        api_key: Option<String>,
        presentation: Presentation,
    ) -> Result<Self, ClientError> {
        let client = ClientConfig::new(endpoint)?;
        let remote = client.is_remote();
        let api_key = api_key
            .filter(|value| !value.trim().is_empty())
            .map(SecretString::new);
        Ok(Self {
            endpoint: endpoint.to_string(),
            api_key,
            remote,
            presentation,
        })
    }

    #[must_use]
    pub fn derive_options(&self) -> DeriveOptions {
        let api_key_configured = self.api_key.is_some();
        DeriveOptions {
            endpoint_display: self.endpoint.clone(),
            remote: self.remote,
            authenticated: !self.remote || api_key_configured,
            api_key_configured,
            selected_screen_id: "auto".to_string(),
            presentation: self.presentation,
        }
    }

    pub fn daemon_client(&self) -> Result<DaemonClient, ClientError> {
        let mut config = ClientConfig::new(&self.endpoint)?;
        if let Some(api_key) = &self.api_key {
            config = config.with_api_key(api_key.clone());
        }
        DaemonClient::new(config)
    }

    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    #[must_use]
    pub const fn is_remote(&self) -> bool {
        self.remote
    }
}

impl fmt::Debug for ControllerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControllerConfig")
            .field(
                "endpoint",
                &llmux_islands_core::sanitize_endpoint(&self.endpoint),
            )
            .field("remote", &self.remote)
            .field("api_key_configured", &self.api_key.is_some())
            .field("presentation", &self.presentation)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformRequest {
    OpenUrl(String),
    CopyText(String),
    TestNotification { sound_id: Option<String> },
    Quit,
}

#[derive(Debug, Default)]
pub struct DispatchOutcome {
    pub effects: Vec<Effect>,
    pub platform: Vec<PlatformRequest>,
    pub error: Option<String>,
}

impl DispatchOutcome {
    fn effects(effects: Vec<Effect>) -> Self {
        Self {
            effects,
            ..Self::default()
        }
    }

    fn platform(request: PlatformRequest) -> Self {
        Self {
            platform: vec![request],
            ..Self::default()
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            error: Some(message.into()),
            ..Self::default()
        }
    }
}

/// Qt-independent controller model. It owns the canonical reducer and maps
/// semantic QML actions to typed core/platform boundaries.
pub struct ControllerModel {
    core: llmux_islands_core::Core,
    connection_options: DeriveOptions,
    next_operation_id: u64,
    local_settings: settings::LocalSettings,
    autostart_enabled: bool,
    install_owner: String,
    screen_inventory: Vec<ScreenInventoryItem>,
    tray_available: Option<bool>,
    notifications_available: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScreenInventoryItem {
    id: String,
    label: String,
}

impl ControllerModel {
    #[must_use]
    pub fn new(options: DeriveOptions) -> Self {
        let connection_options = options.clone();
        let core = llmux_islands_core::Core::new(options);
        Self {
            core,
            connection_options,
            next_operation_id: 0,
            local_settings: settings::LocalSettings::default(),
            autostart_enabled: false,
            install_owner: "unknown".to_string(),
            screen_inventory: Vec::new(),
            tray_available: None,
            notifications_available: None,
        }
    }

    pub fn from_fixture(options: DeriveOptions, now_ms: u64) -> Result<Self, serde_json::Error> {
        let mut model = Self::new(options);
        let request_id = model
            .core
            .reduce(Action::RefreshRequested {
                source: RefreshSource::Startup,
            })
            .into_iter()
            .find_map(|effect| match effect {
                Effect::FetchDashboard { request_id } => Some(request_id),
                _ => None,
            });
        if let Some(request_id) = request_id {
            let document = serde_json::from_str(DASHBOARD_FIXTURE)?;
            model.core.reduce(Action::DashboardReceived {
                request_id,
                document: Box::new(document),
                received_at_ms: now_ms,
            });
        }
        Ok(model)
    }

    #[must_use]
    pub fn ui_state(&self) -> UiState {
        let mut state = self.core.state().clone();
        state.connection.endpoint_display =
            llmux_islands_core::sanitize_endpoint(&self.connection_options.endpoint_display);
        state.connection.remote = self.connection_options.remote;
        state.connection.authenticated = self.connection_options.authenticated;
        let selected_screen_id = if self.local_settings.selected_screen_id.is_empty() {
            "auto"
        } else {
            &self.local_settings.selected_screen_id
        };
        state.window.selected_screen_id = selected_screen_id.to_string();
        state.settings.show_fable_weekly = self.local_settings.show_fable_weekly;
        state.settings.api_key_configured =
            self.connection_options.api_key_configured || self.local_settings.api_key.is_some();
        state.settings.sound_id = Some(self.local_settings.sound_id.clone());
        state.settings.screens = screen_options(selected_screen_id, &self.screen_inventory);
        state.settings.sounds = sound_options();
        state.settings.autostart = json!({ "enabled": self.autostart_enabled });
        state.settings.maintenance = json!({
            "channel": self.local_settings.release_channel,
            "version": env!("CARGO_PKG_VERSION"),
            "islands_version": env!("CARGO_PKG_VERSION"),
            "latest_version": Value::Null,
            "update_available": Value::Null,
            "install_owner": self.install_owner,
            "license": "MIT",
            "source_url": "https://github.com/2lab-ai/llmux",
        });
        state.settings.capabilities = capability_state(
            state.window.presentation,
            self.tray_available,
            self.notifications_available,
        );
        state
    }

    pub fn state_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.ui_state())
    }

    pub fn apply(&mut self, action: Action) -> Vec<Effect> {
        self.core.reduce(action)
    }

    pub fn set_platform_state(
        &mut self,
        local_settings: settings::LocalSettings,
        autostart_enabled: bool,
        install_owner: impl Into<String>,
    ) {
        self.local_settings = local_settings;
        self.autostart_enabled = autostart_enabled;
        self.install_owner = llmux_islands_core::sanitize_text(&install_owner.into());
    }

    pub fn set_connection_options(&mut self, options: DeriveOptions) {
        self.connection_options = options;
    }

    pub fn dispatch(&mut self, action: &str, payload_json: &str, now_ms: u64) -> DispatchOutcome {
        let payload = match serde_json::from_str::<Value>(payload_json) {
            Ok(payload) if payload.is_object() => payload,
            _ => return DispatchOutcome::error("invalid action payload"),
        };

        match action {
            "app_started" => DispatchOutcome::effects(self.apply(Action::AppStarted)),
            "tray_activated" => DispatchOutcome::effects(self.apply(Action::TrayActivated)),
            "open_requested" => {
                let reason = match string_field(&payload, &["reason"]) {
                    Some("click") => OpenReason::Click,
                    Some("hover") => OpenReason::Hover,
                    Some("notification") => OpenReason::Notification,
                    Some("usage_alert") => OpenReason::UsageAlert,
                    Some("boot") => OpenReason::Boot,
                    _ => return DispatchOutcome::error("invalid window open reason"),
                };
                DispatchOutcome::effects(self.apply(Action::OpenRequested { reason }))
            }
            "close_requested" => DispatchOutcome::effects(self.apply(Action::CloseRequested)),
            "boot_close_elapsed" => {
                let Some(tray_available) = payload.get("tray_available").and_then(Value::as_bool)
                else {
                    return DispatchOutcome::error("tray availability is required");
                };
                let window = &self.core.state().window;
                if tray_available && window.open && window.open_reason == OpenReason::Boot {
                    DispatchOutcome::effects(self.apply(Action::CloseRequested))
                } else {
                    DispatchOutcome::default()
                }
            }
            "window_metrics_changed" => {
                let Some(width) = payload
                    .get("width")
                    .and_then(Value::as_u64)
                    .and_then(|width| u32::try_from(width).ok())
                    .filter(|width| *width > 0)
                else {
                    return DispatchOutcome::error("window width is invalid");
                };
                let Some(content_height) = payload
                    .get("content_height")
                    .and_then(Value::as_u64)
                    .and_then(|height| u32::try_from(height).ok())
                    .filter(|height| *height > 0)
                else {
                    return DispatchOutcome::error("window content height is invalid");
                };
                DispatchOutcome::effects(self.apply(Action::WindowMetricsChanged {
                    width,
                    content_height,
                }))
            }
            "navigation_selected" | "select_surface" => {
                let navigation = match string_field(&payload, &["navigation", "surface"]) {
                    Some("usage") => Navigation::Usage,
                    Some("statistics") => Navigation::Statistics,
                    Some("menu") => Navigation::Menu,
                    _ => return DispatchOutcome::error("invalid navigation selection"),
                };
                DispatchOutcome::effects(self.apply(Action::NavigationSelected { navigation }))
            }
            "refresh" | "refresh_requested" | "dashboard_poll" => {
                let source = match string_field(&payload, &["source"]) {
                    Some("poll") => RefreshSource::Poll,
                    Some("retry") => RefreshSource::Retry,
                    Some("startup") => RefreshSource::Startup,
                    _ => RefreshSource::Manual,
                };
                DispatchOutcome::effects(self.apply(Action::RefreshRequested { source }))
            }
            "login_started" => self.start_login(&payload, now_ms),
            "login_cancelled" => self.cancel_login(),
            "add_api_key_submitted" => self.add_api_key(&payload, now_ms),
            "pause_account_requested" => self.pause_account(&payload, now_ms),
            "remove_account_confirmed" => self.remove_account(&payload, now_ms),
            "email_anonymous_changed" => self.update_email_anonymity(&payload, now_ms),
            "show_fable_changed" => match payload.get("enabled").and_then(Value::as_bool) {
                Some(enabled) => {
                    self.local_setting(LocalSettingsChange::ShowFable { enabled }, &payload, now_ms)
                }
                None => DispatchOutcome::error("show_fable enabled state is required"),
            },
            "set_setting" => {
                if payload
                    .get("email_anonymous")
                    .and_then(Value::as_bool)
                    .is_some()
                {
                    self.update_email_anonymity(&payload, now_ms)
                } else if let Some(show_fable_weekly) =
                    payload.get("show_fable_weekly").and_then(Value::as_bool)
                {
                    self.local_setting(
                        LocalSettingsChange::ShowFable {
                            enabled: show_fable_weekly,
                        },
                        &payload,
                        now_ms,
                    )
                } else {
                    DispatchOutcome::error("unsupported settings action")
                }
            }
            "screen_selected" => match string_field(&payload, &["id"]) {
                Some(id) => self.local_setting(
                    LocalSettingsChange::ScreenSelected { id: id.to_string() },
                    &payload,
                    now_ms,
                ),
                None => DispatchOutcome::error("screen id is required"),
            },
            "screen_inventory_changed" => self.update_screen_inventory(&payload),
            "desktop_capabilities_changed" => self.update_desktop_capabilities(&payload),
            "sound_selected" => match string_field(&payload, &["id"]) {
                Some(id) => self.local_setting(
                    LocalSettingsChange::SoundSelected { id: id.to_string() },
                    &payload,
                    now_ms,
                ),
                None => DispatchOutcome::error("sound id is required"),
            },
            "connection_apply_requested" => self.connection_setting(&payload, now_ms),
            "event_upsert_requested" => self.upsert_event(&payload, now_ms),
            "event_remove_requested" => self.remove_event(&payload, now_ms),
            "set_autostart" | "autostart_changed" => self.set_autostart(&payload, now_ms),
            "maintenance_requested" => self.maintenance(&payload, now_ms),
            "update_requested" => self.start_maintenance(MaintenanceCommand::Update, now_ms),
            "channel_change_requested" => self.change_channel(&payload, now_ms),
            "open_url_requested" => match string_field(&payload, &["url"]) {
                Some(url) if is_http_url(url) => {
                    DispatchOutcome::platform(PlatformRequest::OpenUrl(url.to_string()))
                }
                _ => DispatchOutcome::error("invalid external URL"),
            },
            "copy_text_requested" => match string_field(&payload, &["text"]) {
                Some(text) if !text.is_empty() => {
                    DispatchOutcome::platform(PlatformRequest::CopyText(text.to_string()))
                }
                _ => DispatchOutcome::error("copy text is empty"),
            },
            "sound_preview_requested" => {
                DispatchOutcome::platform(PlatformRequest::TestNotification {
                    sound_id: string_field(&payload, &["id"]).map(ToString::to_string),
                })
            }
            "test_notification" => {
                DispatchOutcome::platform(PlatformRequest::TestNotification { sound_id: None })
            }
            "quit" | "quit_requested" => DispatchOutcome::platform(PlatformRequest::Quit),
            _ => DispatchOutcome::error("unsupported semantic action"),
        }
    }

    fn start_login(&mut self, payload: &Value, now_ms: u64) -> DispatchOutcome {
        let provider = match string_field(payload, &["provider"]) {
            Some("claude") => Provider::Claude,
            Some("codex") => Provider::Codex,
            Some("grok") => Provider::Grok,
            _ => return DispatchOutcome::error("invalid login provider"),
        };
        let operation_id = self.operation_id("login", payload);
        DispatchOutcome::effects(self.apply(Action::LoginStarted {
            operation_id,
            provider,
            started_at_ms: now_ms,
        }))
    }

    fn cancel_login(&mut self) -> DispatchOutcome {
        let operation_id = self
            .core
            .state()
            .operation
            .as_ref()
            .filter(|operation| operation.kind == "login")
            .map(|operation| operation.id.clone());
        match operation_id {
            Some(operation_id) => {
                let effects = self.apply(Action::LoginCancelRequested { operation_id });
                if effects.is_empty() {
                    DispatchOutcome::error("login cannot be cancelled yet")
                } else {
                    DispatchOutcome::effects(effects)
                }
            }
            None => DispatchOutcome::error("no active login"),
        }
    }

    fn add_api_key(&mut self, payload: &Value, now_ms: u64) -> DispatchOutcome {
        let Some(api_key) = string_field(payload, &["api_key"]) else {
            return DispatchOutcome::error("API key is required");
        };
        let name = string_field(payload, &["name"])
            .filter(|name| !name.trim().is_empty())
            .map(ToString::to_string);
        let operation_id = self.operation_id("add-account", payload);
        DispatchOutcome::effects(self.apply(Action::OperationStarted {
            id: operation_id,
            request: OperationRequest::AddAccount {
                name: name.clone(),
                api_key: SecretString::new(api_key),
            },
            target_display: name,
            started_at_ms: now_ms,
        }))
    }

    fn pause_account(&mut self, payload: &Value, now_ms: u64) -> DispatchOutcome {
        let Some(account_id) = string_field(payload, &["account", "account_id"]) else {
            return DispatchOutcome::error("account id is required");
        };
        let Some(paused) = payload.get("paused").and_then(Value::as_bool) else {
            return DispatchOutcome::error("paused state is required");
        };
        let operation_id = self.operation_id("pause-account", payload);
        DispatchOutcome::effects(self.apply(Action::OperationStarted {
            id: operation_id,
            request: OperationRequest::PauseAccount {
                account_id: account_id.to_string(),
                paused,
            },
            target_display: Some(account_id.to_string()),
            started_at_ms: now_ms,
        }))
    }

    fn remove_account(&mut self, payload: &Value, now_ms: u64) -> DispatchOutcome {
        let Some(account_id) = string_field(payload, &["account", "account_id"]) else {
            return DispatchOutcome::error("account id is required");
        };
        let operation_id = self.operation_id("remove-account", payload);
        DispatchOutcome::effects(self.apply(Action::OperationStarted {
            id: operation_id,
            request: OperationRequest::RemoveAccount {
                account_id: account_id.to_string(),
                confirmed: true,
            },
            target_display: Some(account_id.to_string()),
            started_at_ms: now_ms,
        }))
    }

    fn update_email_anonymity(&mut self, payload: &Value, now_ms: u64) -> DispatchOutcome {
        let Some(email_anonymous) = payload
            .get("enabled")
            .or_else(|| payload.get("email_anonymous"))
            .and_then(Value::as_bool)
        else {
            return DispatchOutcome::error("email_anonymous is required");
        };
        let id = self.operation_id("settings", payload);
        DispatchOutcome::effects(self.apply(Action::SettingsChanged {
            id,
            email_anonymous,
            started_at_ms: now_ms,
        }))
    }

    fn local_setting(
        &mut self,
        change: LocalSettingsChange,
        payload: &Value,
        now_ms: u64,
    ) -> DispatchOutcome {
        let operation_id = self.operation_id("settings", payload);
        let target_display = match &change {
            LocalSettingsChange::ScreenSelected { id }
            | LocalSettingsChange::SoundSelected { id } => {
                Some(llmux_islands_core::sanitize_text(id))
            }
            LocalSettingsChange::ShowFable { .. } => Some("Fable weekly quota".to_string()),
            LocalSettingsChange::ConnectionApplied { endpoint, .. } => {
                Some(llmux_islands_core::sanitize_endpoint(endpoint))
            }
        };
        DispatchOutcome::effects(self.apply(Action::OperationStarted {
            id: operation_id,
            request: OperationRequest::PersistLocalSettings { change },
            target_display,
            started_at_ms: now_ms,
        }))
    }

    fn connection_setting(&mut self, payload: &Value, now_ms: u64) -> DispatchOutcome {
        let Some(host) = string_field(payload, &["host"]) else {
            return DispatchOutcome::error("connection host is required");
        };
        let scheme = match string_field(payload, &["scheme"]) {
            None | Some("http") => "http",
            Some("https") => "https",
            Some(_) => return DispatchOutcome::error("connection scheme is invalid"),
        };
        let Some(port) = payload
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port > 0)
        else {
            return DispatchOutcome::error("connection port is invalid");
        };
        let host = host.trim();
        let host = match (host.strip_prefix('['), host.strip_suffix(']')) {
            (Some(without_open), Some(_)) => without_open.strip_suffix(']').unwrap_or(without_open),
            (None, None) => host,
            _ => return DispatchOutcome::error("connection host is invalid"),
        };
        if host.is_empty()
            || host
                .chars()
                .any(|character| character.is_whitespace() || "/@?#".contains(character))
        {
            return DispatchOutcome::error("connection host is invalid");
        }
        let authority = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        let endpoint = format!("{scheme}://{authority}:{port}");
        let endpoint_config = match ClientConfig::new(&endpoint) {
            Ok(config) => config,
            Err(_) => return DispatchOutcome::error("connection endpoint is invalid"),
        };
        let submitted_key =
            string_field(payload, &["api_key"]).filter(|api_key| !api_key.trim().is_empty());
        let key_mode = string_field(payload, &["api_key_mode"]);
        let api_key = match key_mode {
            Some("clear") if submitted_key.is_none() => None,
            Some("replace") => match submitted_key {
                Some(api_key) => Some(SecretString::new(api_key)),
                None => return DispatchOutcome::error("replacement API key is required"),
            },
            Some("keep") | None => submitted_key.map(SecretString::new).or_else(|| {
                self.local_settings
                    .api_key
                    .as_ref()
                    .map(|api_key| SecretString::new(api_key.expose_secret()))
            }),
            Some("clear") => {
                return DispatchOutcome::error("a cleared API key cannot also be replaced")
            }
            Some(_) => return DispatchOutcome::error("API key mode is invalid"),
        };
        if endpoint_config.is_remote() && api_key.is_none() && key_mode != Some("clear") {
            return DispatchOutcome::error(
                "remote connection requires an API key or an explicit clear",
            );
        }
        self.local_setting(
            LocalSettingsChange::ConnectionApplied { endpoint, api_key },
            payload,
            now_ms,
        )
    }

    fn update_desktop_capabilities(&mut self, payload: &Value) -> DispatchOutcome {
        let Some(tray_available) = payload.get("tray_available").and_then(Value::as_bool) else {
            return DispatchOutcome::error("tray availability is required");
        };
        let Some(notifications_available) = payload
            .get("notifications_available")
            .and_then(Value::as_bool)
        else {
            return DispatchOutcome::error("notification availability is required");
        };
        self.tray_available = Some(tray_available);
        self.notifications_available = Some(notifications_available);
        DispatchOutcome::default()
    }

    fn update_screen_inventory(&mut self, payload: &Value) -> DispatchOutcome {
        let Some(screens) = payload.get("screens").and_then(Value::as_array) else {
            return DispatchOutcome::error("screen inventory is required");
        };
        let mut seen = BTreeSet::new();
        self.screen_inventory = screens
            .iter()
            .take(32)
            .filter_map(|screen| {
                let id = string_field(screen, &["id"])?;
                if id.is_empty()
                    || id.len() > 512
                    || id.chars().any(char::is_control)
                    || !seen.insert(id.to_string())
                {
                    return None;
                }
                let label = string_field(screen, &["label"])
                    .filter(|label| !label.is_empty())
                    .unwrap_or(id);
                Some(ScreenInventoryItem {
                    id: id.to_string(),
                    label: llmux_islands_core::sanitize_text(label),
                })
            })
            .collect();
        DispatchOutcome::default()
    }

    fn upsert_event(&mut self, payload: &Value, now_ms: u64) -> DispatchOutcome {
        let source = payload
            .get("event")
            .filter(|event| event.is_object())
            .unwrap_or(payload);
        let (Some(id), Some(from), Some(to), Some(content)) = (
            string_field(source, &["id"]),
            string_field(source, &["from"]),
            string_field(source, &["to"]),
            string_field(source, &["content"]),
        ) else {
            return DispatchOutcome::error("complete event fields are required");
        };
        let operation_id = self.operation_id("event", payload);
        DispatchOutcome::effects(self.apply(Action::EventUpsertRequested {
            id: operation_id,
            event: EventDraft {
                id: id.to_string(),
                from: from.to_string(),
                to: to.to_string(),
                content: content.to_string(),
            },
            started_at_ms: now_ms,
        }))
    }

    fn remove_event(&mut self, payload: &Value, now_ms: u64) -> DispatchOutcome {
        let Some(event_id) = string_field(payload, &["id", "event_id"]) else {
            return DispatchOutcome::error("event id is required");
        };
        let operation_id = self.operation_id("event", payload);
        DispatchOutcome::effects(self.apply(Action::EventRemoveRequested {
            id: operation_id,
            event_id: event_id.to_string(),
            started_at_ms: now_ms,
        }))
    }

    fn set_autostart(&mut self, payload: &Value, now_ms: u64) -> DispatchOutcome {
        let Some(enabled) = payload.get("enabled").and_then(Value::as_bool) else {
            return DispatchOutcome::error("autostart enabled state is required");
        };
        let id = self.operation_id("autostart", payload);
        DispatchOutcome::effects(self.apply(Action::AutostartChanged {
            id,
            enabled,
            started_at_ms: now_ms,
        }))
    }

    fn maintenance(&mut self, payload: &Value, now_ms: u64) -> DispatchOutcome {
        match string_field(payload, &["kind", "command"]) {
            Some("update") => self.start_maintenance(MaintenanceCommand::Update, now_ms),
            Some("change_channel") => self.change_channel(payload, now_ms),
            _ => DispatchOutcome::error("invalid maintenance command"),
        }
    }

    fn change_channel(&mut self, payload: &Value, now_ms: u64) -> DispatchOutcome {
        let channel = match string_field(payload, &["channel"]) {
            Some("stable") => ReleaseChannel::Stable,
            Some("preview") => ReleaseChannel::Preview,
            _ => return DispatchOutcome::error("invalid release channel"),
        };
        self.start_maintenance(MaintenanceCommand::ChangeChannel { channel }, now_ms)
    }

    fn start_maintenance(&mut self, command: MaintenanceCommand, now_ms: u64) -> DispatchOutcome {
        let payload = Value::Object(Default::default());
        let id = self.operation_id("maintenance", &payload);
        DispatchOutcome::effects(self.apply(Action::MaintenanceRequested {
            id,
            command,
            started_at_ms: now_ms,
        }))
    }

    fn operation_id(&mut self, prefix: &str, payload: &Value) -> String {
        if let Some(id) = string_field(payload, &["operation_id"]).filter(|id| !id.is_empty()) {
            return id.to_string();
        }
        self.next_operation_id = self.next_operation_id.saturating_add(1);
        format!("{prefix}-{}", self.next_operation_id)
    }
}

fn string_field<'a>(value: &'a Value, fields: &[&str]) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_str))
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn screen_options(selected_screen_id: &str, inventory: &[ScreenInventoryItem]) -> Vec<Value> {
    let mut screens = vec![json!({
        "id": "auto",
        "label": "Automatic",
        "selected": selected_screen_id == "auto",
    })];
    screens.extend(inventory.iter().map(|screen| {
        json!({
            "id": screen.id,
            "label": screen.label,
            "selected": selected_screen_id == screen.id,
        })
    }));
    if selected_screen_id != "auto"
        && !inventory
            .iter()
            .any(|screen| screen.id == selected_screen_id)
    {
        screens.push(json!({
            "id": selected_screen_id,
            "label": format!("{} (unavailable)", selected_screen_id),
            "selected": true,
        }));
    }
    screens
}

fn sound_options() -> Vec<Value> {
    vec![
        json!({ "id": "silent", "label": "Disabled" }),
        json!({ "id": "message-new-instant", "label": "KDE Default" }),
        json!({ "id": "complete", "label": "Complete" }),
        json!({ "id": "message", "label": "Message" }),
    ]
}

fn capability_state(
    presentation: Presentation,
    tray_available: Option<bool>,
    notifications_available: Option<bool>,
) -> Value {
    json!({
        "layer_shell": {
            "available": presentation == Presentation::LayerShell,
            "reason": match presentation {
                Presentation::LayerShell => "LayerShellQt top overlay is active",
                Presentation::PositionedX11 => "X11 uses the positioned always-on-top fallback",
                Presentation::Regular => "The regular-window fallback is active",
            },
        },
        "tray": {
            "available": tray_available.unwrap_or(false),
            "reason": match tray_available {
                Some(true) => "Qt reports a system tray host for this session",
                Some(false) => "Qt reports no system tray host; the regular window stays open",
                None => "System tray availability has not been detected yet",
            },
        },
        "notifications": {
            "available": notifications_available.unwrap_or(false),
            "reason": match notifications_available {
                Some(true) => "Qt tray notifications are available for this session",
                Some(false) => "Native notifications are unavailable without a tray host",
                None => "Notification availability has not been detected yet",
            },
        },
    })
}
