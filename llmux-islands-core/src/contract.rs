use std::collections::BTreeMap;
use std::fmt;

use llmux::dashboard::DashboardDoc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const UI_SCHEMA_VERSION: u32 = 1;
pub const MAX_VERIFICATION_RECEIPTS: usize = 100;
pub const LOGIN_TIMEOUT_MS: u64 = 5 * 60 * 1_000;
pub const DASHBOARD_RETRY_BASE_MS: u64 = 1_000;
pub const DASHBOARD_RETRY_MAX_MS: u64 = 30_000;
pub const MIN_WINDOW_WIDTH: u32 = 240;
pub const MAX_WINDOW_WIDTH: u32 = 3_840;
pub const MIN_CONTENT_HEIGHT: u32 = 44;
pub const MAX_CONTENT_HEIGHT: u32 = 2_160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Starting,
    Ready,
    Offline,
    Fatal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Navigation {
    Usage,
    Statistics,
    Menu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenReason {
    None,
    Click,
    Hover,
    Notification,
    UsageAlert,
    Boot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presentation {
    LayerShell,
    PositionedX11,
    Regular,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Claude,
    Codex,
    Grok,
    Api,
    Unknown,
}

impl Provider {
    pub(crate) fn from_account_kind(kind: &str) -> Self {
        match kind.to_ascii_lowercase().as_str() {
            "oauth" | "claude" => Self::Claude,
            "codex" => Self::Codex,
            "grok" => Self::Grok,
            "apikey" | "api_key" | "api" => Self::Api,
            _ => Self::Unknown,
        }
    }

    pub(crate) fn from_group(group: &str) -> Self {
        match group.to_ascii_lowercase().as_str() {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "grok" => Self::Grok,
            "api" => Self::Api,
            _ => Self::Unknown,
        }
    }

    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::Api => "api",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiState {
    pub schema_version: u32,
    pub revision: u64,
    pub lifecycle: Lifecycle,
    pub window: WindowState,
    pub navigation: Navigation,
    pub connection: ConnectionState,
    pub usage: UsageState,
    pub statistics: StatisticsState,
    pub settings: SettingsState,
    pub operation: Option<OperationState>,
    pub notices: Vec<Notice>,
    pub verification_receipts: Vec<VerificationReceipt>,
}

impl UiState {
    pub(crate) fn initial(options: &DeriveOptions) -> Self {
        Self {
            schema_version: UI_SCHEMA_VERSION,
            revision: 0,
            lifecycle: Lifecycle::Starting,
            window: WindowState {
                open: false,
                open_reason: OpenReason::None,
                selected_screen_id: options.selected_screen_id.clone(),
                presentation: options.presentation,
                width: 260,
                content_height: 44,
                provider_in_flight: BTreeMap::new(),
            },
            navigation: Navigation::Usage,
            connection: ConnectionState {
                endpoint_display: options.endpoint_display.clone(),
                remote: options.remote,
                authenticated: options.authenticated,
                daemon_version: None,
                last_success_ms: None,
                retry_at_ms: None,
                error: None,
            },
            usage: UsageState::default(),
            statistics: StatisticsState::default(),
            settings: SettingsState {
                api_key_configured: options.api_key_configured,
                ..SettingsState::default()
            },
            operation: None,
            notices: Vec::new(),
            verification_receipts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    pub open: bool,
    pub open_reason: OpenReason,
    pub selected_screen_id: String,
    pub presentation: Presentation,
    pub width: u32,
    pub content_height: u32,
    pub provider_in_flight: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionState {
    pub endpoint_display: String,
    pub remote: bool,
    pub authenticated: bool,
    pub daemon_version: Option<String>,
    pub last_success_ms: Option<u64>,
    pub retry_at_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageState {
    pub accounts: Vec<AccountTile>,
    pub current_by_group: BTreeMap<String, String>,
    pub provider_in_flight: BTreeMap<String, u32>,
    pub login: LoginState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountTile {
    pub id: String,
    pub display_name: String,
    pub provider: Provider,
    pub current: bool,
    pub paused: bool,
    pub healthy: bool,
    pub status: String,
    pub blocked_reason: Option<String>,
    pub in_flight: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_expiry: Option<TokenExpiry>,
    pub gauges: Vec<Gauge>,
    pub warning_level: WarningLevel,
    pub busy_action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenExpiry {
    pub state: String,
    pub expires_at_ms: u64,
    pub countdown_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GaugeKind {
    FiveHour,
    SevenDay,
    FableWeekly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gauge {
    pub kind: GaugeKind,
    pub available: bool,
    pub used_fraction: f64,
    pub remaining_fraction: f64,
    pub resets_at: Option<u64>,
    pub reset_text: Option<String>,
    pub constraining: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningLevel {
    Normal,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginState {
    pub phase: LoginPhase,
    pub provider: Option<String>,
    pub state: Option<String>,
    pub verification_uri: Option<String>,
    pub user_code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginPhase {
    #[default]
    Idle,
    Starting,
    Pending,
    Cancelling,
    Done,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatisticsState {
    pub overview: Value,
    pub models: Vec<Value>,
    pub clients: Vec<Value>,
    pub health: Vec<Value>,
    pub heatmaps: Vec<Value>,
    pub activity_receipts: Vec<ActivityReceipt>,
    pub data_quality: Value,
}

impl Default for StatisticsState {
    fn default() -> Self {
        Self {
            // Initial state is observable before the first daemon response and
            // must satisfy the same schema as every hydrated state. Native
            // shells use strict decoders and cannot treat missing counters as
            // implicit zeroes.
            overview: json!({
                "requests": 0,
                "ok": 0,
                "errors": 0,
                "tokens_in": 0,
                "tokens_out": 0,
                "rpm_5m": 0.0,
                "in_flight": 0,
                "cost_usd": 0.0
            }),
            models: Vec::new(),
            clients: Vec::new(),
            health: Vec::new(),
            heatmaps: Vec::new(),
            activity_receipts: Vec::new(),
            data_quality: json!({
                "model_usage": "hydrated activity/runtime",
                "windowed": "best effort",
                "cost": "API-equivalent estimate",
                "cache": "missing fields shown as unavailable"
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsState {
    pub email_anonymous: bool,
    pub show_fable_weekly: bool,
    pub api_key_configured: bool,
    pub sound_id: Option<String>,
    pub screens: Vec<Value>,
    pub sounds: Vec<Value>,
    pub events: Vec<Value>,
    pub autostart: Value,
    pub maintenance: Value,
    pub capabilities: Value,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            email_anonymous: false,
            show_fable_weekly: true,
            api_key_configured: false,
            sound_id: None,
            screens: Vec::new(),
            sounds: Vec::new(),
            events: Vec::new(),
            autostart: json!({ "enabled": null }),
            maintenance: json!({
                "channel": null,
                "version": null,
                "islands_version": env!("CARGO_PKG_VERSION"),
                "latest_version": null,
                "update_available": null,
                "install_owner": null,
                "license": "MIT",
                "source_url": "https://github.com/2lab-ai/llmux"
            }),
            capabilities: json!({
                "presentation": Presentation::Regular,
                "remote": false,
                "layer_shell": {
                    "available": false,
                    "reason": "platform capability not reported"
                },
                "tray": {
                    "available": false,
                    "reason": "platform capability not reported"
                },
                "notifications": {
                    "available": false,
                    "reason": "platform capability not reported"
                }
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationState {
    pub id: String,
    pub kind: String,
    pub target_display: Option<String>,
    pub started_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub level: NoticeLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptKind {
    InFlight,
    Request,
    Note,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityReceipt {
    pub receipt_id: String,
    pub kind: ReceiptKind,
    pub occurred_at_ms: u64,
    pub status: Option<u16>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub account_display: Option<String>,
    pub provider: Option<Provider>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fast: bool,
    pub tokens: Option<ReceiptTokens>,
    pub cache: Option<ReceiptCache>,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub message: Option<String>,
    pub error: bool,
    /// Tenant attribution id + resolved client display name (activity
    /// client-name) from the dashboard doc's completed rows. Additive:
    /// `Option` fields decode as `None` when absent, so older serialized
    /// states and strict native decoders keep parsing without a schema bump.
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub client_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptTokens {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptCache {
    pub read: Option<u64>,
    pub creation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOperation {
    Login,
    AddAccount,
    RemoveAccount,
    PauseAccount,
    Settings,
    Event,
    Maintenance,
    Autostart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    Succeeded,
    Failed,
    Cancelled,
    NoChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventDraft {
    pub id: String,
    pub from: String,
    pub to: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalSettingsChange {
    ScreenSelected {
        id: String,
    },
    SoundSelected {
        id: String,
    },
    ShowFable {
        enabled: bool,
    },
    ConnectionApplied {
        endpoint: String,
        api_key: Option<SecretString>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChannel {
    Stable,
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MaintenanceCommand {
    Update,
    ChangeChannel { channel: ReleaseChannel },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationReceipt {
    pub id: String,
    pub operation: VerificationOperation,
    pub target_display: Option<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub outcome: OperationOutcome,
    pub message: String,
}

#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct DeriveOptions {
    pub endpoint_display: String,
    pub remote: bool,
    pub authenticated: bool,
    pub api_key_configured: bool,
    pub selected_screen_id: String,
    pub presentation: Presentation,
}

impl Default for DeriveOptions {
    fn default() -> Self {
        Self {
            endpoint_display: "http://127.0.0.1:3456".to_string(),
            remote: false,
            authenticated: true,
            api_key_configured: false,
            selected_screen_id: "auto".to_string(),
            presentation: Presentation::Regular,
        }
    }
}

impl fmt::Debug for DeriveOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeriveOptions")
            .field("endpoint_display", &"[SANITIZED ENDPOINT]")
            .field("remote", &self.remote)
            .field("authenticated", &self.authenticated)
            .field("api_key_configured", &self.api_key_configured)
            .field("selected_screen_id", &self.selected_screen_id)
            .field("presentation", &self.presentation)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshSource {
    Startup,
    Manual,
    Poll,
    Retry,
    Mutation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginStatus {
    Pending {
        state: String,
        verification_uri: Option<String>,
        user_code: Option<String>,
        message: Option<String>,
    },
    Succeeded {
        target_display: Option<String>,
        message: String,
    },
    Failed {
        message: String,
    },
    Cancelled {
        message: String,
    },
    CancellationAcknowledged {
        message: String,
    },
    CancellationFailed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationRequest {
    AddAccount {
        name: Option<String>,
        api_key: SecretString,
    },
    PauseAccount {
        account_id: String,
        paused: bool,
    },
    RemoveAccount {
        account_id: String,
        confirmed: bool,
    },
    UpdateSettings {
        email_anonymous: bool,
    },
    UpsertEvent {
        event: EventDraft,
    },
    RemoveEvent {
        event_id: String,
    },
    PersistLocalSettings {
        change: LocalSettingsChange,
    },
    SetAutostart {
        enabled: bool,
    },
    RunMaintenance {
        command: MaintenanceCommand,
    },
}

impl OperationRequest {
    pub(crate) const fn verification_operation(&self) -> VerificationOperation {
        match self {
            Self::AddAccount { .. } => VerificationOperation::AddAccount,
            Self::PauseAccount { .. } => VerificationOperation::PauseAccount,
            Self::RemoveAccount { .. } => VerificationOperation::RemoveAccount,
            Self::UpdateSettings { .. } => VerificationOperation::Settings,
            Self::PersistLocalSettings { .. } => VerificationOperation::Settings,
            Self::UpsertEvent { .. } | Self::RemoveEvent { .. } => VerificationOperation::Event,
            Self::SetAutostart { .. } => VerificationOperation::Autostart,
            Self::RunMaintenance { .. } => VerificationOperation::Maintenance,
        }
    }

    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::AddAccount { .. } => "add_account",
            Self::PauseAccount { .. } => "pause_account",
            Self::RemoveAccount { .. } => "remove_account",
            Self::UpdateSettings { .. } => "settings",
            Self::PersistLocalSettings { .. } => "settings",
            Self::UpsertEvent { .. } => "event_upsert",
            Self::RemoveEvent { .. } => "event_remove",
            Self::SetAutostart { .. } => "autostart",
            Self::RunMaintenance { .. } => "maintenance",
        }
    }

    pub(crate) fn validation_error(&self) -> Option<&'static str> {
        match self {
            Self::AddAccount { api_key, .. } if api_key.is_empty() => Some("API key is required"),
            Self::PauseAccount { account_id, .. } | Self::RemoveAccount { account_id, .. }
                if account_id.trim().is_empty() =>
            {
                Some("account id is required")
            }
            Self::RemoveAccount {
                confirmed: false, ..
            } => Some("account removal requires confirmation"),
            Self::UpsertEvent { event } if event.id.trim().is_empty() => {
                Some("event id is required")
            }
            Self::UpsertEvent { event } if event.content.trim().is_empty() => {
                Some("event content is required")
            }
            Self::UpsertEvent { event }
                if llmux::event::parse_event_time(&event.from).is_none()
                    || llmux::event::parse_event_time(&event.to).is_none() =>
            {
                Some("event from/to must be valid daemon timestamp strings")
            }
            Self::UpsertEvent { event }
                if llmux::event::parse_event_time(&event.from)
                    >= llmux::event::parse_event_time(&event.to) =>
            {
                Some("event from must be earlier than to")
            }
            Self::RemoveEvent { event_id } if event_id.trim().is_empty() => {
                Some("event id is required")
            }
            Self::PersistLocalSettings {
                change:
                    LocalSettingsChange::ScreenSelected { id }
                    | LocalSettingsChange::SoundSelected { id },
            } if id.trim().is_empty() => Some("setting id is required"),
            Self::PersistLocalSettings {
                change: LocalSettingsChange::ConnectionApplied { endpoint, .. },
            } if endpoint.trim().is_empty() => Some("daemon endpoint is required"),
            _ => None,
        }
    }

    pub(crate) fn into_effect(self, operation_id: String) -> Effect {
        match self {
            Self::UpdateSettings { email_anonymous } => Effect::UpdateSettings {
                operation_id,
                email_anonymous,
            },
            Self::UpsertEvent { event } => Effect::UpsertEvent {
                operation_id,
                event,
            },
            Self::RemoveEvent { event_id } => Effect::RemoveEvent {
                operation_id,
                event_id,
            },
            Self::PersistLocalSettings { change } => Effect::PersistSettings {
                operation_id,
                change,
            },
            Self::SetAutostart { enabled } => Effect::SetAutostart {
                operation_id,
                enabled,
            },
            Self::RunMaintenance { command } => Effect::RunMaintenance {
                operation_id,
                command,
            },
            request => Effect::RunOperation {
                operation_id,
                request,
            },
        }
    }
}

#[derive(Clone)]
pub enum Action {
    AppStarted,
    TrayActivated,
    OpenRequested {
        reason: OpenReason,
    },
    CloseRequested,
    NavigationSelected {
        navigation: Navigation,
    },
    WindowMetricsChanged {
        width: u32,
        content_height: u32,
    },
    RefreshRequested {
        source: RefreshSource,
    },
    DashboardReceived {
        request_id: String,
        document: Box<DashboardDoc>,
        received_at_ms: u64,
    },
    DashboardFailed {
        request_id: String,
        error: String,
        failed_at_ms: u64,
    },
    LoginStarted {
        operation_id: String,
        provider: Provider,
        started_at_ms: u64,
    },
    LoginStatusReceived {
        operation_id: String,
        status: LoginStatus,
        at_ms: u64,
    },
    LoginCancelRequested {
        operation_id: String,
    },
    SettingsChanged {
        id: String,
        email_anonymous: bool,
        started_at_ms: u64,
    },
    EventUpsertRequested {
        id: String,
        event: EventDraft,
        started_at_ms: u64,
    },
    EventRemoveRequested {
        id: String,
        event_id: String,
        started_at_ms: u64,
    },
    AutostartChanged {
        id: String,
        enabled: bool,
        started_at_ms: u64,
    },
    MaintenanceRequested {
        id: String,
        command: MaintenanceCommand,
        started_at_ms: u64,
    },
    OperationStarted {
        id: String,
        request: OperationRequest,
        target_display: Option<String>,
        started_at_ms: u64,
    },
    OperationFinished {
        id: String,
        outcome: OperationOutcome,
        message: String,
        finished_at_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    EnsureLocalDaemon,
    FetchDashboard {
        request_id: String,
    },
    ScheduleDashboardRetry {
        retry_at_ms: u64,
    },
    CancelDashboardRetry,
    StartLogin {
        operation_id: String,
        provider: Provider,
    },
    PollLogin {
        operation_id: String,
        state: String,
    },
    CancelLogin {
        operation_id: String,
        state: String,
    },
    StopLoginPoll {
        operation_id: String,
    },
    RunOperation {
        operation_id: String,
        request: OperationRequest,
    },
    UpdateSettings {
        operation_id: String,
        email_anonymous: bool,
    },
    UpsertEvent {
        operation_id: String,
        event: EventDraft,
    },
    RemoveEvent {
        operation_id: String,
        event_id: String,
    },
    PersistSettings {
        operation_id: String,
        change: LocalSettingsChange,
    },
    SetAutostart {
        operation_id: String,
        enabled: bool,
    },
    RunMaintenance {
        operation_id: String,
        command: MaintenanceCommand,
    },
    UpdateTray {
        provider_in_flight: BTreeMap<String, u32>,
    },
}
