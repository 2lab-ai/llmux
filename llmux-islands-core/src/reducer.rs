use std::collections::BTreeMap;

use crate::contract::{
    Action, DeriveOptions, Effect, Lifecycle, LoginPhase, LoginStatus, Navigation, OpenReason,
    OperationOutcome, OperationRequest, OperationState, Provider, RefreshSource, UiState,
    VerificationOperation, VerificationReceipt, DASHBOARD_RETRY_BASE_MS, DASHBOARD_RETRY_MAX_MS,
    LOGIN_TIMEOUT_MS, MAX_CONTENT_HEIGHT, MAX_VERIFICATION_RECEIPTS, MAX_WINDOW_WIDTH,
    MIN_CONTENT_HEIGHT, MIN_WINDOW_WIDTH,
};
use crate::derive::derive_ui_state_with_account_handles;
use crate::privacy::{
    display_account, display_receipt_target, sanitize_account_text, sanitize_endpoint,
    sanitize_text,
};
use crate::receipts::from_activity_with_account_privacy;

pub struct Core {
    state: UiState,
    options: DeriveOptions,
    active_dashboard_request: Option<String>,
    dashboard_refresh_queued: bool,
    active_login: Option<PendingLogin>,
    active_operation: Option<PendingOperation>,
    dashboard_failure_count: u32,
    next_dashboard_id: u64,
    next_account_handle_id: u64,
    opaque_handle_by_raw: BTreeMap<String, String>,
    account_handle_by_raw: BTreeMap<String, String>,
    account_raw_by_handle: BTreeMap<String, String>,
}

struct PendingLogin {
    id: String,
    provider: Provider,
    started_at_ms: u64,
    state: Option<String>,
    cancelling: bool,
}

struct PendingOperation {
    id: String,
    operation: VerificationOperation,
    target_display: Option<String>,
    account_raw_id: Option<String>,
    busy_action: Option<String>,
    started_at_ms: u64,
}

impl Core {
    pub fn new(mut options: DeriveOptions) -> Self {
        options.endpoint_display = sanitize_endpoint(&options.endpoint_display);
        Self {
            state: UiState::initial(&options),
            options,
            active_dashboard_request: None,
            dashboard_refresh_queued: false,
            active_login: None,
            active_operation: None,
            dashboard_failure_count: 0,
            next_dashboard_id: 0,
            next_account_handle_id: 0,
            opaque_handle_by_raw: BTreeMap::new(),
            account_handle_by_raw: BTreeMap::new(),
            account_raw_by_handle: BTreeMap::new(),
        }
    }

    pub fn state(&self) -> &UiState {
        &self.state
    }

    /// Pure-state transition entrypoint. The caller executes returned effects
    /// and feeds typed result actions back into this reducer.
    pub fn reduce(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::AppStarted => self.app_started(),
            Action::TrayActivated => self.tray_activated(),
            Action::OpenRequested { reason } => self.open_requested(reason),
            Action::CloseRequested => self.close_requested(),
            Action::NavigationSelected { navigation } => self.navigation_selected(navigation),
            Action::WindowMetricsChanged {
                width,
                content_height,
            } => self.window_metrics_changed(width, content_height),
            Action::RefreshRequested { source } => self.refresh_requested(source),
            Action::DashboardReceived {
                request_id,
                document,
                received_at_ms,
            } => self.dashboard_received(&request_id, &document, received_at_ms),
            Action::DashboardFailed {
                request_id,
                error,
                failed_at_ms,
            } => self.dashboard_failed(&request_id, &error, failed_at_ms),
            Action::LoginStarted {
                operation_id,
                provider,
                started_at_ms,
            } => self.login_started(operation_id, provider, started_at_ms),
            Action::LoginStatusReceived {
                operation_id,
                status,
                at_ms,
            } => self.login_status_received(&operation_id, status, at_ms),
            Action::LoginCancelRequested { operation_id } => {
                self.login_cancel_requested(&operation_id)
            }
            Action::SettingsChanged {
                id,
                email_anonymous,
                started_at_ms,
            } => self.operation_started(
                id,
                OperationRequest::UpdateSettings { email_anonymous },
                Some("email anonymity".to_string()),
                started_at_ms,
            ),
            Action::EventUpsertRequested {
                id,
                event,
                started_at_ms,
            } => {
                let target = Some(event.id.clone());
                self.operation_started(
                    id,
                    OperationRequest::UpsertEvent { event },
                    target,
                    started_at_ms,
                )
            }
            Action::EventRemoveRequested {
                id,
                event_id,
                started_at_ms,
            } => {
                let target = Some(event_id.clone());
                self.operation_started(
                    id,
                    OperationRequest::RemoveEvent { event_id },
                    target,
                    started_at_ms,
                )
            }
            Action::AutostartChanged {
                id,
                enabled,
                started_at_ms,
            } => self.operation_started(
                id,
                OperationRequest::SetAutostart { enabled },
                Some("autostart".to_string()),
                started_at_ms,
            ),
            Action::MaintenanceRequested {
                id,
                command,
                started_at_ms,
            } => self.operation_started(
                id,
                OperationRequest::RunMaintenance { command },
                Some("maintenance".to_string()),
                started_at_ms,
            ),
            Action::OperationStarted {
                id,
                request,
                target_display,
                started_at_ms,
            } => self.operation_started(id, request, target_display, started_at_ms),
            Action::OperationFinished {
                id,
                outcome,
                message,
                finished_at_ms,
            } => self.operation_finished(&id, outcome, &message, finished_at_ms),
        }
    }

    fn app_started(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        if !self.options.remote {
            effects.push(Effect::EnsureLocalDaemon);
        }
        if let Some(effect) = self.request_dashboard() {
            effects.push(effect);
        }
        effects
    }

    fn tray_activated(&mut self) -> Vec<Effect> {
        if self.state.window.open {
            self.state.window.open = false;
            self.state.window.open_reason = OpenReason::None;
        } else {
            self.state.window.open = true;
            self.state.window.open_reason = OpenReason::Click;
        }
        self.bump_revision();
        Vec::new()
    }

    fn open_requested(&mut self, reason: OpenReason) -> Vec<Effect> {
        if self.state.window.open && self.state.window.open_reason == reason {
            return Vec::new();
        }
        self.state.window.open = true;
        self.state.window.open_reason = reason;
        self.bump_revision();
        Vec::new()
    }

    fn close_requested(&mut self) -> Vec<Effect> {
        if !self.state.window.open && self.state.window.open_reason == OpenReason::None {
            return Vec::new();
        }
        self.state.window.open = false;
        self.state.window.open_reason = OpenReason::None;
        self.bump_revision();
        Vec::new()
    }

    fn navigation_selected(&mut self, navigation: Navigation) -> Vec<Effect> {
        if self.state.navigation == navigation {
            return Vec::new();
        }
        self.state.navigation = navigation;
        self.bump_revision();
        Vec::new()
    }

    fn window_metrics_changed(&mut self, width: u32, content_height: u32) -> Vec<Effect> {
        let width = width.clamp(MIN_WINDOW_WIDTH, MAX_WINDOW_WIDTH);
        let content_height = content_height.clamp(MIN_CONTENT_HEIGHT, MAX_CONTENT_HEIGHT);
        if self.state.window.width == width && self.state.window.content_height == content_height {
            return Vec::new();
        }
        self.state.window.width = width;
        self.state.window.content_height = content_height;
        self.bump_revision();
        Vec::new()
    }

    fn refresh_requested(&mut self, source: RefreshSource) -> Vec<Effect> {
        if (source == RefreshSource::Poll && self.state.connection.retry_at_ms.is_some())
            || (source == RefreshSource::Retry && self.state.connection.retry_at_ms.is_none())
        {
            return Vec::new();
        }
        self.request_dashboard().into_iter().collect()
    }

    fn request_dashboard(&mut self) -> Option<Effect> {
        if self.active_dashboard_request.is_some() {
            return None;
        }
        self.next_dashboard_id = self.next_dashboard_id.saturating_add(1);
        let request_id = format!("dashboard-{}", self.next_dashboard_id);
        self.active_dashboard_request = Some(request_id.clone());
        Some(Effect::FetchDashboard { request_id })
    }

    fn request_mandatory_dashboard(&mut self) -> Option<Effect> {
        if self.active_dashboard_request.is_some() {
            self.dashboard_refresh_queued = true;
            None
        } else {
            self.request_dashboard()
        }
    }

    fn append_queued_dashboard(&mut self, effects: &mut Vec<Effect>) {
        if !self.dashboard_refresh_queued {
            return;
        }
        self.dashboard_refresh_queued = false;
        if let Some(effect) = self.request_dashboard() {
            effects.push(effect);
        }
    }

    fn dashboard_received(
        &mut self,
        request_id: &str,
        document: &llmux::dashboard::DashboardDoc,
        received_at_ms: u64,
    ) -> Vec<Effect> {
        if self.active_dashboard_request.as_deref() != Some(request_id) {
            return Vec::new();
        }
        self.active_dashboard_request = None;
        self.rebuild_account_handles(document);

        let old_revision = self.state.revision;
        let mut login = self.state.usage.login.clone();
        if let Some(message) = &mut login.message {
            *message = sanitize_account_text(
                message,
                document.email_anonymous,
                self.account_text_handles(document.email_anonymous),
            );
        }
        let mut operation = self.state.operation.clone();
        if let Some(target_display) = operation
            .as_mut()
            .and_then(|operation| operation.target_display.as_mut())
        {
            *target_display = self.sanitize_operation_target(
                self.active_operation
                    .as_ref()
                    .map_or(VerificationOperation::Settings, |pending| pending.operation),
                target_display,
                document.email_anonymous,
            );
        }
        let mut notices = self.state.notices.clone();
        for notice in &mut notices {
            notice.message = sanitize_account_text(
                &notice.message,
                document.email_anonymous,
                self.account_text_handles(document.email_anonymous),
            );
        }
        let mut receipts = self.state.verification_receipts.clone();
        for receipt in &mut receipts {
            self.sanitize_verification_receipt(receipt, document.email_anonymous);
        }
        let open = self.state.window.open;
        let open_reason = self.state.window.open_reason;
        let navigation = self.state.navigation;
        let width = self.state.window.width;
        let content_height = self.state.window.content_height;
        let had_scheduled_retry = self.state.connection.retry_at_ms.is_some();

        let mut next = derive_ui_state_with_account_handles(
            document,
            &self.options,
            received_at_ms,
            &self.account_handle_by_raw,
        );
        // Structural ids must resolve only against the current roster. Fold
        // activity receipts a second time with historical aliases solely for
        // privacy-safe free text from removed accounts.
        next.statistics.activity_receipts = from_activity_with_account_privacy(
            &document.activity,
            received_at_ms,
            document.email_anonymous,
            self.account_text_handles(document.email_anonymous),
        );
        next.revision = old_revision.saturating_add(1);
        next.usage.login = login;
        next.operation = operation;
        next.notices = notices;
        next.verification_receipts = receipts;
        next.window.open = open;
        next.window.open_reason = open_reason;
        next.window.width = width;
        next.window.content_height = content_height;
        next.navigation = navigation;
        if let Some(pending) = &self.active_operation {
            let account_handle = pending
                .account_raw_id
                .as_deref()
                .and_then(|raw_id| self.account_handle_by_raw.get(raw_id))
                .map(String::as_str);
            mark_account_busy(&mut next, account_handle, pending.busy_action.as_deref());
        }
        self.state = next;
        self.dashboard_failure_count = 0;

        let mut effects = Vec::new();
        if had_scheduled_retry {
            effects.push(Effect::CancelDashboardRetry);
        }
        effects.push(Effect::UpdateTray {
            provider_in_flight: self.state.window.provider_in_flight.clone(),
        });
        self.append_queued_dashboard(&mut effects);
        effects
    }

    fn dashboard_failed(
        &mut self,
        request_id: &str,
        error: &str,
        failed_at_ms: u64,
    ) -> Vec<Effect> {
        if self.active_dashboard_request.as_deref() != Some(request_id) {
            return Vec::new();
        }
        self.active_dashboard_request = None;
        self.dashboard_failure_count = self.dashboard_failure_count.saturating_add(1);
        let exponent = self.dashboard_failure_count.saturating_sub(1).min(5);
        let delay_ms = DASHBOARD_RETRY_BASE_MS
            .saturating_mul(1_u64 << exponent)
            .min(DASHBOARD_RETRY_MAX_MS);
        let retry_at_ms = failed_at_ms.saturating_add(delay_ms);
        self.state.lifecycle = Lifecycle::Offline;
        self.state.connection.error = Some(sanitize_account_text(
            error,
            self.state.settings.email_anonymous,
            self.account_text_handles(self.state.settings.email_anonymous),
        ));
        self.state.connection.retry_at_ms = Some(retry_at_ms);
        self.bump_revision();
        // A scheduled retry also satisfies any mutation-triggered refresh that
        // was queued behind the failed request. Coalesce the two instead of
        // bypassing backoff with an immediate second fetch.
        self.dashboard_refresh_queued = false;
        vec![Effect::ScheduleDashboardRetry { retry_at_ms }]
    }

    fn login_started(
        &mut self,
        operation_id: String,
        provider: Provider,
        started_at_ms: u64,
    ) -> Vec<Effect> {
        if self.active_login.is_some() || self.active_operation.is_some() {
            return self.reject_operation(
                operation_id,
                VerificationOperation::Login,
                Some(provider.key().to_string()),
                started_at_ms,
                "operation rejected: another operation is already in progress",
            );
        }
        self.active_login = Some(PendingLogin {
            id: operation_id.clone(),
            provider,
            started_at_ms,
            state: None,
            cancelling: false,
        });
        self.state.usage.login.phase = LoginPhase::Starting;
        self.state.usage.login.provider = Some(provider.key().to_string());
        self.state.usage.login.state = None;
        self.state.usage.login.verification_uri = None;
        self.state.usage.login.user_code = None;
        self.state.usage.login.message = None;
        self.state.operation = Some(OperationState {
            id: operation_id.clone(),
            kind: "login".to_string(),
            target_display: Some(provider.key().to_string()),
            started_at_ms,
        });
        self.bump_revision();
        vec![Effect::StartLogin {
            operation_id,
            provider,
        }]
    }

    fn login_status_received(
        &mut self,
        operation_id: &str,
        status: LoginStatus,
        at_ms: u64,
    ) -> Vec<Effect> {
        if self.active_login.as_ref().map(|login| login.id.as_str()) != Some(operation_id) {
            return Vec::new();
        }
        let (started_at_ms, cancelling) = self
            .active_login
            .as_ref()
            .map(|login| (login.started_at_ms, login.cancelling))
            .unwrap_or_default();
        if at_ms.saturating_sub(started_at_ms) >= LOGIN_TIMEOUT_MS {
            return self.finish_login(
                operation_id,
                OperationOutcome::Failed,
                None,
                "login timed out after 5 minutes",
                at_ms,
            );
        }
        if cancelling {
            return match status {
                LoginStatus::CancellationAcknowledged { message } => self.finish_login(
                    operation_id,
                    OperationOutcome::Cancelled,
                    None,
                    &message,
                    at_ms,
                ),
                LoginStatus::CancellationFailed { message } => self.finish_login(
                    operation_id,
                    OperationOutcome::Failed,
                    None,
                    &message,
                    at_ms,
                ),
                LoginStatus::Pending { .. }
                | LoginStatus::Succeeded { .. }
                | LoginStatus::Failed { .. }
                | LoginStatus::Cancelled { .. } => Vec::new(),
            };
        }
        match status {
            LoginStatus::Pending {
                state,
                verification_uri,
                user_code,
                message,
            } => {
                if let Some(login) = self.active_login.as_mut() {
                    login.state = Some(state.clone());
                }
                self.state.usage.login.phase = LoginPhase::Pending;
                // The daemon state is an executor-only correlation value.  A
                // shell only needs to know whether cancellation is available;
                // publishing the raw OAuth state in UiState would turn a
                // render model into a credential-bearing transport.
                self.state.usage.login.state = Some("active".to_string());
                // A transient poll failure is represented by Pending with no
                // new verification fields.  Keep the last provider-issued
                // URI/code so the user can still complete the device flow.
                if let Some(verification_uri) = verification_uri {
                    self.state.usage.login.verification_uri =
                        Some(sanitize_endpoint(&verification_uri));
                }
                if let Some(user_code) = user_code {
                    self.state.usage.login.user_code = Some(sanitize_text(&user_code));
                }
                self.state.usage.login.message = message.as_deref().map(|message| {
                    sanitize_account_text(
                        message,
                        self.state.settings.email_anonymous,
                        self.account_text_handles(self.state.settings.email_anonymous),
                    )
                });
                self.bump_revision();
                vec![Effect::PollLogin {
                    operation_id: operation_id.to_string(),
                    state,
                }]
            }
            LoginStatus::Succeeded {
                target_display,
                message,
            } => self.finish_login(
                operation_id,
                OperationOutcome::Succeeded,
                target_display,
                &message,
                at_ms,
            ),
            LoginStatus::Failed { message } => self.finish_login(
                operation_id,
                OperationOutcome::Failed,
                None,
                &message,
                at_ms,
            ),
            LoginStatus::Cancelled { message } => self.finish_login(
                operation_id,
                OperationOutcome::Cancelled,
                None,
                &message,
                at_ms,
            ),
            LoginStatus::CancellationAcknowledged { .. }
            | LoginStatus::CancellationFailed { .. } => Vec::new(),
        }
    }

    fn login_cancel_requested(&mut self, operation_id: &str) -> Vec<Effect> {
        let Some(pending) = self.active_login.as_mut() else {
            return Vec::new();
        };
        if pending.id != operation_id {
            return Vec::new();
        }
        let Some(state) = pending.state.clone() else {
            return Vec::new();
        };
        if pending.cancelling {
            return Vec::new();
        }
        pending.cancelling = true;
        self.state.usage.login.phase = LoginPhase::Cancelling;
        self.state.usage.login.message = Some("cancelling login".to_string());
        self.bump_revision();
        vec![
            Effect::StopLoginPoll {
                operation_id: operation_id.to_string(),
            },
            Effect::CancelLogin {
                operation_id: operation_id.to_string(),
                state,
            },
        ]
    }

    fn finish_login(
        &mut self,
        operation_id: &str,
        outcome: OperationOutcome,
        target_display: Option<String>,
        message: &str,
        finished_at_ms: u64,
    ) -> Vec<Effect> {
        let Some(pending) = self.active_login.take() else {
            return Vec::new();
        };
        let phase = match outcome {
            OperationOutcome::Succeeded | OperationOutcome::NoChange => LoginPhase::Done,
            OperationOutcome::Failed => LoginPhase::Error,
            OperationOutcome::Cancelled => LoginPhase::Cancelled,
        };
        self.state.usage.login.phase = phase;
        self.state.usage.login.provider = Some(pending.provider.key().to_string());
        self.state.usage.login.state = None;
        self.state.usage.login.verification_uri = None;
        self.state.usage.login.user_code = None;
        let safe_target = target_display
            .as_deref()
            .map(|target| self.account_receipt_target(target));
        let mut message_handles = self
            .account_text_handles(self.state.settings.email_anonymous)
            .clone();
        if self.state.settings.email_anonymous {
            if let (Some(raw_target), Some(safe_target)) =
                (target_display.as_deref(), safe_target.as_deref())
            {
                message_handles.insert(raw_target.to_string(), safe_target.to_string());
            }
        }
        let safe_message = sanitize_account_text(
            message,
            self.state.settings.email_anonymous,
            &message_handles,
        );
        self.state.usage.login.message = Some(safe_message.clone());
        self.state.operation = None;
        self.append_receipt(VerificationReceipt {
            id: operation_id.to_string(),
            operation: VerificationOperation::Login,
            target_display: safe_target,
            started_at_ms: pending.started_at_ms,
            finished_at_ms,
            outcome,
            message: safe_message,
        });
        self.bump_revision();

        let mut effects = Vec::new();
        if !pending.cancelling {
            effects.push(Effect::StopLoginPoll {
                operation_id: operation_id.to_string(),
            });
        }
        if outcome == OperationOutcome::Succeeded {
            if let Some(effect) = self.request_mandatory_dashboard() {
                effects.push(effect);
            }
        }
        effects
    }

    fn operation_started(
        &mut self,
        id: String,
        mut request: OperationRequest,
        target_display: Option<String>,
        started_at_ms: u64,
    ) -> Vec<Effect> {
        let operation = request.verification_operation();
        let mut safe_target = target_display.as_deref().map(|target| {
            self.sanitize_operation_target(operation, target, self.state.settings.email_anonymous)
        });
        if let Some(message) = request.validation_error() {
            self.append_receipt(VerificationReceipt {
                id,
                operation,
                target_display: safe_target,
                started_at_ms,
                finished_at_ms: started_at_ms,
                outcome: OperationOutcome::Failed,
                message: message.to_string(),
            });
            self.bump_revision();
            return Vec::new();
        }
        if self.active_operation.is_some() || self.active_login.is_some() {
            return self.reject_operation(
                id,
                operation,
                safe_target,
                started_at_ms,
                "operation rejected: another operation is already in progress",
            );
        }
        let (account_raw_id, busy_action) = match &mut request {
            OperationRequest::AddAccount { name, .. } => (name.clone(), None),
            OperationRequest::PauseAccount { account_id, .. } => {
                let Some(raw_id) = self.account_raw_by_handle.get(account_id).cloned() else {
                    return self.reject_operation(
                        id,
                        operation,
                        safe_target,
                        started_at_ms,
                        "unknown account handle",
                    );
                };
                safe_target = self.account_display_for_handle(account_id);
                *account_id = raw_id.clone();
                (Some(raw_id), Some("pause_account".to_string()))
            }
            OperationRequest::RemoveAccount { account_id, .. } => {
                let Some(raw_id) = self.account_raw_by_handle.get(account_id).cloned() else {
                    return self.reject_operation(
                        id,
                        operation,
                        safe_target,
                        started_at_ms,
                        "unknown account handle",
                    );
                };
                safe_target = self.account_display_for_handle(account_id);
                *account_id = raw_id.clone();
                (Some(raw_id), Some("remove_account".to_string()))
            }
            _ => (None, None),
        };
        self.active_operation = Some(PendingOperation {
            id: id.clone(),
            operation,
            target_display: safe_target.clone(),
            account_raw_id: account_raw_id.clone(),
            busy_action: busy_action.clone(),
            started_at_ms,
        });
        self.state.operation = Some(OperationState {
            id: id.clone(),
            kind: request.kind().to_string(),
            target_display: safe_target,
            started_at_ms,
        });
        let account_handle = account_raw_id
            .as_deref()
            .and_then(|raw_id| self.account_handle_by_raw.get(raw_id))
            .map(String::as_str);
        mark_account_busy(&mut self.state, account_handle, busy_action.as_deref());
        self.bump_revision();
        vec![request.into_effect(id)]
    }

    fn operation_finished(
        &mut self,
        id: &str,
        outcome: OperationOutcome,
        message: &str,
        finished_at_ms: u64,
    ) -> Vec<Effect> {
        if self
            .active_operation
            .as_ref()
            .map(|operation| operation.id.as_str())
            != Some(id)
        {
            return Vec::new();
        }
        let Some(pending) = self.active_operation.take() else {
            return Vec::new();
        };
        let account_handle = pending
            .account_raw_id
            .as_deref()
            .and_then(|raw_id| self.account_handle_by_raw.get(raw_id))
            .map(String::as_str);
        mark_account_busy(&mut self.state, account_handle, None);
        self.state.operation = None;
        let mut safe_message = message.to_string();
        if self.state.settings.email_anonymous {
            if let Some(raw_id) = pending.account_raw_id.as_deref() {
                let mut handles = self.account_text_handles(true).clone();
                handles.insert(raw_id.to_string(), self.private_account_target(raw_id));
                safe_message = sanitize_account_text(&safe_message, true, &handles);
            }
        }
        self.append_receipt(VerificationReceipt {
            id: id.to_string(),
            operation: pending.operation,
            target_display: pending.target_display,
            started_at_ms: pending.started_at_ms,
            finished_at_ms,
            outcome,
            message: safe_message,
        });
        self.bump_revision();
        if outcome == OperationOutcome::Succeeded {
            self.request_mandatory_dashboard().into_iter().collect()
        } else {
            Vec::new()
        }
    }

    fn append_receipt(&mut self, mut receipt: VerificationReceipt) {
        self.sanitize_verification_receipt(&mut receipt, self.state.settings.email_anonymous);
        if self.state.verification_receipts.len() >= MAX_VERIFICATION_RECEIPTS {
            self.state.verification_receipts.remove(0);
        }
        self.state.verification_receipts.push(receipt);
    }

    fn sanitize_verification_receipt(&self, receipt: &mut VerificationReceipt, anonymous: bool) {
        if anonymous && is_account_operation(receipt.operation) {
            if let Some(raw_target) = receipt.target_display.clone() {
                let safe_target = self.private_account_target(&raw_target);
                let mut handles = self.account_text_handles(true).clone();
                handles.insert(raw_target, safe_target.clone());
                receipt.message = sanitize_account_text(&receipt.message, true, &handles);
                receipt.target_display = Some(safe_target);
                return;
            }
        }
        receipt.message = sanitize_account_text(
            &receipt.message,
            anonymous,
            self.account_text_handles(anonymous),
        );
        if anonymous {
            receipt.target_display = receipt
                .target_display
                .as_deref()
                .map(|target| sanitize_account_text(target, true, self.account_text_handles(true)));
        }
    }

    fn account_receipt_target(&self, target: &str) -> String {
        if self.state.settings.email_anonymous {
            self.private_account_target(target)
        } else {
            display_receipt_target(target)
        }
    }

    fn sanitize_operation_target(
        &self,
        operation: VerificationOperation,
        target: &str,
        anonymous: bool,
    ) -> String {
        if !anonymous {
            return display_receipt_target(target);
        }
        if is_account_operation(operation) {
            self.private_account_target(target)
        } else {
            sanitize_account_text(target, true, self.account_text_handles(true))
        }
    }

    fn private_account_target(&self, target: &str) -> String {
        if self
            .opaque_handle_by_raw
            .values()
            .any(|handle| handle == target)
        {
            sanitize_text(target)
        } else {
            self.opaque_handle_by_raw
                .get(target)
                .cloned()
                .unwrap_or_else(|| display_account(target, true))
        }
    }

    fn account_text_handles(&self, anonymous: bool) -> &BTreeMap<String, String> {
        if anonymous {
            &self.opaque_handle_by_raw
        } else {
            &self.account_handle_by_raw
        }
    }

    fn bump_revision(&mut self) {
        self.state.revision = self.state.revision.saturating_add(1);
    }

    fn rebuild_account_handles(&mut self, document: &llmux::dashboard::DashboardDoc) {
        self.account_handle_by_raw.clear();
        self.account_raw_by_handle.clear();

        for account in &document.accounts {
            let opaque_handle = if let Some(handle) = self.opaque_handle_by_raw.get(&account.name) {
                handle.clone()
            } else {
                self.next_account_handle_id = self.next_account_handle_id.saturating_add(1);
                let handle = format!("account-{}", self.next_account_handle_id);
                self.opaque_handle_by_raw
                    .insert(account.name.clone(), handle.clone());
                handle
            };
            let handle = if document.email_anonymous {
                opaque_handle
            } else {
                account.name.clone()
            };
            self.account_handle_by_raw
                .insert(account.name.clone(), handle.clone());
            self.account_raw_by_handle
                .insert(handle, account.name.clone());
        }
    }

    fn account_display_for_handle(&self, handle: &str) -> Option<String> {
        self.state
            .usage
            .accounts
            .iter()
            .find(|account| account.id == handle)
            .map(|account| display_receipt_target(&account.display_name))
    }

    fn reject_operation(
        &mut self,
        id: String,
        operation: VerificationOperation,
        target_display: Option<String>,
        at_ms: u64,
        message: &str,
    ) -> Vec<Effect> {
        self.append_receipt(VerificationReceipt {
            id,
            operation,
            target_display,
            started_at_ms: at_ms,
            finished_at_ms: at_ms,
            outcome: OperationOutcome::Failed,
            message: message.to_string(),
        });
        self.bump_revision();
        Vec::new()
    }
}

fn is_account_operation(operation: VerificationOperation) -> bool {
    matches!(
        operation,
        VerificationOperation::Login
            | VerificationOperation::AddAccount
            | VerificationOperation::RemoveAccount
            | VerificationOperation::PauseAccount
    )
}

fn mark_account_busy(state: &mut UiState, account_id: Option<&str>, busy_action: Option<&str>) {
    let Some(account_id) = account_id else {
        return;
    };
    if let Some(account) = state
        .usage
        .accounts
        .iter_mut()
        .find(|account| account.id == account_id)
    {
        account.busy_action = busy_action.map(str::to_string);
    }
}
