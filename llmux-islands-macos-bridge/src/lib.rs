//! Length-delimited C ABI for embedding the shared Islands reducer in AppKit.
//!
//! The ABI deliberately exposes JSON at the language boundary: `UiState` is
//! already the versioned cross-platform contract, while a small audited C ABI
//! avoids adding a code generator and another runtime to the signed macOS app.

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::slice;
use std::str;
use std::sync::{Mutex, MutexGuard};

use llmux::dashboard::DashboardDoc;
use llmux_islands_core::{
    sanitize_endpoint, sanitize_text, Action, Core, DeriveOptions, Effect, EventDraft,
    LocalSettingsChange, LoginStatus, MaintenanceCommand, Navigation, OpenReason, OperationOutcome,
    OperationRequest, Presentation, Provider, RefreshSource, SecretString, UiState,
};
use serde::{Deserialize, Serialize};

const ABI_VERSION: u32 = 1;
const MAX_OPTIONS_BYTES: usize = 64 * 1024;
const MAX_ACTION_BYTES: usize = 256 * 1024;
const MAX_DASHBOARD_BYTES: usize = 32 * 1024 * 1024;
const MAX_REQUEST_ID_BYTES: usize = 4 * 1024;

/// ABI-owned byte slice. Its allocation must be released by
/// [`llmux_islands_owned_bytes_free`].
#[repr(C)]
pub struct LlmuxIslandsOwnedBytes {
    pub ptr: *mut u8,
    pub len: usize,
}

impl LlmuxIslandsOwnedBytes {
    const fn empty() -> Self {
        Self {
            ptr: ptr::null_mut(),
            len: 0,
        }
    }

    fn from_vec(bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            return Self::empty();
        }
        let mut bytes = bytes.into_boxed_slice();
        let result = Self {
            ptr: bytes.as_mut_ptr(),
            len: bytes.len(),
        };
        std::mem::forget(bytes);
        result
    }
}

/// Stable integer status returned by every fallible ABI call.
#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LlmuxIslandsStatus {
    Ok = 0,
    InvalidArgument = 1,
    InvalidJson = 2,
    InvalidAction = 3,
    Internal = 4,
    Panic = 5,
}

/// Opaque, internally synchronized reducer handle.
pub struct LlmuxIslandsBridge {
    inner: Mutex<BridgeCore>,
}

impl fmt::Debug for LlmuxIslandsBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LlmuxIslandsBridge([OPAQUE])")
    }
}

struct BridgeCore {
    core: Core,
    active_dashboard_request: Option<String>,
    pending_local_setting: Option<PendingLocalSetting>,
    platform_state: PlatformStateOverrides,
}

struct PendingLocalSetting {
    operation_id: String,
    change: PlatformStateChange,
}

enum PlatformStateChange {
    ScreenSelected(String),
    SoundSelected(String),
    ShowFable(bool),
}

#[derive(Default)]
struct PlatformStateOverrides {
    selected_screen_id: Option<String>,
    sound_id: Option<String>,
    show_fable_weekly: Option<bool>,
}

impl PlatformStateOverrides {
    fn apply(&self, state: &mut UiState) {
        if let Some(id) = &self.selected_screen_id {
            state.window.selected_screen_id.clone_from(id);
        }
        if let Some(id) = &self.sound_id {
            state.settings.sound_id = Some(id.clone());
        }
        if let Some(enabled) = self.show_fable_weekly {
            state.settings.show_fable_weekly = enabled;
        }
    }
}

impl BridgeCore {
    fn new(options: BridgeOptions) -> Self {
        let (core_options, platform_state) = options.into_parts();
        Self {
            core: Core::new(core_options),
            active_dashboard_request: None,
            pending_local_setting: None,
            platform_state,
        }
    }

    fn dispatch(&mut self, action: WireAction) -> Result<Vec<BridgeEffect>, BridgeError> {
        let completed_operation = match &action {
            WireAction::OperationFinished { id, outcome, .. }
                if self
                    .core
                    .state()
                    .operation
                    .as_ref()
                    .map(|operation| operation.id.as_str())
                    == Some(id.as_str()) =>
            {
                Some((id.clone(), *outcome))
            }
            _ => None,
        };
        let current_request_finished = action
            .dashboard_request_id()
            .is_some_and(|request_id| self.active_dashboard_request.as_deref() == Some(request_id));
        let effects = self.core.reduce(action.into_core());
        if let Some((operation_id, outcome)) = completed_operation {
            self.finish_local_setting(&operation_id, outcome);
        }
        if current_request_finished {
            self.active_dashboard_request = None;
        }
        self.project_effects(effects)
    }

    fn apply_dashboard(
        &mut self,
        request_id: String,
        document: DashboardDoc,
        received_at_ms: u64,
    ) -> Result<Vec<BridgeEffect>, BridgeError> {
        let is_current = self.active_dashboard_request.as_deref() == Some(request_id.as_str());
        let effects = self.core.reduce(Action::DashboardReceived {
            request_id,
            document: Box::new(document),
            received_at_ms,
        });
        if is_current {
            self.active_dashboard_request = None;
        }
        self.project_effects(effects)
    }

    fn project_effects(&mut self, effects: Vec<Effect>) -> Result<Vec<BridgeEffect>, BridgeError> {
        let projected: Vec<_> = effects
            .into_iter()
            .map(BridgeEffect::from_core)
            .collect::<Result<_, _>>()?;
        for effect in &projected {
            match effect {
                BridgeEffect::FetchDashboard { request_id } => {
                    self.active_dashboard_request = Some(request_id.clone());
                }
                BridgeEffect::PersistSettings {
                    operation_id,
                    change,
                } => {
                    self.pending_local_setting =
                        change
                            .platform_state_change()
                            .map(|change| PendingLocalSetting {
                                operation_id: operation_id.clone(),
                                change,
                            });
                }
                _ => {}
            }
        }
        Ok(projected)
    }

    fn finish_local_setting(&mut self, operation_id: &str, outcome: OperationOutcome) {
        let Some(pending) = self.pending_local_setting.take() else {
            return;
        };
        if pending.operation_id != operation_id {
            self.pending_local_setting = Some(pending);
            return;
        }
        if !matches!(
            outcome,
            OperationOutcome::Succeeded | OperationOutcome::NoChange
        ) {
            return;
        }
        match pending.change {
            PlatformStateChange::ScreenSelected(id) => {
                self.platform_state.selected_screen_id = Some(id);
            }
            PlatformStateChange::SoundSelected(id) => {
                self.platform_state.sound_id = Some(id);
            }
            PlatformStateChange::ShowFable(enabled) => {
                self.platform_state.show_fable_weekly = Some(enabled);
            }
        }
    }

    fn canonical_state(&self) -> UiState {
        let mut state = self.core.state().clone();
        self.platform_state.apply(&mut state);
        state
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeOptions {
    #[serde(default)]
    connection: ConnectionOptions,
    #[serde(default)]
    platform: PlatformOptions,
}

impl BridgeOptions {
    fn into_parts(self) -> (DeriveOptions, PlatformStateOverrides) {
        let selected_screen_id = sanitize_text(&self.platform.selected_screen_id);
        let selected_screen_id = if selected_screen_id.trim().is_empty() {
            "auto".to_string()
        } else {
            selected_screen_id
        };
        let sound_id = self
            .platform
            .sound_id
            .as_deref()
            .map(sanitize_text)
            .filter(|id| !id.trim().is_empty());
        let platform_state = PlatformStateOverrides {
            selected_screen_id: Some(selected_screen_id.clone()),
            sound_id,
            show_fable_weekly: self.platform.show_fable_weekly,
        };
        let core_options = DeriveOptions {
            endpoint_display: sanitize_endpoint(&self.connection.endpoint_display),
            remote: self.connection.remote,
            authenticated: self.connection.authenticated,
            api_key_configured: self.connection.api_key_configured,
            selected_screen_id,
            presentation: self.platform.presentation,
        };
        (core_options, platform_state)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionOptions {
    #[serde(default = "default_endpoint")]
    endpoint_display: String,
    #[serde(default)]
    remote: bool,
    #[serde(default = "default_true")]
    authenticated: bool,
    #[serde(default)]
    api_key_configured: bool,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            endpoint_display: default_endpoint(),
            remote: false,
            authenticated: true,
            api_key_configured: false,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformOptions {
    #[serde(default = "default_screen")]
    selected_screen_id: String,
    #[serde(default)]
    sound_id: Option<String>,
    #[serde(default)]
    show_fable_weekly: Option<bool>,
    #[serde(default = "default_presentation")]
    presentation: Presentation,
}

impl Default for PlatformOptions {
    fn default() -> Self {
        Self {
            selected_screen_id: default_screen(),
            sound_id: None,
            show_fable_weekly: None,
            presentation: default_presentation(),
        }
    }
}

fn default_endpoint() -> String {
    "http://127.0.0.1:3456".to_string()
}

fn default_screen() -> String {
    "auto".to_string()
}

const fn default_presentation() -> Presentation {
    Presentation::Regular
}

const fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireAction {
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
        source: RefreshSourceWire,
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
        status: LoginStatusWire,
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
    OperationStarted {
        id: String,
        request: OperationRequestWire,
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

impl WireAction {
    fn dashboard_request_id(&self) -> Option<&str> {
        match self {
            Self::DashboardFailed { request_id, .. } => Some(request_id),
            _ => None,
        }
    }

    fn into_core(self) -> Action {
        match self {
            Self::AppStarted => Action::AppStarted,
            Self::TrayActivated => Action::TrayActivated,
            Self::OpenRequested { reason } => Action::OpenRequested { reason },
            Self::CloseRequested => Action::CloseRequested,
            Self::NavigationSelected { navigation } => Action::NavigationSelected { navigation },
            Self::WindowMetricsChanged {
                width,
                content_height,
            } => Action::WindowMetricsChanged {
                width,
                content_height,
            },
            Self::RefreshRequested { source } => Action::RefreshRequested {
                source: source.into_core(),
            },
            Self::DashboardFailed {
                request_id,
                error,
                failed_at_ms,
            } => Action::DashboardFailed {
                request_id,
                error,
                failed_at_ms,
            },
            Self::LoginStarted {
                operation_id,
                provider,
                started_at_ms,
            } => Action::LoginStarted {
                operation_id,
                provider,
                started_at_ms,
            },
            Self::LoginStatusReceived {
                operation_id,
                status,
                at_ms,
            } => Action::LoginStatusReceived {
                operation_id,
                status: status.into_core(),
                at_ms,
            },
            Self::LoginCancelRequested { operation_id } => {
                Action::LoginCancelRequested { operation_id }
            }
            Self::SettingsChanged {
                id,
                email_anonymous,
                started_at_ms,
            } => Action::SettingsChanged {
                id,
                email_anonymous,
                started_at_ms,
            },
            Self::OperationStarted {
                id,
                request,
                target_display,
                started_at_ms,
            } => Action::OperationStarted {
                id,
                request: request.into_core(),
                target_display,
                started_at_ms,
            },
            Self::OperationFinished {
                id,
                outcome,
                message,
                finished_at_ms,
            } => Action::OperationFinished {
                id,
                outcome,
                message,
                finished_at_ms,
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum RefreshSourceWire {
    Startup,
    Manual,
    Poll,
    Retry,
    Mutation,
}

impl RefreshSourceWire {
    const fn into_core(self) -> RefreshSource {
        match self {
            Self::Startup => RefreshSource::Startup,
            Self::Manual => RefreshSource::Manual,
            Self::Poll => RefreshSource::Poll,
            Self::Retry => RefreshSource::Retry,
            Self::Mutation => RefreshSource::Mutation,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum LoginStatusWire {
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

impl LoginStatusWire {
    fn into_core(self) -> LoginStatus {
        match self {
            Self::Pending {
                state,
                verification_uri,
                user_code,
                message,
            } => LoginStatus::Pending {
                state,
                verification_uri,
                user_code,
                message,
            },
            Self::Succeeded {
                target_display,
                message,
            } => LoginStatus::Succeeded {
                target_display,
                message,
            },
            Self::Failed { message } => LoginStatus::Failed { message },
            Self::Cancelled { message } => LoginStatus::Cancelled { message },
            Self::CancellationAcknowledged { message } => {
                LoginStatus::CancellationAcknowledged { message }
            }
            Self::CancellationFailed { message } => LoginStatus::CancellationFailed { message },
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OperationRequestWire {
    AddAccount {
        name: Option<String>,
        has_api_key: bool,
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
    PersistScreen {
        id: String,
    },
    PersistSound {
        id: String,
    },
    PersistShowFable {
        enabled: bool,
    },
    PersistConnection {
        endpoint: String,
        api_key_configured: bool,
    },
    SetAutostart {
        enabled: bool,
    },
    RunMaintenance {
        command: MaintenanceCommand,
    },
}

impl OperationRequestWire {
    fn into_core(self) -> OperationRequest {
        match self {
            Self::AddAccount { name, has_api_key } => OperationRequest::AddAccount {
                name,
                api_key: SecretString::new(if has_api_key { "configured" } else { "" }),
            },
            Self::PauseAccount { account_id, paused } => {
                OperationRequest::PauseAccount { account_id, paused }
            }
            Self::RemoveAccount {
                account_id,
                confirmed,
            } => OperationRequest::RemoveAccount {
                account_id,
                confirmed,
            },
            Self::UpdateSettings { email_anonymous } => {
                OperationRequest::UpdateSettings { email_anonymous }
            }
            Self::UpsertEvent { event } => OperationRequest::UpsertEvent { event },
            Self::RemoveEvent { event_id } => OperationRequest::RemoveEvent { event_id },
            Self::PersistScreen { id } => OperationRequest::PersistLocalSettings {
                change: LocalSettingsChange::ScreenSelected { id },
            },
            Self::PersistSound { id } => OperationRequest::PersistLocalSettings {
                change: LocalSettingsChange::SoundSelected { id },
            },
            Self::PersistShowFable { enabled } => OperationRequest::PersistLocalSettings {
                change: LocalSettingsChange::ShowFable { enabled },
            },
            Self::PersistConnection {
                endpoint,
                api_key_configured,
            } => OperationRequest::PersistLocalSettings {
                change: LocalSettingsChange::ConnectionApplied {
                    endpoint,
                    api_key: api_key_configured.then(|| SecretString::new("configured")),
                },
            },
            Self::SetAutostart { enabled } => OperationRequest::SetAutostart { enabled },
            Self::RunMaintenance { command } => OperationRequest::RunMaintenance { command },
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BridgeEffect {
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
        provider: String,
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
        request: OperationEffect,
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
        change: LocalSettingsEffect,
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

impl BridgeEffect {
    fn from_core(effect: Effect) -> Result<Self, BridgeError> {
        Ok(match effect {
            Effect::EnsureLocalDaemon => Self::EnsureLocalDaemon,
            Effect::FetchDashboard { request_id } => Self::FetchDashboard { request_id },
            Effect::ScheduleDashboardRetry { retry_at_ms } => {
                Self::ScheduleDashboardRetry { retry_at_ms }
            }
            Effect::CancelDashboardRetry => Self::CancelDashboardRetry,
            Effect::StartLogin {
                operation_id,
                provider,
            } => Self::StartLogin {
                operation_id,
                provider: provider_name(provider).to_string(),
            },
            Effect::PollLogin {
                operation_id,
                state,
            } => Self::PollLogin {
                operation_id,
                state,
            },
            Effect::CancelLogin {
                operation_id,
                state,
            } => Self::CancelLogin {
                operation_id,
                state,
            },
            Effect::StopLoginPoll { operation_id } => Self::StopLoginPoll { operation_id },
            Effect::RunOperation {
                operation_id,
                request,
            } => Self::RunOperation {
                operation_id,
                request: OperationEffect::from_core(request)?,
            },
            Effect::UpdateSettings {
                operation_id,
                email_anonymous,
            } => Self::UpdateSettings {
                operation_id,
                email_anonymous,
            },
            Effect::UpsertEvent {
                operation_id,
                event,
            } => Self::UpsertEvent {
                operation_id,
                event,
            },
            Effect::RemoveEvent {
                operation_id,
                event_id,
            } => Self::RemoveEvent {
                operation_id,
                event_id,
            },
            Effect::PersistSettings {
                operation_id,
                change,
            } => Self::PersistSettings {
                operation_id,
                change: LocalSettingsEffect::from_core(change),
            },
            Effect::SetAutostart {
                operation_id,
                enabled,
            } => Self::SetAutostart {
                operation_id,
                enabled,
            },
            Effect::RunMaintenance {
                operation_id,
                command,
            } => Self::RunMaintenance {
                operation_id,
                command,
            },
            Effect::UpdateTray { provider_in_flight } => Self::UpdateTray { provider_in_flight },
        })
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum OperationEffect {
    #[serde(rename = "add_account")]
    Add {
        name: Option<String>,
        api_key_required: bool,
    },
    #[serde(rename = "pause_account")]
    Pause { account_id: String, paused: bool },
    #[serde(rename = "remove_account")]
    Remove { account_id: String, confirmed: bool },
}

impl OperationEffect {
    fn from_core(request: OperationRequest) -> Result<Self, BridgeError> {
        Ok(match request {
            OperationRequest::AddAccount { name, api_key } => Self::Add {
                name,
                api_key_required: !api_key.expose_secret().trim().is_empty(),
            },
            OperationRequest::PauseAccount { account_id, paused } => {
                Self::Pause { account_id, paused }
            }
            OperationRequest::RemoveAccount {
                account_id,
                confirmed,
            } => Self::Remove {
                account_id,
                confirmed,
            },
            // The core emits dedicated Effect variants for all other request
            // kinds. Fail closed if that typed contract ever regresses.
            OperationRequest::UpdateSettings { .. }
            | OperationRequest::UpsertEvent { .. }
            | OperationRequest::RemoveEvent { .. }
            | OperationRequest::PersistLocalSettings { .. }
            | OperationRequest::SetAutostart { .. }
            | OperationRequest::RunMaintenance { .. } => return Err(BridgeError::internal()),
        })
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LocalSettingsEffect {
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
        api_key_configured: bool,
    },
}

impl LocalSettingsEffect {
    fn from_core(change: LocalSettingsChange) -> Self {
        match change {
            LocalSettingsChange::ScreenSelected { id } => Self::ScreenSelected { id },
            LocalSettingsChange::SoundSelected { id } => Self::SoundSelected { id },
            LocalSettingsChange::ShowFable { enabled } => Self::ShowFable { enabled },
            LocalSettingsChange::ConnectionApplied { endpoint, api_key } => {
                Self::ConnectionApplied {
                    endpoint: sanitize_endpoint(&endpoint),
                    api_key_configured: api_key.is_some(),
                }
            }
        }
    }

    fn platform_state_change(&self) -> Option<PlatformStateChange> {
        match self {
            Self::ScreenSelected { id } => Some(PlatformStateChange::ScreenSelected(id.clone())),
            Self::SoundSelected { id } => Some(PlatformStateChange::SoundSelected(id.clone())),
            Self::ShowFable { enabled } => Some(PlatformStateChange::ShowFable(*enabled)),
            Self::ConnectionApplied { .. } => None,
        }
    }
}

const fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
        Provider::Grok => "grok",
        Provider::Api => "api",
        Provider::Unknown => "unknown",
    }
}

struct BridgeError {
    status: LlmuxIslandsStatus,
    code: &'static str,
    message: &'static str,
}

impl BridgeError {
    const fn new(status: LlmuxIslandsStatus, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }

    const fn invalid_argument(message: &'static str) -> Self {
        Self::new(
            LlmuxIslandsStatus::InvalidArgument,
            "invalid_argument",
            message,
        )
    }

    const fn invalid_json(message: &'static str) -> Self {
        Self::new(LlmuxIslandsStatus::InvalidJson, "invalid_json", message)
    }

    const fn invalid_action() -> Self {
        Self::new(
            LlmuxIslandsStatus::InvalidAction,
            "invalid_action",
            "unsupported or malformed semantic action",
        )
    }

    const fn internal() -> Self {
        Self::new(
            LlmuxIslandsStatus::Internal,
            "internal",
            "could not serialize bridge output",
        )
    }
}

#[derive(Serialize)]
struct ErrorPayload<'a> {
    code: &'a str,
    message: String,
}

fn parse_json_value(bytes: &[u8], message: &'static str) -> Result<serde_json::Value, BridgeError> {
    serde_json::from_slice(bytes).map_err(|_| BridgeError::invalid_json(message))
}

fn parse_options(bytes: &[u8]) -> Result<BridgeOptions, BridgeError> {
    let value = parse_json_value(bytes, "invalid bridge options JSON")?;
    serde_json::from_value(value)
        .map_err(|_| BridgeError::invalid_argument("unsupported or malformed bridge options"))
}

fn parse_action(bytes: &[u8]) -> Result<WireAction, BridgeError> {
    let value = parse_json_value(bytes, "invalid semantic action JSON")?;
    serde_json::from_value(value).map_err(|_| BridgeError::invalid_action())
}

fn parse_dashboard(bytes: &[u8]) -> Result<DashboardDoc, BridgeError> {
    serde_json::from_slice(bytes)
        .map_err(|_| BridgeError::invalid_json("invalid dashboard document JSON"))
}

fn serialize_transition(
    bridge: &BridgeCore,
    effects: &[BridgeEffect],
) -> Result<(Vec<u8>, Vec<u8>), BridgeError> {
    let state =
        serde_json::to_vec(&bridge.canonical_state()).map_err(|_| BridgeError::internal())?;
    let effects = serde_json::to_vec(effects).map_err(|_| BridgeError::internal())?;
    Ok((state, effects))
}

fn serialize_state(bridge: &BridgeCore) -> Result<Vec<u8>, BridgeError> {
    serde_json::to_vec(&bridge.canonical_state()).map_err(|_| BridgeError::internal())
}

unsafe fn input_bytes<'a>(
    input: *const u8,
    len: usize,
    maximum: usize,
    label: &'static str,
) -> Result<&'a [u8], BridgeError> {
    if input.is_null() {
        return Err(BridgeError::invalid_argument(label));
    }
    if len == 0 || len > maximum {
        return Err(BridgeError::invalid_argument(label));
    }
    // SAFETY: The C caller guarantees `input` points to `len` readable bytes
    // for the duration of this call; null and length bounds were checked.
    Ok(unsafe { slice::from_raw_parts(input, len) })
}

unsafe fn request_id(input: *const u8, len: usize) -> Result<String, BridgeError> {
    // SAFETY: Forwarded C-buffer preconditions are identical to input_bytes.
    let bytes = unsafe {
        input_bytes(
            input,
            len,
            MAX_REQUEST_ID_BYTES,
            "invalid dashboard request id",
        )?
    };
    let value = str::from_utf8(bytes)
        .map_err(|_| BridgeError::invalid_argument("invalid dashboard request id"))?;
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(BridgeError::invalid_argument(
            "invalid dashboard request id",
        ));
    }
    Ok(value.to_string())
}

unsafe fn lock_bridge<'a>(
    bridge: *mut LlmuxIslandsBridge,
) -> Result<MutexGuard<'a, BridgeCore>, BridgeError> {
    let bridge = unsafe { bridge.as_ref() }
        .ok_or_else(|| BridgeError::invalid_argument("bridge handle is required"))?;
    Ok(bridge
        .inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

unsafe fn clear_output(output: *mut LlmuxIslandsOwnedBytes) {
    if !output.is_null() {
        // SAFETY: The caller supplied writable storage for one output struct.
        unsafe { output.write(LlmuxIslandsOwnedBytes::empty()) };
    }
}

unsafe fn write_output(
    output: *mut LlmuxIslandsOwnedBytes,
    bytes: Vec<u8>,
) -> Result<(), BridgeError> {
    if output.is_null() {
        return Err(BridgeError::invalid_argument("output buffer is required"));
    }
    // SAFETY: The pointer was checked and the C caller promises writable
    // storage for one output struct.
    unsafe { output.write(LlmuxIslandsOwnedBytes::from_vec(bytes)) };
    Ok(())
}

unsafe fn write_error(output: *mut LlmuxIslandsOwnedBytes, error: &BridgeError) {
    if output.is_null() {
        return;
    }
    let payload = ErrorPayload {
        code: error.code,
        message: sanitize_text(error.message),
    };
    let bytes = serde_json::to_vec(&payload)
        .unwrap_or_else(|_| br#"{"code":"internal","message":"bridge operation failed"}"#.to_vec());
    // SAFETY: The caller supplied writable storage for one output struct.
    unsafe { output.write(LlmuxIslandsOwnedBytes::from_vec(bytes)) };
}

unsafe fn ffi_boundary<F>(
    out_error: *mut LlmuxIslandsOwnedBytes,
    operation: F,
) -> LlmuxIslandsStatus
where
    F: FnOnce() -> Result<(), BridgeError>,
{
    // SAFETY: Clearing the caller-owned output is part of every ABI call's
    // documented overwrite contract.
    unsafe { clear_output(out_error) };
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(())) => LlmuxIslandsStatus::Ok,
        Ok(Err(error)) => {
            // SAFETY: write_error is null-safe and writes one output struct.
            unsafe { write_error(out_error, &error) };
            error.status
        }
        Err(_) => {
            let error = BridgeError::new(
                LlmuxIslandsStatus::Panic,
                "panic",
                "bridge operation aborted safely",
            );
            // SAFETY: write_error is null-safe and writes one output struct.
            unsafe { write_error(out_error, &error) };
            LlmuxIslandsStatus::Panic
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn llmux_islands_bridge_abi_version() -> u32 {
    ABI_VERSION
}

/// Construct a reducer handle from sanitized connection/platform JSON.
///
/// # Safety
/// `options_json` must point to `options_len` readable bytes. `out_bridge`
/// and a non-null `out_error` must point to writable output storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn llmux_islands_bridge_new(
    options_json: *const u8,
    options_len: usize,
    out_bridge: *mut *mut LlmuxIslandsBridge,
    out_error: *mut LlmuxIslandsOwnedBytes,
) -> LlmuxIslandsStatus {
    if !out_bridge.is_null() {
        // SAFETY: The caller supplied writable storage for the handle.
        unsafe { out_bridge.write(ptr::null_mut()) };
    }
    // SAFETY: All raw-pointer access stays inside the guarded closure.
    unsafe {
        ffi_boundary(out_error, || {
            if out_bridge.is_null() {
                return Err(BridgeError::invalid_argument(
                    "bridge output handle is required",
                ));
            }
            let bytes = input_bytes(
                options_json,
                options_len,
                MAX_OPTIONS_BYTES,
                "invalid bridge options buffer",
            )?;
            let bridge = Box::new(LlmuxIslandsBridge {
                inner: Mutex::new(BridgeCore::new(parse_options(bytes)?)),
            });
            out_bridge.write(Box::into_raw(bridge));
            Ok(())
        })
    }
}

/// Reduce one semantic action and atomically return state plus executor effects.
///
/// # Safety
/// The handle must be live, `action_json` must point to `action_len` readable
/// bytes, and all non-null outputs must point to writable output storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn llmux_islands_bridge_dispatch(
    bridge: *mut LlmuxIslandsBridge,
    action_json: *const u8,
    action_len: usize,
    out_state_json: *mut LlmuxIslandsOwnedBytes,
    out_effects_json: *mut LlmuxIslandsOwnedBytes,
    out_error: *mut LlmuxIslandsOwnedBytes,
) -> LlmuxIslandsStatus {
    // SAFETY: The output structs are caller-owned writable storage.
    unsafe {
        clear_output(out_state_json);
        clear_output(out_effects_json);
        ffi_boundary(out_error, || {
            if out_state_json.is_null() || out_effects_json.is_null() {
                return Err(BridgeError::invalid_argument(
                    "state and effects outputs are required",
                ));
            }
            let bytes = input_bytes(
                action_json,
                action_len,
                MAX_ACTION_BYTES,
                "invalid semantic action buffer",
            )?;
            let action = parse_action(bytes)?;
            let mut bridge = lock_bridge(bridge)?;
            let effects = bridge.dispatch(action)?;
            let (state, effects) = serialize_transition(&bridge, &effects)?;
            write_output(out_state_json, state)?;
            write_output(out_effects_json, effects)?;
            Ok(())
        })
    }
}

/// Reduce one request-correlated dashboard document.
///
/// # Safety
/// The handle must be live, both inputs must point to their advertised readable
/// lengths, and all non-null outputs must point to writable output storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn llmux_islands_bridge_apply_dashboard(
    bridge: *mut LlmuxIslandsBridge,
    request_id_ptr: *const u8,
    request_id_len: usize,
    dashboard_json: *const u8,
    dashboard_len: usize,
    received_at_ms: u64,
    out_state_json: *mut LlmuxIslandsOwnedBytes,
    out_effects_json: *mut LlmuxIslandsOwnedBytes,
    out_error: *mut LlmuxIslandsOwnedBytes,
) -> LlmuxIslandsStatus {
    // SAFETY: The output structs are caller-owned writable storage.
    unsafe {
        clear_output(out_state_json);
        clear_output(out_effects_json);
        ffi_boundary(out_error, || {
            if out_state_json.is_null() || out_effects_json.is_null() {
                return Err(BridgeError::invalid_argument(
                    "state and effects outputs are required",
                ));
            }
            let request_id = request_id(request_id_ptr, request_id_len)?;
            let bytes = input_bytes(
                dashboard_json,
                dashboard_len,
                MAX_DASHBOARD_BYTES,
                "invalid dashboard document buffer",
            )?;
            let document = parse_dashboard(bytes)?;
            let mut bridge = lock_bridge(bridge)?;
            let effects = bridge.apply_dashboard(request_id, document, received_at_ms)?;
            let (state, effects) = serialize_transition(&bridge, &effects)?;
            write_output(out_state_json, state)?;
            write_output(out_effects_json, effects)?;
            Ok(())
        })
    }
}

/// Serialize the current canonical UiState without performing a transition.
///
/// # Safety
/// The handle must be live and a non-null state output must point to writable
/// storage. `out_error`, when non-null, must also be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn llmux_islands_bridge_state_json(
    bridge: *mut LlmuxIslandsBridge,
    out_state_json: *mut LlmuxIslandsOwnedBytes,
    out_error: *mut LlmuxIslandsOwnedBytes,
) -> LlmuxIslandsStatus {
    // SAFETY: The output struct is caller-owned writable storage.
    unsafe {
        clear_output(out_state_json);
        ffi_boundary(out_error, || {
            if out_state_json.is_null() {
                return Err(BridgeError::invalid_argument("state output is required"));
            }
            let bridge = lock_bridge(bridge)?;
            write_output(out_state_json, serialize_state(&bridge)?)?;
            Ok(())
        })
    }
}

/// Destroy one opaque bridge handle. Null is a no-op.
///
/// # Safety
/// A non-null pointer must be a live handle returned by
/// [`llmux_islands_bridge_new`], and no concurrent call may use it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn llmux_islands_bridge_free(bridge: *mut LlmuxIslandsBridge) {
    if bridge.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Ownership is transferred back exactly once by the caller.
        drop(unsafe { Box::from_raw(bridge) });
    }));
}

/// Zero and release one ABI-owned byte allocation. Null and already-cleared
/// structs are no-ops.
///
/// # Safety
/// A non-null pointer must reference writable storage. Its fields must either
/// be zero or be an allocation returned by this library and not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn llmux_islands_owned_bytes_free(bytes: *mut LlmuxIslandsOwnedBytes) {
    let Some(bytes) = (unsafe { bytes.as_mut() }) else {
        return;
    };
    if !bytes.ptr.is_null() && bytes.len > 0 {
        // SAFETY: The pointer/length pair was created from Box<[u8]> by this
        // library and the caller transfers it back exactly once.
        let raw = ptr::slice_from_raw_parts_mut(bytes.ptr, bytes.len);
        let mut allocation = unsafe { Box::from_raw(raw) };
        allocation.fill(0);
        drop(allocation);
    }
    bytes.ptr = ptr::null_mut();
    bytes.len = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_boundary_converts_unwind_to_sanitized_status() {
        let mut error = LlmuxIslandsOwnedBytes::empty();
        // SAFETY: `error` is valid writable output storage.
        let status = unsafe {
            ffi_boundary(&mut error, || -> Result<(), BridgeError> {
                panic!("sk-never-cross-the-boundary")
            })
        };
        assert!(status == LlmuxIslandsStatus::Panic);
        // SAFETY: error was returned by this module and remains live.
        let text = unsafe { str::from_utf8_unchecked(slice::from_raw_parts(error.ptr, error.len)) };
        assert!(!text.contains("sk-never-cross-the-boundary"));
        assert!(text.contains("aborted safely"));
        // SAFETY: error owns one allocation returned by this module.
        unsafe { llmux_islands_owned_bytes_free(&mut error) };
    }

    #[test]
    fn bridge_debug_is_constant_and_never_formats_executor_state() {
        let mut bridge = LlmuxIslandsBridge {
            inner: Mutex::new(BridgeCore::new(BridgeOptions::default())),
        };
        let action = WireAction::LoginStarted {
            operation_id: "raw-account@example.com".to_string(),
            provider: Provider::Grok,
            started_at_ms: 1,
        };
        let result = bridge
            .inner
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .dispatch(action);
        assert!(result.is_ok());
        let rendered = format!("{bridge:?}");
        assert_eq!(rendered, "LlmuxIslandsBridge([OPAQUE])");
        assert!(!rendered.contains("raw-account@example.com"));
    }

    #[test]
    fn every_executor_effect_has_a_stable_top_level_type_and_nested_kind() {
        let effects = vec![
            BridgeEffect::EnsureLocalDaemon,
            BridgeEffect::FetchDashboard {
                request_id: "dashboard-1".to_string(),
            },
            BridgeEffect::ScheduleDashboardRetry { retry_at_ms: 10 },
            BridgeEffect::CancelDashboardRetry,
            BridgeEffect::StartLogin {
                operation_id: "login-1".to_string(),
                provider: "grok".to_string(),
            },
            BridgeEffect::PollLogin {
                operation_id: "login-1".to_string(),
                state: "executor-only-state".to_string(),
            },
            BridgeEffect::CancelLogin {
                operation_id: "login-1".to_string(),
                state: "executor-only-state".to_string(),
            },
            BridgeEffect::StopLoginPoll {
                operation_id: "login-1".to_string(),
            },
            BridgeEffect::RunOperation {
                operation_id: "add-1".to_string(),
                request: OperationEffect::Add {
                    name: Some("work".to_string()),
                    api_key_required: true,
                },
            },
            BridgeEffect::RunOperation {
                operation_id: "pause-1".to_string(),
                request: OperationEffect::Pause {
                    account_id: "raw-account@example.com".to_string(),
                    paused: true,
                },
            },
            BridgeEffect::RunOperation {
                operation_id: "remove-1".to_string(),
                request: OperationEffect::Remove {
                    account_id: "raw-account@example.com".to_string(),
                    confirmed: true,
                },
            },
            BridgeEffect::UpdateSettings {
                operation_id: "settings-1".to_string(),
                email_anonymous: true,
            },
            BridgeEffect::UpsertEvent {
                operation_id: "event-1".to_string(),
                event: EventDraft {
                    id: "event".to_string(),
                    from: "2026-07-14T10:00:00Z".to_string(),
                    to: "2026-07-14T11:00:00Z".to_string(),
                    content: "content".to_string(),
                },
            },
            BridgeEffect::RemoveEvent {
                operation_id: "event-2".to_string(),
                event_id: "event".to_string(),
            },
            BridgeEffect::PersistSettings {
                operation_id: "screen-1".to_string(),
                change: LocalSettingsEffect::ScreenSelected {
                    id: "primary".to_string(),
                },
            },
            BridgeEffect::PersistSettings {
                operation_id: "sound-1".to_string(),
                change: LocalSettingsEffect::SoundSelected {
                    id: "default".to_string(),
                },
            },
            BridgeEffect::PersistSettings {
                operation_id: "fable-1".to_string(),
                change: LocalSettingsEffect::ShowFable { enabled: true },
            },
            BridgeEffect::PersistSettings {
                operation_id: "connection-1".to_string(),
                change: LocalSettingsEffect::ConnectionApplied {
                    endpoint: "https://daemon.example.com".to_string(),
                    api_key_configured: true,
                },
            },
            BridgeEffect::SetAutostart {
                operation_id: "autostart-1".to_string(),
                enabled: true,
            },
            BridgeEffect::RunMaintenance {
                operation_id: "update-1".to_string(),
                command: MaintenanceCommand::Update,
            },
            BridgeEffect::UpdateTray {
                provider_in_flight: BTreeMap::from([("claude".to_string(), 1)]),
            },
        ];
        let value = serde_json::to_value(&effects).expect("effect contract JSON");
        let rows = value.as_array().expect("effect array");
        assert!(rows.iter().all(|row| row["type"].is_string()));
        assert_eq!(rows[8]["request"]["kind"], "add_account");
        assert_eq!(rows[9]["request"]["kind"], "pause_account");
        assert_eq!(rows[10]["request"]["kind"], "remove_account");
        assert_eq!(rows[14]["change"]["kind"], "screen_selected");
        assert_eq!(rows[17]["change"]["kind"], "connection_applied");
        assert!(rows[8]["request"].get("api_key").is_none());
    }

    #[test]
    fn every_documented_action_shape_parses_and_reaches_the_reducer_boundary() {
        let mut actions = vec![
            serde_json::json!({ "type": "app_started" }),
            serde_json::json!({ "type": "tray_activated" }),
            serde_json::json!({ "type": "close_requested" }),
            serde_json::json!({
                "type": "window_metrics_changed",
                "width": 320,
                "content_height": 480
            }),
            serde_json::json!({
                "type": "dashboard_failed",
                "request_id": "dashboard-1",
                "error": "daemon unavailable",
                "failed_at_ms": 2
            }),
            serde_json::json!({
                "type": "login_started",
                "operation_id": "login-1",
                "provider": "grok",
                "started_at_ms": 1
            }),
            serde_json::json!({
                "type": "login_cancel_requested",
                "operation_id": "login-1"
            }),
            serde_json::json!({
                "type": "settings_changed",
                "id": "settings-1",
                "email_anonymous": true,
                "started_at_ms": 1
            }),
        ];
        for reason in ["click", "hover", "notification", "usage_alert", "boot"] {
            actions.push(serde_json::json!({
                "type": "open_requested",
                "reason": reason
            }));
        }
        for navigation in ["usage", "statistics", "menu"] {
            actions.push(serde_json::json!({
                "type": "navigation_selected",
                "navigation": navigation
            }));
        }
        for source in ["startup", "manual", "poll", "retry", "mutation"] {
            actions.push(serde_json::json!({
                "type": "refresh_requested",
                "source": source
            }));
        }
        actions.extend([
            serde_json::json!({
                "type": "login_status_received",
                "operation_id": "login-1",
                "status": {
                    "phase": "pending",
                    "state": "executor-state",
                    "verification_uri": "https://x.ai/device",
                    "user_code": "ABCD",
                    "message": "waiting"
                },
                "at_ms": 2
            }),
            serde_json::json!({
                "type": "login_status_received",
                "operation_id": "login-1",
                "status": {
                    "phase": "succeeded",
                    "target_display": "account@example.com",
                    "message": "login succeeded"
                },
                "at_ms": 2
            }),
        ]);
        for phase in [
            "failed",
            "cancelled",
            "cancellation_acknowledged",
            "cancellation_failed",
        ] {
            actions.push(serde_json::json!({
                "type": "login_status_received",
                "operation_id": "login-1",
                "status": { "phase": phase, "message": "terminal" },
                "at_ms": 2
            }));
        }
        let operation_requests = [
            serde_json::json!({
                "kind": "add_account",
                "name": "work",
                "has_api_key": true
            }),
            serde_json::json!({
                "kind": "pause_account",
                "account_id": "account-1",
                "paused": true
            }),
            serde_json::json!({
                "kind": "remove_account",
                "account_id": "account-1",
                "confirmed": true
            }),
            serde_json::json!({
                "kind": "update_settings",
                "email_anonymous": true
            }),
            serde_json::json!({
                "kind": "upsert_event",
                "event": {
                    "id": "event-1",
                    "from": "2026-07-14T10:00:00Z",
                    "to": "2026-07-14T11:00:00Z",
                    "content": "content"
                }
            }),
            serde_json::json!({ "kind": "remove_event", "event_id": "event-1" }),
            serde_json::json!({ "kind": "persist_screen", "id": "primary" }),
            serde_json::json!({ "kind": "persist_sound", "id": "default" }),
            serde_json::json!({ "kind": "persist_show_fable", "enabled": true }),
            serde_json::json!({
                "kind": "persist_connection",
                "endpoint": "https://daemon.example.com",
                "api_key_configured": true
            }),
            serde_json::json!({ "kind": "set_autostart", "enabled": true }),
            serde_json::json!({
                "kind": "run_maintenance",
                "command": { "kind": "update" }
            }),
            serde_json::json!({
                "kind": "run_maintenance",
                "command": { "kind": "change_channel", "channel": "preview" }
            }),
        ];
        for (index, request) in operation_requests.into_iter().enumerate() {
            actions.push(serde_json::json!({
                "type": "operation_started",
                "id": format!("operation-{index}"),
                "request": request,
                "target_display": "target",
                "started_at_ms": 1
            }));
        }
        for outcome in ["succeeded", "failed", "cancelled", "no_change"] {
            actions.push(serde_json::json!({
                "type": "operation_finished",
                "id": "operation-finish",
                "outcome": outcome,
                "message": "finished",
                "finished_at_ms": 2
            }));
        }

        for value in actions {
            let bytes = serde_json::to_vec(&value).expect("test action JSON");
            let action = parse_action(&bytes);
            assert!(action.is_ok(), "failed to parse {value}");
            let mut bridge = BridgeCore::new(BridgeOptions::default());
            let setup = bridge.dispatch(WireAction::AppStarted);
            assert!(setup.is_ok());
            match value["type"].as_str() {
                Some("login_status_received") | Some("login_cancel_requested") => {
                    let login = bridge.dispatch(WireAction::LoginStarted {
                        operation_id: "login-1".to_string(),
                        provider: Provider::Grok,
                        started_at_ms: 1,
                    });
                    assert!(login.is_ok());
                    if value["type"] == "login_cancel_requested" {
                        let pending = bridge.dispatch(WireAction::LoginStatusReceived {
                            operation_id: "login-1".to_string(),
                            status: LoginStatusWire::Pending {
                                state: "executor-state".to_string(),
                                verification_uri: None,
                                user_code: None,
                                message: None,
                            },
                            at_ms: 2,
                        });
                        assert!(pending.is_ok());
                    }
                }
                Some("operation_finished") => {
                    let operation = bridge.dispatch(WireAction::SettingsChanged {
                        id: "operation-finish".to_string(),
                        email_anonymous: true,
                        started_at_ms: 1,
                    });
                    assert!(operation.is_ok());
                }
                _ => {}
            }
            let result = bridge.dispatch(action.unwrap_or(WireAction::AppStarted));
            assert!(result.is_ok(), "failed to dispatch {value}");
            let serialized = result
                .ok()
                .and_then(|effects| serde_json::to_value(effects).ok());
            assert!(
                serialized.is_some(),
                "failed to serialize effects for {value}"
            );
        }
    }
}
