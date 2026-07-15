//! Platform-neutral state contract for llmux Islands.

mod client;
mod contract;
mod derive;
mod privacy;
mod receipts;
mod reducer;

pub use client::{
    ClientConfig, ClientError, ClientErrorKind, DaemonClient, EffectExecution, LoginStart,
    OperationAck,
};
pub use contract::{
    AccountTile, Action, ActivityReceipt, ConnectionState, DeriveOptions, Effect, EventDraft,
    Gauge, GaugeKind, Lifecycle, LocalSettingsChange, LoginPhase, LoginState, LoginStatus,
    MaintenanceCommand, Navigation, Notice, NoticeLevel, OpenReason, OperationOutcome,
    OperationRequest, OperationState, Presentation, Provider, ReceiptCache, ReceiptKind,
    ReceiptTokens, RefreshSource, ReleaseChannel, SecretString, SettingsState, StatisticsState,
    TokenExpiry, UiState, UsageState, VerificationOperation, VerificationReceipt, WarningLevel,
    WindowState, DASHBOARD_RETRY_BASE_MS, DASHBOARD_RETRY_MAX_MS, LOGIN_TIMEOUT_MS,
    MAX_CONTENT_HEIGHT, MAX_VERIFICATION_RECEIPTS, MAX_WINDOW_WIDTH, MIN_CONTENT_HEIGHT,
    MIN_WINDOW_WIDTH, UI_SCHEMA_VERSION,
};
pub use derive::derive_ui_state;
pub use privacy::{
    display_account, display_receipt_target, sanitize_endpoint, sanitize_path, sanitize_text,
};
pub use receipts::from_activity;
pub use reducer::Core;
