use llmux::dashboard::DashboardDoc;
use llmux_islands_core::{
    Action, Core, DeriveOptions, Effect, EventDraft, Lifecycle, LocalSettingsChange, LoginPhase,
    LoginStatus, MaintenanceCommand, Navigation, OpenReason, OperationOutcome, OperationRequest,
    Provider, RefreshSource, ReleaseChannel, SecretString, VerificationOperation,
    DASHBOARD_RETRY_BASE_MS, DASHBOARD_RETRY_MAX_MS, LOGIN_TIMEOUT_MS, MAX_CONTENT_HEIGHT,
    MAX_WINDOW_WIDTH, MIN_CONTENT_HEIGHT, MIN_WINDOW_WIDTH,
};

fn fixture() -> DashboardDoc {
    serde_json::from_str(include_str!("../fixtures/dashboard-current.json")).expect("fixture")
}

fn dashboard_request_id(effects: &[Effect]) -> String {
    match effects {
        [Effect::FetchDashboard { request_id }] => request_id.clone(),
        other => panic!("expected one dashboard effect, got {other:?}"),
    }
}

fn hydrate(core: &mut Core, at_ms: u64) {
    let request_id = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Startup,
    }));
    core.reduce(Action::DashboardReceived {
        request_id,
        document: Box::new(fixture()),
        received_at_ms: at_ms,
    });
}

#[test]
fn stale_dashboard_results_are_ignored_by_request_id() {
    let mut core = Core::new(DeriveOptions::default());
    let first = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Startup,
    }));
    core.reduce(Action::DashboardFailed {
        request_id: first.clone(),
        error: "timeout Authorization: Bearer sk-old-secret".into(),
        failed_at_ms: 10,
    });
    let second = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Retry,
    }));
    let before = core.state().clone();

    let stale_effects = core.reduce(Action::DashboardReceived {
        request_id: first,
        document: Box::new(fixture()),
        received_at_ms: 30,
    });
    assert!(stale_effects.is_empty());
    assert_eq!(core.state(), &before);

    core.reduce(Action::DashboardReceived {
        request_id: second,
        document: Box::new(fixture()),
        received_at_ms: 31,
    });
    assert_eq!(core.state().lifecycle, Lifecycle::Ready);
    assert_eq!(core.state().connection.last_success_ms, Some(31));
}

#[test]
fn grok_terminal_login_clears_verification_data_and_emits_one_receipt() {
    let mut core = Core::new(DeriveOptions::default());
    let start = core.reduce(Action::LoginStarted {
        operation_id: "login-1".into(),
        provider: Provider::Grok,
        started_at_ms: 100,
    });
    assert!(matches!(start.as_slice(), [Effect::StartLogin { .. }]));

    core.reduce(Action::LoginStatusReceived {
        operation_id: "login-1".into(),
        status: LoginStatus::Pending {
            state: "daemon-state".into(),
            verification_uri: Some("https://x.ai/device?secret=ephemeral".into()),
            user_code: Some("ABCD-EFGH".into()),
            message: Some("waiting".into()),
        },
        at_ms: 110,
    });
    assert_eq!(
        core.state().usage.login.user_code.as_deref(),
        Some("ABCD-EFGH")
    );
    assert_eq!(core.state().usage.login.state.as_deref(), Some("active"));

    core.reduce(Action::LoginStatusReceived {
        operation_id: "login-1".into(),
        status: LoginStatus::Pending {
            state: "daemon-state".into(),
            verification_uri: None,
            user_code: None,
            message: Some("login status unavailable; retrying".into()),
        },
        at_ms: 115,
    });
    assert_eq!(
        core.state().usage.login.verification_uri.as_deref(),
        Some("https://x.ai/device")
    );
    assert_eq!(
        core.state().usage.login.user_code.as_deref(),
        Some("ABCD-EFGH")
    );
    assert!(!serde_json::to_string(core.state())
        .expect("state JSON")
        .contains("daemon-state"));

    let terminal = core.reduce(Action::LoginStatusReceived {
        operation_id: "login-1".into(),
        status: LoginStatus::Succeeded {
            target_display: Some("alice@example.com".into()),
            message: "account added".into(),
        },
        at_ms: 120,
    });
    assert!(matches!(
        terminal.as_slice(),
        [Effect::StopLoginPoll { .. }, Effect::FetchDashboard { .. }]
    ));
    assert!(core.state().usage.login.state.is_none());
    assert!(core.state().usage.login.verification_uri.is_none());
    assert!(core.state().usage.login.user_code.is_none());
    assert_eq!(core.state().verification_receipts.len(), 1);
    assert_eq!(
        core.state().verification_receipts[0].operation,
        VerificationOperation::Login
    );
    assert_eq!(
        core.state().verification_receipts[0]
            .target_display
            .as_deref(),
        Some("a***@example.com")
    );

    let revision = core.state().revision;
    core.reduce(Action::LoginStatusReceived {
        operation_id: "login-1".into(),
        status: LoginStatus::Failed {
            message: "late failure".into(),
        },
        at_ms: 130,
    });
    assert_eq!(core.state().revision, revision);
    assert_eq!(core.state().verification_receipts.len(), 1);
}

#[test]
fn login_cancel_uses_the_daemon_state_and_late_success_is_ignored() {
    let mut core = Core::new(DeriveOptions::default());
    core.reduce(Action::LoginStarted {
        operation_id: "login-cancel".into(),
        provider: Provider::Grok,
        started_at_ms: 140,
    });
    let poll = core.reduce(Action::LoginStatusReceived {
        operation_id: "login-cancel".into(),
        status: LoginStatus::Pending {
            state: "daemon-cancel-state".into(),
            verification_uri: Some("https://x.ai/device".into()),
            user_code: Some("ABCD-EFGH".into()),
            message: None,
        },
        at_ms: 145,
    });
    assert!(matches!(
        poll.as_slice(),
        [Effect::PollLogin {
            operation_id,
            state
        }] if operation_id == "login-cancel" && state == "daemon-cancel-state"
    ));

    let cancel = core.reduce(Action::LoginCancelRequested {
        operation_id: "login-cancel".into(),
    });
    assert!(matches!(
        cancel.as_slice(),
        [
            Effect::StopLoginPoll { operation_id: stopped },
            Effect::CancelLogin {
                operation_id,
                state
            }
        ] if stopped == "login-cancel"
            && operation_id == "login-cancel"
            && state == "daemon-cancel-state"
    ));
    assert_eq!(core.state().usage.login.phase, LoginPhase::Cancelling);

    let before = core.state().clone();
    let raced = core.reduce(Action::LoginStatusReceived {
        operation_id: "login-cancel".into(),
        status: LoginStatus::Succeeded {
            target_display: Some("late@example.com".into()),
            message: "success raced with cancellation".into(),
        },
        at_ms: 147,
    });
    assert!(raced.is_empty());
    assert_eq!(core.state(), &before);

    let acknowledged = core.reduce(Action::LoginStatusReceived {
        operation_id: "login-cancel".into(),
        status: LoginStatus::CancellationAcknowledged {
            message: "cancelled".into(),
        },
        at_ms: 150,
    });
    assert!(
        acknowledged.is_empty(),
        "poll was already stopped at request time"
    );
    assert!(core.state().usage.login.verification_uri.is_none());
    assert!(core.state().usage.login.user_code.is_none());
    assert_eq!(core.state().verification_receipts.len(), 1);
    assert_eq!(
        core.state().verification_receipts[0].outcome,
        OperationOutcome::Cancelled
    );

    let revision = core.state().revision;
    core.reduce(Action::LoginStatusReceived {
        operation_id: "login-cancel".into(),
        status: LoginStatus::Succeeded {
            target_display: Some("late@example.com".into()),
            message: "late success".into(),
        },
        at_ms: 160,
    });
    assert_eq!(core.state().revision, revision);
    assert_eq!(core.state().verification_receipts.len(), 1);
}

#[test]
fn login_deadline_is_enforced_by_the_core_before_accepting_a_late_status() {
    let mut core = Core::new(DeriveOptions::default());
    let started_at_ms = 1_000;
    core.reduce(Action::LoginStarted {
        operation_id: "login-timeout".into(),
        provider: Provider::Grok,
        started_at_ms,
    });

    let terminal = core.reduce(Action::LoginStatusReceived {
        operation_id: "login-timeout".into(),
        status: LoginStatus::Succeeded {
            target_display: Some("too-late@example.com".into()),
            message: "late success".into(),
        },
        at_ms: started_at_ms + LOGIN_TIMEOUT_MS,
    });

    assert!(matches!(
        terminal.as_slice(),
        [Effect::StopLoginPoll { .. }]
    ));
    assert_eq!(core.state().usage.login.phase, LoginPhase::Error);
    assert_eq!(core.state().verification_receipts.len(), 1);
    assert_eq!(
        core.state().verification_receipts[0].outcome,
        OperationOutcome::Failed
    );
    assert!(core.state().verification_receipts[0]
        .message
        .contains("timed out"));
}

#[test]
fn successful_mutation_queues_one_mandatory_refresh_behind_an_active_poll() {
    let mut core = Core::new(DeriveOptions::default());
    let active_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Poll,
    }));
    assert!(core
        .reduce(Action::RefreshRequested {
            source: RefreshSource::Poll,
        })
        .is_empty());

    core.reduce(Action::SettingsChanged {
        id: "settings-during-poll".into(),
        email_anonymous: true,
        started_at_ms: 10,
    });
    assert!(core
        .reduce(Action::OperationFinished {
            id: "settings-during-poll".into(),
            outcome: OperationOutcome::Succeeded,
            message: "updated".into(),
            finished_at_ms: 20,
        })
        .is_empty());

    let settled = core.reduce(Action::DashboardReceived {
        request_id: active_request,
        document: Box::new(fixture()),
        received_at_ms: 30,
    });
    assert!(matches!(
        settled.as_slice(),
        [Effect::UpdateTray { .. }, Effect::FetchDashboard { request_id }]
            if request_id == "dashboard-2"
    ));
}

#[test]
fn successful_login_queues_one_mandatory_refresh_behind_an_active_poll() {
    let mut core = Core::new(DeriveOptions::default());
    let active_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Poll,
    }));
    core.reduce(Action::LoginStarted {
        operation_id: "login-during-poll".into(),
        provider: Provider::Grok,
        started_at_ms: 10,
    });

    let login_terminal = core.reduce(Action::LoginStatusReceived {
        operation_id: "login-during-poll".into(),
        status: LoginStatus::Succeeded {
            target_display: Some("new@example.com".into()),
            message: "added".into(),
        },
        at_ms: 20,
    });
    assert!(matches!(
        login_terminal.as_slice(),
        [Effect::StopLoginPoll { .. }]
    ));

    let settled = core.reduce(Action::DashboardFailed {
        request_id: active_request,
        error: "old poll failed".into(),
        failed_at_ms: 30,
    });
    assert!(matches!(
        settled.as_slice(),
        [Effect::ScheduleDashboardRetry { retry_at_ms }]
            if *retry_at_ms == 30 + DASHBOARD_RETRY_BASE_MS
    ));
}

#[test]
fn semantic_window_actions_are_clamped_idempotent_and_survive_dashboard_refresh() {
    let mut core = Core::new(DeriveOptions::default());

    core.reduce(Action::TrayActivated);
    assert!(core.state().window.open);
    assert_eq!(core.state().window.open_reason, OpenReason::Click);
    core.reduce(Action::OpenRequested {
        reason: OpenReason::Notification,
    });
    core.reduce(Action::NavigationSelected {
        navigation: Navigation::Statistics,
    });
    core.reduce(Action::WindowMetricsChanged {
        width: 0,
        content_height: u32::MAX,
    });
    assert_eq!(core.state().window.width, MIN_WINDOW_WIDTH);
    assert_eq!(core.state().window.content_height, MAX_CONTENT_HEIGHT);

    let revision = core.state().revision;
    assert!(core
        .reduce(Action::OpenRequested {
            reason: OpenReason::Notification,
        })
        .is_empty());
    assert!(core
        .reduce(Action::NavigationSelected {
            navigation: Navigation::Statistics,
        })
        .is_empty());
    assert!(core
        .reduce(Action::WindowMetricsChanged {
            width: MIN_WINDOW_WIDTH,
            content_height: u32::MAX,
        })
        .is_empty());
    assert_eq!(
        core.state().revision,
        revision,
        "identical shell feedback must not trigger a revision loop"
    );

    hydrate(&mut core, 500);
    assert!(core.state().window.open);
    assert_eq!(core.state().window.open_reason, OpenReason::Notification);
    assert_eq!(core.state().navigation, Navigation::Statistics);
    assert_eq!(core.state().window.width, MIN_WINDOW_WIDTH);
    assert_eq!(core.state().window.content_height, MAX_CONTENT_HEIGHT);

    core.reduce(Action::CloseRequested);
    assert!(!core.state().window.open);
    assert_eq!(core.state().window.open_reason, OpenReason::None);
    let revision = core.state().revision;
    core.reduce(Action::CloseRequested);
    assert_eq!(core.state().revision, revision);

    core.reduce(Action::WindowMetricsChanged {
        width: u32::MAX,
        content_height: 0,
    });
    assert_eq!(core.state().window.width, MAX_WINDOW_WIDTH);
    assert_eq!(core.state().window.content_height, MIN_CONTENT_HEIGHT);
}

#[test]
fn dashboard_failures_back_off_to_a_bound_and_reset_after_success() {
    let mut core = Core::new(DeriveOptions::default());
    let failure_times = [100, 1_000, 2_000, 3_000, 4_000, 5_000, 6_000];

    for (index, failed_at_ms) in failure_times.into_iter().enumerate() {
        let source = if index == 0 {
            RefreshSource::Startup
        } else {
            RefreshSource::Retry
        };
        let request_id = dashboard_request_id(&core.reduce(Action::RefreshRequested { source }));
        let delay = DASHBOARD_RETRY_BASE_MS
            .saturating_mul(1_u64 << index.min(5))
            .min(DASHBOARD_RETRY_MAX_MS);
        let effects = core.reduce(Action::DashboardFailed {
            request_id,
            error: "daemon unavailable".into(),
            failed_at_ms,
        });
        let expected_retry_at = failed_at_ms.saturating_add(delay);
        assert!(matches!(
            effects.as_slice(),
            [Effect::ScheduleDashboardRetry { retry_at_ms }]
                if *retry_at_ms == expected_retry_at
        ));
        assert_eq!(core.state().connection.retry_at_ms, Some(expected_retry_at));
        assert!(core
            .reduce(Action::RefreshRequested {
                source: RefreshSource::Poll,
            })
            .is_empty());
    }

    let recovery_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Retry,
    }));
    let recovery = core.reduce(Action::DashboardReceived {
        request_id: recovery_request,
        document: Box::new(fixture()),
        received_at_ms: 10_000,
    });
    assert!(matches!(
        recovery.as_slice(),
        [Effect::CancelDashboardRetry, Effect::UpdateTray { .. }]
    ));
    assert_eq!(core.state().connection.retry_at_ms, None);
    assert!(core
        .reduce(Action::RefreshRequested {
            source: RefreshSource::Retry,
        })
        .is_empty());

    let next_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Manual,
    }));
    let next_failure = core.reduce(Action::DashboardFailed {
        request_id: next_request,
        error: "offline again".into(),
        failed_at_ms: 20_000,
    });
    assert!(matches!(
        next_failure.as_slice(),
        [Effect::ScheduleDashboardRetry { retry_at_ms }]
            if *retry_at_ms == 20_000 + DASHBOARD_RETRY_BASE_MS
    ));
}

#[test]
fn privacy_on_masks_connection_errors_and_pending_login_messages() {
    let raw_email = "alice@example.com";
    let raw_account_id = "daemon-account-id-42";
    let mut document = fixture();
    document.accounts[1].name = raw_account_id.to_string();

    let mut failed_core = Core::new(DeriveOptions::default());
    let initial_request = dashboard_request_id(&failed_core.reduce(Action::RefreshRequested {
        source: RefreshSource::Startup,
    }));
    failed_core.reduce(Action::DashboardReceived {
        request_id: initial_request,
        document: Box::new(document.clone()),
        received_at_ms: 100,
    });
    let failed_request = dashboard_request_id(&failed_core.reduce(Action::RefreshRequested {
        source: RefreshSource::Manual,
    }));
    failed_core.reduce(Action::DashboardFailed {
        request_id: failed_request,
        error: format!("daemon {raw_email} / {raw_account_id} unavailable; HTTP 503"),
        failed_at_ms: 110,
    });
    assert_eq!(
        failed_core.state().connection.error.as_deref(),
        Some("daemon account-1 / account-2 unavailable; HTTP 503")
    );

    let mut login_core = Core::new(DeriveOptions::default());
    let initial_request = dashboard_request_id(&login_core.reduce(Action::RefreshRequested {
        source: RefreshSource::Startup,
    }));
    login_core.reduce(Action::DashboardReceived {
        request_id: initial_request,
        document: Box::new(document),
        received_at_ms: 100,
    });
    login_core.reduce(Action::LoginStarted {
        operation_id: "login-pending-private".into(),
        provider: Provider::Grok,
        started_at_ms: 120,
    });
    login_core.reduce(Action::LoginStatusReceived {
        operation_id: "login-pending-private".into(),
        status: LoginStatus::Pending {
            state: "executor-only-state".into(),
            verification_uri: Some("https://x.ai/device".into()),
            user_code: Some("ABCD-EFGH".into()),
            message: Some(format!(
                "waiting for {raw_email} / {raw_account_id}; HTTP 202"
            )),
        },
        at_ms: 130,
    });
    assert_eq!(
        login_core.state().usage.login.message.as_deref(),
        Some("waiting for account-1 / account-2; HTTP 202")
    );

    for state in [failed_core.state(), login_core.state()] {
        let serialized = serde_json::to_string(state).expect("anonymous state JSON");
        assert!(!serialized.contains(raw_email));
        assert!(!serialized.contains(raw_account_id));
    }
}

#[test]
fn privacy_alias_history_masks_removed_ids_in_unstructured_activity_and_login_text() {
    let retired = "retired-daemon-account-77";
    let mut first_document = fixture();
    first_document.accounts[1].name = retired.to_string();
    first_document
        .current_by_group
        .insert("codex".into(), retired.into());
    let mut core = Core::new(DeriveOptions::default());
    let first_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Startup,
    }));
    core.reduce(Action::DashboardReceived {
        request_id: first_request,
        document: Box::new(first_document),
        received_at_ms: 100,
    });

    let mut after_removal = fixture();
    after_removal.accounts.truncate(1);
    after_removal
        .current_by_group
        .insert("codex".into(), retired.into());
    let note = after_removal
        .activity
        .completed
        .iter_mut()
        .find_map(|row| match row {
            llmux::dashboard::CompletedDoc::Note { text, .. } => Some(text),
            llmux::dashboard::CompletedDoc::Request { .. } => None,
        })
        .expect("activity note fixture");
    *note = format!("cleanup finished for {retired}; HTTP 200");
    let second_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Manual,
    }));
    core.reduce(Action::DashboardReceived {
        request_id: second_request,
        document: Box::new(after_removal),
        received_at_ms: 110,
    });
    assert!(core
        .state()
        .statistics
        .activity_receipts
        .iter()
        .any(|receipt| receipt.message.as_deref()
            == Some("cleanup finished for account-2; HTTP 200")));
    assert!(core
        .state()
        .usage
        .current_by_group
        .values()
        .all(|account| account != "account-2"));

    core.reduce(Action::LoginStarted {
        operation_id: "login-after-removal".into(),
        provider: Provider::Grok,
        started_at_ms: 120,
    });
    core.reduce(Action::LoginStatusReceived {
        operation_id: "login-after-removal".into(),
        status: LoginStatus::Pending {
            state: "executor-only-state".into(),
            verification_uri: None,
            user_code: None,
            message: Some(format!("waiting for retired account {retired}; HTTP 202")),
        },
        at_ms: 130,
    });
    assert_eq!(
        core.state().usage.login.message.as_deref(),
        Some("waiting for retired account account-2; HTTP 202")
    );
    assert!(!serde_json::to_string(core.state())
        .expect("anonymous state JSON")
        .contains(retired));
}

#[test]
fn concurrent_user_mutations_emit_failed_receipts_without_replacing_active_state() {
    let mut core = Core::new(DeriveOptions::default());
    hydrate(&mut core, 100);
    let first_account = core.state().usage.accounts[0].id.clone();
    let second_account = core.state().usage.accounts[1].id.clone();

    assert!(matches!(
        core.reduce(Action::OperationStarted {
            id: "pause-active".into(),
            request: OperationRequest::PauseAccount {
                account_id: first_account,
                paused: true,
            },
            target_display: Some("alice@example.com".into()),
            started_at_ms: 110,
        })
        .as_slice(),
        [Effect::RunOperation { .. }]
    ));
    let active_state = core.state().operation.clone();

    assert!(core
        .reduce(Action::OperationStarted {
            id: "remove-concurrent".into(),
            request: OperationRequest::RemoveAccount {
                account_id: second_account,
                confirmed: true,
            },
            target_display: Some("codex@example.com".into()),
            started_at_ms: 111,
        })
        .is_empty());
    let rejected = core
        .state()
        .verification_receipts
        .last()
        .expect("concurrent operation receipt");
    assert_eq!(rejected.id, "remove-concurrent");
    assert_eq!(rejected.outcome, OperationOutcome::Failed);
    assert!(rejected.message.contains("already in progress"));
    assert_eq!(rejected.target_display.as_deref(), Some("account-2"));
    assert_eq!(core.state().operation, active_state);

    assert!(core
        .reduce(Action::LoginStarted {
            operation_id: "login-concurrent".into(),
            provider: Provider::Grok,
            started_at_ms: 112,
        })
        .is_empty());
    let rejected_login = core
        .state()
        .verification_receipts
        .last()
        .expect("concurrent login receipt");
    assert_eq!(rejected_login.id, "login-concurrent");
    assert_eq!(rejected_login.operation, VerificationOperation::Login);
    assert_eq!(rejected_login.outcome, OperationOutcome::Failed);
    assert_eq!(core.state().operation, active_state);
}

#[test]
fn terminal_account_operation_is_verified_and_late_results_are_ignored() {
    let mut core = Core::new(DeriveOptions::default());
    hydrate(&mut core, 190);
    let account_handle = core.state().usage.accounts[0].id.clone();
    let effects = core.reduce(Action::OperationStarted {
        id: "pause-1".into(),
        request: OperationRequest::PauseAccount {
            account_id: account_handle,
            paused: true,
        },
        target_display: Some("alice@example.com".into()),
        started_at_ms: 200,
    });
    assert!(matches!(effects.as_slice(), [Effect::RunOperation { .. }]));

    let terminal = core.reduce(Action::OperationFinished {
        id: "pause-1".into(),
        outcome: OperationOutcome::Succeeded,
        message: "paused".into(),
        finished_at_ms: 210,
    });
    assert!(matches!(
        terminal.as_slice(),
        [Effect::FetchDashboard { .. }]
    ));
    assert!(core.state().operation.is_none());
    assert_eq!(core.state().verification_receipts.len(), 1);
    assert_eq!(
        core.state().verification_receipts[0].operation,
        VerificationOperation::PauseAccount
    );

    let revision = core.state().revision;
    core.reduce(Action::OperationFinished {
        id: "pause-1".into(),
        outcome: OperationOutcome::Failed,
        message: "late Authorization: Bearer sk-secret".into(),
        finished_at_ms: 220,
    });
    assert_eq!(core.state().revision, revision);
    assert_eq!(core.state().verification_receipts.len(), 1);
}

#[test]
fn privacy_on_masks_email_and_non_email_ids_in_verification_receipt_messages() {
    for raw_account_id in ["alice@example.com", "daemon-account-id-42"] {
        let mut doc = fixture();
        doc.accounts[0].name = raw_account_id.to_string();
        doc.email_anonymous = true;

        let mut core = Core::new(DeriveOptions::default());
        let request_id = dashboard_request_id(&core.reduce(Action::RefreshRequested {
            source: RefreshSource::Startup,
        }));
        core.reduce(Action::DashboardReceived {
            request_id,
            document: Box::new(doc),
            received_at_ms: 100,
        });
        let account_handle = core.state().usage.accounts[0].id.clone();
        assert!(matches!(
            core.reduce(Action::OperationStarted {
                id: "pause-private".into(),
                request: OperationRequest::PauseAccount {
                    account_id: account_handle,
                    paused: true,
                },
                target_display: Some(raw_account_id.into()),
                started_at_ms: 110,
            })
            .as_slice(),
            [Effect::RunOperation {
                request: OperationRequest::PauseAccount { account_id, .. },
                ..
            }] if account_id == raw_account_id
        ));

        core.reduce(Action::OperationFinished {
            id: "pause-private".into(),
            outcome: OperationOutcome::Failed,
            message: format!("daemon rejected account {raw_account_id}; HTTP 409"),
            finished_at_ms: 120,
        });

        let receipt = &core.state().verification_receipts[0];
        assert_eq!(
            receipt.message,
            "daemon rejected account account-1; HTTP 409"
        );
        assert!(receipt
            .target_display
            .as_deref()
            .is_some_and(|target| !target.contains(raw_account_id)));
        assert!(!serde_json::to_string(core.state())
            .expect("anonymous state JSON")
            .contains(raw_account_id));
    }
}

#[test]
fn privacy_on_masks_a_new_non_email_account_target_before_dashboard_refresh() {
    let mut core = Core::new(DeriveOptions::default());
    hydrate(&mut core, 100);
    let raw_account_id = "new-daemon-account-id-99";
    core.reduce(Action::OperationStarted {
        id: "add-private".into(),
        request: OperationRequest::AddAccount {
            name: Some(raw_account_id.into()),
            api_key: SecretString::new("sk-test-account-secret"),
        },
        target_display: Some(raw_account_id.into()),
        started_at_ms: 110,
    });
    assert_eq!(
        core.state()
            .operation
            .as_ref()
            .and_then(|operation| operation.target_display.as_deref()),
        Some("anonymous")
    );
    core.reduce(Action::OperationFinished {
        id: "add-private".into(),
        outcome: OperationOutcome::Failed,
        message: format!("daemon rejected {raw_account_id}; HTTP 409"),
        finished_at_ms: 120,
    });

    let receipt = &core.state().verification_receipts[0];
    assert_eq!(receipt.target_display.as_deref(), Some("anonymous"));
    assert_eq!(receipt.message, "daemon rejected anonymous; HTTP 409");
    assert!(!serde_json::to_string(core.state())
        .expect("anonymous state JSON")
        .contains(raw_account_id));
}

#[test]
fn enabling_privacy_masks_an_active_operation_label_and_its_later_message() {
    let raw_account_id = "new-daemon-account-id-100";
    let mut visible_document = fixture();
    visible_document.email_anonymous = false;
    let mut core = Core::new(DeriveOptions::default());
    let initial_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Startup,
    }));
    core.reduce(Action::DashboardReceived {
        request_id: initial_request,
        document: Box::new(visible_document),
        received_at_ms: 100,
    });
    core.reduce(Action::OperationStarted {
        id: "add-across-privacy".into(),
        request: OperationRequest::AddAccount {
            name: Some(raw_account_id.into()),
            api_key: SecretString::new("sk-test-account-secret"),
        },
        target_display: Some(raw_account_id.into()),
        started_at_ms: 110,
    });
    assert_eq!(
        core.state()
            .operation
            .as_ref()
            .and_then(|operation| operation.target_display.as_deref()),
        Some(raw_account_id)
    );

    let refresh_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Manual,
    }));
    core.reduce(Action::DashboardReceived {
        request_id: refresh_request,
        document: Box::new(fixture()),
        received_at_ms: 120,
    });
    assert_eq!(
        core.state()
            .operation
            .as_ref()
            .and_then(|operation| operation.target_display.as_deref()),
        Some("anonymous")
    );

    core.reduce(Action::OperationFinished {
        id: "add-across-privacy".into(),
        outcome: OperationOutcome::Failed,
        message: format!("daemon rejected {raw_account_id}; HTTP 409"),
        finished_at_ms: 130,
    });
    assert_eq!(
        core.state().verification_receipts[0].message,
        "daemon rejected anonymous; HTTP 409"
    );
    assert!(!serde_json::to_string(core.state())
        .expect("anonymous state JSON")
        .contains(raw_account_id));
}

#[test]
fn privacy_on_masks_a_new_login_account_id_in_terminal_free_text() {
    let mut core = Core::new(DeriveOptions::default());
    hydrate(&mut core, 100);
    let raw_account_id = "new-login-account-id-99";
    core.reduce(Action::LoginStarted {
        operation_id: "login-private".into(),
        provider: Provider::Grok,
        started_at_ms: 110,
    });
    core.reduce(Action::LoginStatusReceived {
        operation_id: "login-private".into(),
        status: LoginStatus::Succeeded {
            target_display: Some(raw_account_id.into()),
            message: format!("connected {raw_account_id}; HTTP 200"),
        },
        at_ms: 120,
    });

    let receipt = &core.state().verification_receipts[0];
    assert_eq!(receipt.target_display.as_deref(), Some("anonymous"));
    assert_eq!(receipt.message, "connected anonymous; HTTP 200");
    assert!(!serde_json::to_string(core.state())
        .expect("anonymous state JSON")
        .contains(raw_account_id));
}

#[test]
fn enabling_privacy_rewrites_existing_verification_receipt_free_text() {
    let mut visible_doc = fixture();
    visible_doc.email_anonymous = false;
    let mut core = Core::new(DeriveOptions::default());
    let initial_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Startup,
    }));
    core.reduce(Action::DashboardReceived {
        request_id: initial_request,
        document: Box::new(visible_doc),
        received_at_ms: 100,
    });
    let raw_account_id = "alice@example.com";
    let account_id = core.state().usage.accounts[0].id.clone();
    core.reduce(Action::OperationStarted {
        id: "pause-before-privacy".into(),
        request: OperationRequest::PauseAccount {
            account_id,
            paused: true,
        },
        target_display: Some(raw_account_id.into()),
        started_at_ms: 110,
    });
    core.reduce(Action::OperationFinished {
        id: "pause-before-privacy".into(),
        outcome: OperationOutcome::Failed,
        message: format!("daemon rejected {raw_account_id}; HTTP 409"),
        finished_at_ms: 120,
    });
    assert!(core.state().verification_receipts[0]
        .message
        .contains(raw_account_id));

    let refresh_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Manual,
    }));
    core.reduce(Action::DashboardReceived {
        request_id: refresh_request,
        document: Box::new(fixture()),
        received_at_ms: 130,
    });

    let receipt = &core.state().verification_receipts[0];
    assert_eq!(receipt.message, "daemon rejected account-1; HTTP 409");
    assert!(!serde_json::to_string(core.state())
        .expect("anonymous state JSON")
        .contains(raw_account_id));
}

#[test]
fn account_mutation_marks_only_its_tile_busy_across_dashboard_refreshes() {
    let mut core = Core::new(DeriveOptions::default());
    let initial_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Startup,
    }));
    core.reduce(Action::DashboardReceived {
        request_id: initial_request,
        document: Box::new(fixture()),
        received_at_ms: 190,
    });

    let account_handle = core.state().usage.accounts[0].id.clone();
    let effects = core.reduce(Action::OperationStarted {
        id: "pause-busy".into(),
        request: OperationRequest::PauseAccount {
            account_id: account_handle.clone(),
            paused: true,
        },
        target_display: Some("alice@example.com".into()),
        started_at_ms: 200,
    });
    assert!(matches!(
        effects.as_slice(),
        [Effect::RunOperation {
            request: OperationRequest::PauseAccount { account_id, .. },
            ..
        }] if account_id == "alice@example.com"
    ));
    assert_eq!(
        core.state().usage.accounts[0].busy_action.as_deref(),
        Some("pause_account")
    );
    assert!(core.state().usage.accounts[1].busy_action.is_none());

    let refresh_request = dashboard_request_id(&core.reduce(Action::RefreshRequested {
        source: RefreshSource::Manual,
    }));
    core.reduce(Action::DashboardReceived {
        request_id: refresh_request,
        document: Box::new(fixture()),
        received_at_ms: 205,
    });
    assert_eq!(
        core.state().usage.accounts[0].busy_action.as_deref(),
        Some("pause_account"),
        "an unrelated dashboard response must not re-enable the active tile"
    );

    core.reduce(Action::OperationFinished {
        id: "pause-busy".into(),
        outcome: OperationOutcome::Failed,
        message: "daemon refused pause".into(),
        finished_at_ms: 210,
    });
    assert!(core.state().usage.accounts[0].busy_action.is_none());
}

#[test]
fn account_mutations_reject_unknown_or_raw_ids_at_the_ui_boundary() {
    let mut core = Core::new(DeriveOptions::default());
    hydrate(&mut core, 100);

    for (index, account_id) in ["alice@example.com", "account-unknown"]
        .into_iter()
        .enumerate()
    {
        let effects = core.reduce(Action::OperationStarted {
            id: format!("unknown-{index}"),
            request: OperationRequest::PauseAccount {
                account_id: account_id.into(),
                paused: true,
            },
            target_display: Some(account_id.into()),
            started_at_ms: 110 + index as u64,
        });
        assert!(effects.is_empty());
        let receipt = core
            .state()
            .verification_receipts
            .last()
            .expect("rejection receipt");
        assert_eq!(receipt.outcome, OperationOutcome::Failed);
        assert!(receipt.message.contains("unknown account handle"));
    }
}

#[test]
fn unconfirmed_remove_never_emits_a_mutating_effect() {
    let mut core = Core::new(DeriveOptions::default());
    let effects = core.reduce(Action::OperationStarted {
        id: "remove-1".into(),
        request: OperationRequest::RemoveAccount {
            account_id: "alice@example.com".into(),
            confirmed: false,
        },
        target_display: Some("alice@example.com".into()),
        started_at_ms: 300,
    });
    assert!(effects.is_empty());
    assert!(core.state().operation.is_none());
    assert_eq!(core.state().verification_receipts.len(), 1);
    assert_eq!(
        core.state().verification_receipts[0].outcome,
        OperationOutcome::Failed
    );
}

#[test]
fn typed_settings_result_ignores_a_stale_operation_id_then_emits_one_receipt() {
    let mut core = Core::new(DeriveOptions::default());
    let effects = core.reduce(Action::SettingsChanged {
        id: "settings-1".into(),
        email_anonymous: true,
        started_at_ms: 400,
    });
    assert!(matches!(
        effects.as_slice(),
        [Effect::UpdateSettings {
            operation_id,
            email_anonymous: true
        }] if operation_id == "settings-1"
    ));

    let before = core.state().clone();
    let stale = core.reduce(Action::OperationFinished {
        id: "settings-stale".into(),
        outcome: OperationOutcome::Succeeded,
        message: "late success".into(),
        finished_at_ms: 405,
    });
    assert!(stale.is_empty());
    assert_eq!(core.state(), &before);

    let terminal = core.reduce(Action::OperationFinished {
        id: "settings-1".into(),
        outcome: OperationOutcome::Succeeded,
        message: "email anonymity updated".into(),
        finished_at_ms: 410,
    });
    assert!(matches!(
        terminal.as_slice(),
        [Effect::FetchDashboard { .. }]
    ));
    assert_eq!(core.state().verification_receipts.len(), 1);
    assert_eq!(
        core.state().verification_receipts[0].operation,
        VerificationOperation::Settings
    );
}

#[test]
fn event_autostart_and_maintenance_requests_are_typed_and_validated() {
    let invalid_event = EventDraft {
        id: "launch".into(),
        from: "2026-07-15T18:00:00+09:00".into(),
        to: "2026-07-14T09:00:00+09:00".into(),
        content: "Launch window".into(),
    };
    let mut invalid_core = Core::new(DeriveOptions::default());
    let effects = invalid_core.reduce(Action::EventUpsertRequested {
        id: "event-invalid".into(),
        event: invalid_event,
        started_at_ms: 500,
    });
    assert!(effects.is_empty());
    assert_eq!(
        invalid_core.state().verification_receipts[0].operation,
        VerificationOperation::Event
    );
    assert_eq!(
        invalid_core.state().verification_receipts[0].outcome,
        OperationOutcome::Failed
    );

    let event = EventDraft {
        id: "launch".into(),
        from: "2026-07-14T09:00:00+09:00".into(),
        to: "202607151800".into(),
        content: "Launch window".into(),
    };
    let mut event_core = Core::new(DeriveOptions::default());
    assert!(matches!(
        event_core
            .reduce(Action::EventUpsertRequested {
                id: "event-1".into(),
                event: event.clone(),
                started_at_ms: 510,
            })
            .as_slice(),
        [Effect::UpsertEvent {
            operation_id,
            event: emitted
        }] if operation_id == "event-1" && emitted == &event
    ));
    event_core.reduce(Action::OperationFinished {
        id: "event-1".into(),
        outcome: OperationOutcome::Succeeded,
        message: "event updated".into(),
        finished_at_ms: 511,
    });
    assert_eq!(
        event_core.state().verification_receipts[0].operation,
        VerificationOperation::Event
    );
    let mut remove_core = Core::new(DeriveOptions::default());
    assert!(matches!(
        remove_core
            .reduce(Action::EventRemoveRequested {
                id: "event-2".into(),
                event_id: "launch".into(),
                started_at_ms: 520,
            })
            .as_slice(),
        [Effect::RemoveEvent {
            operation_id,
            event_id
        }] if operation_id == "event-2" && event_id == "launch"
    ));

    let mut autostart_core = Core::new(DeriveOptions::default());
    assert!(matches!(
        autostart_core
            .reduce(Action::AutostartChanged {
                id: "autostart-1".into(),
                enabled: true,
                started_at_ms: 530,
            })
            .as_slice(),
        [Effect::SetAutostart {
            operation_id,
            enabled: true
        }] if operation_id == "autostart-1"
    ));
    autostart_core.reduce(Action::OperationFinished {
        id: "autostart-1".into(),
        outcome: OperationOutcome::NoChange,
        message: "already enabled".into(),
        finished_at_ms: 531,
    });
    assert_eq!(
        autostart_core.state().verification_receipts[0].operation,
        VerificationOperation::Autostart
    );

    let mut maintenance_core = Core::new(DeriveOptions::default());
    let command = MaintenanceCommand::ChangeChannel {
        channel: ReleaseChannel::Preview,
    };
    assert!(matches!(
        maintenance_core
            .reduce(Action::MaintenanceRequested {
                id: "maintenance-1".into(),
                command,
                started_at_ms: 540,
            })
            .as_slice(),
        [Effect::RunMaintenance {
            operation_id,
            command: MaintenanceCommand::ChangeChannel {
                channel: ReleaseChannel::Preview
            }
        }] if operation_id == "maintenance-1"
    ));
    maintenance_core.reduce(Action::OperationFinished {
        id: "maintenance-1".into(),
        outcome: OperationOutcome::NoChange,
        message: "already on preview".into(),
        finished_at_ms: 541,
    });
    assert_eq!(
        maintenance_core.state().verification_receipts[0].operation,
        VerificationOperation::Maintenance
    );
}

#[test]
fn local_settings_cross_a_typed_platform_boundary_and_emit_a_settings_receipt() {
    let mut core = Core::new(DeriveOptions::default());
    let change = LocalSettingsChange::ShowFable { enabled: false };

    let effects = core.reduce(Action::OperationStarted {
        id: "local-settings-1".into(),
        request: OperationRequest::PersistLocalSettings {
            change: change.clone(),
        },
        target_display: Some("Fable weekly quota".into()),
        started_at_ms: 600,
    });
    assert!(matches!(
        effects.as_slice(),
        [Effect::PersistSettings {
            operation_id,
            change: emitted
        }] if operation_id == "local-settings-1" && emitted == &change
    ));

    core.reduce(Action::OperationFinished {
        id: "local-settings-1".into(),
        outcome: OperationOutcome::Succeeded,
        message: "local settings persisted".into(),
        finished_at_ms: 601,
    });
    assert_eq!(
        core.state().verification_receipts[0].operation,
        VerificationOperation::Settings
    );
}
