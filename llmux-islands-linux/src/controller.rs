use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use llmux_islands_core::{
    Action, ClientErrorKind, DaemonClient, Effect, EffectExecution, LocalSettingsChange,
    LoginStatus, MaintenanceCommand, OperationOutcome, Presentation, RefreshSource, ReleaseChannel,
};
use llmux_islands_linux::{
    desktop::{ensure_sibling_daemon, AutostartManager, AutostartState, Change},
    maintenance::{
        execute_maintenance, inspect_install_owner, InstallOwner, MaintenanceDisposition,
        MaintenanceIntent,
    },
    platform::{detect_surface_mode, SurfaceMode},
    settings::{ApiKey, LocalSettings, SaveOutcome, SettingsStore},
    snapshot::{self, SNAPSHOT_NOW_MS},
    ControllerConfig, ControllerModel, PlatformRequest, LOGIN_DEADLINE, LOGIN_POLL_INTERVAL,
};
use std::{
    collections::{BTreeMap, HashSet},
    env,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, state_json)]
        #[qproperty(QString, surface_mode)]
        #[qproperty(bool, smoke_mode)]
        #[qproperty(bool, snapshot_mode)]
        #[qproperty(QString, snapshot_dir)]
        #[qproperty(bool, autostart_enabled)]
        #[namespace = "llmux_islands"]
        type IslandsController = super::IslandsControllerRust;

        #[qinvokable]
        fn dispatch(self: Pin<&mut Self>, action: &QString, payload_json: &QString);

        #[qinvokable]
        fn exit_headless(self: Pin<&mut Self>, exit_code: i32);

        #[qinvokable]
        fn fail_headless(self: Pin<&mut Self>, message: &QString);

        #[qsignal]
        fn platform_command(self: Pin<&mut Self>, command: &QString, payload: &QString);
    }

    impl cxx_qt::Threading for IslandsController {}
}

pub struct IslandsControllerRust {
    state_json: QString,
    surface_mode: QString,
    smoke_mode: bool,
    snapshot_mode: bool,
    snapshot_dir: QString,
    autostart_enabled: bool,
    model: ControllerModel,
    client: Option<Arc<DaemonClient>>,
    endpoint: String,
    local_settings: LocalSettings,
    install_owner: String,
    cancelled_login_polls: Arc<Mutex<HashSet<String>>>,
    dashboard_retry_generation: Arc<AtomicU64>,
    last_provider_in_flight: Option<BTreeMap<String, u32>>,
}

struct PreparedConnection {
    endpoint: String,
    client: Option<Arc<DaemonClient>>,
    options: llmux_islands_core::DeriveOptions,
}

impl Default for IslandsControllerRust {
    fn default() -> Self {
        let snapshot_request = snapshot::active();
        let surface_mode = if snapshot_request.is_some() {
            SurfaceMode::RegularWindow
        } else {
            detect_surface_mode()
        };
        let presentation = presentation(surface_mode.as_str());
        let config = if snapshot_request.is_some() {
            ControllerConfig::from_values(llmux_islands_linux::DEFAULT_ENDPOINT, None, presentation)
        } else {
            ControllerConfig::from_env(presentation).or_else(|_| {
                ControllerConfig::from_values(
                    llmux_islands_linux::DEFAULT_ENDPOINT,
                    None,
                    presentation,
                )
            })
        };
        let local_settings = if snapshot_request.is_some() {
            LocalSettings::default()
        } else {
            match SettingsStore::discover().and_then(|store| store.load()) {
                Ok(settings) => settings,
                Err(_) => LocalSettings::default(),
            }
        };
        let executable = if snapshot_request.is_some() {
            None
        } else {
            env::current_exe().ok()
        };
        let autostart_enabled = if snapshot_request.is_some() {
            false
        } else {
            match (AutostartManager::from_env(), executable.as_deref()) {
                (Ok(manager), Some(executable)) => {
                    matches!(manager.readback(executable), Ok(AutostartState::Installed))
                }
                _ => false,
            }
        };
        let install_owner = if snapshot_request.is_some() {
            "snapshot-fixture".to_string()
        } else {
            executable.as_deref().map_or_else(
                || "unknown".to_string(),
                |executable| install_owner_label(&inspect_install_owner(executable)),
            )
        };

        let (options, endpoint, client) = match config {
            Ok(config) => {
                let client = if snapshot_request.is_some() {
                    None
                } else {
                    config.daemon_client().ok().map(Arc::new)
                };
                (
                    config.derive_options(),
                    config.endpoint().to_string(),
                    client,
                )
            }
            Err(_) => {
                let options = llmux_islands_core::DeriveOptions {
                    presentation,
                    ..llmux_islands_core::DeriveOptions::default()
                };
                (
                    options,
                    llmux_islands_linux::DEFAULT_ENDPOINT.to_string(),
                    None,
                )
            }
        };
        // Live shells start empty and are hydrated only by a real daemon receipt.
        // The checked-in document is reachable only through the explicit snapshot CLI.
        let mut model = if snapshot_request.is_some() {
            match ControllerModel::from_fixture(options, SNAPSHOT_NOW_MS) {
                Ok(model) => model,
                Err(error) => {
                    eprintln!("snapshot fixture error: {error}");
                    std::process::exit(2);
                }
            }
        } else {
            ControllerModel::new(options)
        };
        model.set_platform_state(
            local_settings.clone(),
            autostart_enabled,
            install_owner.clone(),
        );
        let state_json = match model.state_json() {
            Ok(state_json) => state_json,
            Err(_) => "{}".to_string(),
        };

        Self {
            state_json: QString::from(state_json.as_str()),
            surface_mode: QString::from(surface_mode.as_str()),
            smoke_mode: env::args().any(|argument| argument == "--smoke-test"),
            snapshot_mode: snapshot_request.is_some(),
            snapshot_dir: QString::from(
                snapshot_request.map_or("", |request| request.qml_output_directory()),
            ),
            autostart_enabled,
            model,
            client,
            endpoint,
            local_settings,
            install_owner,
            cancelled_login_polls: Arc::new(Mutex::new(HashSet::new())),
            dashboard_retry_generation: Arc::new(AtomicU64::new(0)),
            last_provider_in_flight: None,
        }
    }
}

impl qobject::IslandsController {
    pub fn exit_headless(self: Pin<&mut Self>, exit_code: i32) {
        if *self.smoke_mode() || *self.snapshot_mode() {
            snapshot::exit_immediately(exit_code);
        }
    }

    pub fn fail_headless(self: Pin<&mut Self>, message: &QString) {
        if *self.smoke_mode() || *self.snapshot_mode() {
            eprintln!("snapshot failure: {}", message.to_string());
            snapshot::exit_immediately(2);
        }
    }

    pub fn dispatch(mut self: Pin<&mut Self>, action: &QString, payload_json: &QString) {
        let action = action.to_string();
        let payload_json = payload_json.to_string();
        let outcome = self
            .as_mut()
            .rust_mut()
            .model
            .dispatch(&action, &payload_json, now_ms());
        self.as_mut().sync_state_json();

        for request in outcome.platform {
            self.as_mut().execute_direct_request(request);
        }
        if let Some(error) = outcome.error {
            self.as_mut()
                .emit_platform_command("dispatch_error", &error);
        }
        self.schedule_effects(outcome.effects);
    }

    fn apply_action(mut self: Pin<&mut Self>, action: Action) {
        let effects = self.as_mut().rust_mut().model.apply(action);
        self.as_mut().sync_state_json();
        self.schedule_effects(effects);
    }

    fn sync_state_json(mut self: Pin<&mut Self>) {
        let state_json = self.rust().model.state_json();
        if let Ok(state_json) = state_json {
            let state_json = QString::from(state_json.as_str());
            if self.state_json() != &state_json {
                self.as_mut().set_state_json(state_json);
            }
        }
    }

    fn schedule_effects(mut self: Pin<&mut Self>, mut effects: Vec<Effect>) {
        if matches!(effects.first(), Some(Effect::EnsureLocalDaemon)) {
            effects.remove(0);
            self.spawn_startup_sequence(effects);
            return;
        }
        for effect in effects {
            self.as_mut().schedule_effect(effect);
        }
    }

    fn spawn_startup_sequence(self: Pin<&mut Self>, effects: Vec<Effect>) {
        let qt_thread = self.qt_thread();
        let endpoint = self.rust().endpoint.clone();
        let client = self.rust().client.clone();
        std::thread::spawn(move || {
            let _ = ensure_sibling_daemon(&endpoint);
            for effect in effects {
                if let Some(action) = execute_daemon_effect(client.as_deref(), effect) {
                    queue_action(&qt_thread, action);
                }
            }
        });
    }

    fn schedule_effect(mut self: Pin<&mut Self>, effect: Effect) {
        match effect {
            Effect::EnsureLocalDaemon => self.spawn_startup_sequence(Vec::new()),
            Effect::ScheduleDashboardRetry { retry_at_ms } => {
                self.spawn_dashboard_retry(retry_at_ms)
            }
            Effect::CancelDashboardRetry => {
                self.rust()
                    .dashboard_retry_generation
                    .fetch_add(1, Ordering::SeqCst);
            }
            Effect::StopLoginPoll { operation_id } => {
                if let Ok(mut cancelled) = self.rust().cancelled_login_polls.lock() {
                    cancelled.insert(operation_id);
                }
            }
            Effect::PersistSettings {
                operation_id,
                change,
            } => self.spawn_settings_persistence(operation_id, change),
            Effect::SetAutostart {
                operation_id,
                enabled,
            } => self.spawn_autostart(operation_id, enabled),
            Effect::RunMaintenance {
                operation_id,
                command,
            } => self.spawn_maintenance(operation_id, command),
            Effect::UpdateTray { provider_in_flight } => {
                self.as_mut().update_tray(provider_in_flight)
            }
            Effect::PollLogin {
                operation_id,
                state,
            } => self.spawn_login_poll(operation_id, state),
            Effect::StartLogin {
                operation_id,
                provider,
            } => {
                if let Ok(mut cancelled) = self.rust().cancelled_login_polls.lock() {
                    cancelled.remove(&operation_id);
                }
                self.spawn_daemon_effect(Effect::StartLogin {
                    operation_id,
                    provider,
                });
            }
            effect => self.spawn_daemon_effect(effect),
        }
    }

    fn spawn_daemon_effect(self: Pin<&mut Self>, effect: Effect) {
        let qt_thread = self.qt_thread();
        let client = self.rust().client.clone();
        std::thread::spawn(move || {
            if let Some(action) = execute_daemon_effect(client.as_deref(), effect) {
                queue_action(&qt_thread, action);
            }
        });
    }

    fn spawn_dashboard_retry(self: Pin<&mut Self>, retry_at_ms: u64) {
        let qt_thread = self.qt_thread();
        let retry_generation = Arc::clone(&self.rust().dashboard_retry_generation);
        let generation = retry_generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        std::thread::spawn(move || {
            let delay_ms = retry_at_ms.saturating_sub(now_ms());
            if delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
            if retry_generation.load(Ordering::SeqCst) == generation {
                queue_action(
                    &qt_thread,
                    Action::RefreshRequested {
                        source: RefreshSource::Retry,
                    },
                );
            }
        });
    }

    fn spawn_login_poll(self: Pin<&mut Self>, operation_id: String, state: String) {
        let qt_thread = self.qt_thread();
        let client = self.rust().client.clone();
        let cancelled = Arc::clone(&self.rust().cancelled_login_polls);
        let started_at_ms = self
            .rust()
            .model
            .ui_state()
            .operation
            .as_ref()
            .filter(|operation| operation.id == operation_id)
            .map_or_else(now_ms, |operation| operation.started_at_ms);
        let deadline_ms = started_at_ms.saturating_add(LOGIN_DEADLINE.as_millis() as u64);
        std::thread::spawn(move || {
            std::thread::sleep(LOGIN_POLL_INTERVAL);
            if cancelled
                .lock()
                .is_ok_and(|cancelled| cancelled.contains(&operation_id))
            {
                return;
            }
            if now_ms() >= deadline_ms {
                queue_action(
                    &qt_thread,
                    Action::LoginStatusReceived {
                        operation_id,
                        status: LoginStatus::Failed {
                            message: "login timed out after five minutes".to_string(),
                        },
                        at_ms: now_ms(),
                    },
                );
                return;
            }
            let effect = Effect::PollLogin {
                operation_id,
                state,
            };
            if let Some(action) = execute_daemon_effect(client.as_deref(), effect) {
                queue_action(&qt_thread, action);
            }
        });
    }

    fn spawn_settings_persistence(
        self: Pin<&mut Self>,
        operation_id: String,
        change: LocalSettingsChange,
    ) {
        let qt_thread = self.qt_thread();
        let mut settings = self.rust().local_settings.clone();
        let connection_changed = matches!(&change, LocalSettingsChange::ConnectionApplied { .. });
        apply_local_settings_change(&mut settings, change);
        let presentation = presentation(&self.surface_mode().to_string());
        std::thread::spawn(move || {
            let prepared_connection = if connection_changed {
                prepare_connection(&settings, presentation).map(Some)
            } else {
                Ok(None)
            };
            let (outcome, message, applied, prepared_connection) = match prepared_connection {
                Err(message) => (OperationOutcome::Failed, message.to_string(), None, None),
                Ok(prepared_connection) => {
                    let result = SettingsStore::discover().and_then(|store| store.save(&settings));
                    match result {
                        Ok(SaveOutcome::Written) => (
                            OperationOutcome::Succeeded,
                            "local settings persisted".to_string(),
                            Some(settings),
                            prepared_connection,
                        ),
                        Ok(SaveOutcome::WrittenDurabilityUnknown) => (
                            OperationOutcome::Succeeded,
                            "local settings persisted; crash durability could not be confirmed"
                                .to_string(),
                            Some(settings),
                            prepared_connection,
                        ),
                        Ok(SaveOutcome::Unchanged) => (
                            OperationOutcome::NoChange,
                            "local settings were already current".to_string(),
                            Some(settings),
                            prepared_connection,
                        ),
                        Err(error) => (OperationOutcome::Failed, error.to_string(), None, None),
                    }
                }
            };
            let finished_at_ms = now_ms();
            let _ = qt_thread.queue(move |mut controller| {
                if let Some(settings) = applied {
                    controller
                        .as_mut()
                        .install_local_settings(settings, prepared_connection);
                }
                let effects =
                    controller
                        .as_mut()
                        .rust_mut()
                        .model
                        .apply(Action::OperationFinished {
                            id: operation_id,
                            outcome,
                            message,
                            finished_at_ms,
                        });
                controller.as_mut().sync_state_json();
                controller.schedule_effects(effects);
            });
        });
    }

    fn install_local_settings(
        mut self: Pin<&mut Self>,
        settings: LocalSettings,
        prepared_connection: Option<PreparedConnection>,
    ) {
        // A successful connection receipt causes core to fetch immediately, so
        // the replacement client must be visible before applying that receipt.
        if let Some(prepared) = prepared_connection {
            self.as_mut().rust_mut().endpoint = prepared.endpoint;
            self.as_mut().rust_mut().client = prepared.client;
            self.as_mut()
                .rust_mut()
                .model
                .set_connection_options(prepared.options);
        }
        let autostart_enabled = *self.autostart_enabled();
        let install_owner = self.rust().install_owner.clone();
        self.as_mut().rust_mut().local_settings = settings.clone();
        self.as_mut().rust_mut().model.set_platform_state(
            settings,
            autostart_enabled,
            install_owner,
        );
    }

    fn spawn_autostart(self: Pin<&mut Self>, operation_id: String, enabled: bool) {
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let result = AutostartManager::from_env().and_then(|manager| {
                let executable = env::current_exe()?;
                if enabled {
                    manager.install(&executable)
                } else {
                    manager.remove()
                }
            });
            let (outcome, message, applied) = match result {
                Ok(Change::Changed) => (
                    OperationOutcome::Succeeded,
                    if enabled {
                        "autostart enabled"
                    } else {
                        "autostart disabled"
                    }
                    .to_string(),
                    true,
                ),
                Ok(Change::Unchanged) => (
                    OperationOutcome::NoChange,
                    "autostart was already current".to_string(),
                    true,
                ),
                Err(_) => (
                    OperationOutcome::Failed,
                    "autostart operation failed".to_string(),
                    false,
                ),
            };
            let _ = qt_thread.queue(move |mut controller| {
                if applied {
                    controller.as_mut().set_autostart_enabled(enabled);
                    controller.as_mut().refresh_platform_state();
                }
                controller.apply_action(Action::OperationFinished {
                    id: operation_id,
                    outcome,
                    message,
                    finished_at_ms: now_ms(),
                });
            });
        });
    }

    fn spawn_maintenance(self: Pin<&mut Self>, operation_id: String, command: MaintenanceCommand) {
        let qt_thread = self.qt_thread();
        let executable = env::current_exe().ok();
        let mut local_settings = self.rust().local_settings.clone();
        std::thread::spawn(move || {
            let report = executable
                .as_deref()
                .map(|executable| execute_maintenance(executable, maintenance_intent(command)));
            let (outcome, message, applied_settings) = match report {
                Some(report) => {
                    let outcome = match report.disposition {
                        MaintenanceDisposition::Completed => OperationOutcome::Succeeded,
                        MaintenanceDisposition::Instruction
                        | MaintenanceDisposition::VerifiedArtifactRequired => {
                            OperationOutcome::NoChange
                        }
                        MaintenanceDisposition::Failed => OperationOutcome::Failed,
                    };
                    let applied_settings = if outcome == OperationOutcome::Succeeded {
                        if let MaintenanceCommand::ChangeChannel { channel } = command {
                            local_settings.release_channel = channel;
                            SettingsStore::discover()
                                .and_then(|store| store.save(&local_settings))
                                .ok()
                                .map(|_| local_settings)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    (outcome, report.message, applied_settings)
                }
                None => (
                    OperationOutcome::Failed,
                    "running executable is unavailable".to_string(),
                    None,
                ),
            };
            let _ = qt_thread.queue(move |mut controller| {
                if let Some(settings) = applied_settings {
                    controller.as_mut().install_local_settings(settings, None);
                }
                controller.apply_action(Action::OperationFinished {
                    id: operation_id,
                    outcome,
                    message,
                    finished_at_ms: now_ms(),
                });
            });
        });
    }

    fn update_tray(mut self: Pin<&mut Self>, provider_in_flight: BTreeMap<String, u32>) {
        let previous = self
            .as_mut()
            .rust_mut()
            .last_provider_in_flight
            .replace(provider_in_flight.clone());
        let Some(previous) = previous else {
            return;
        };
        let completed: Vec<String> = previous
            .into_iter()
            .filter(|(provider, before)| {
                *before > 0
                    && provider_in_flight
                        .get(provider)
                        .copied()
                        .unwrap_or_default()
                        == 0
            })
            .map(|(provider, _)| provider)
            .collect();
        let sound_id = self.rust().local_settings.sound_id.clone();
        for provider in completed {
            self.as_mut().emit_notification(
                &format!("{provider} is ready"),
                "The provider has no requests in flight",
                &sound_id,
            );
        }
    }

    fn execute_direct_request(mut self: Pin<&mut Self>, request: PlatformRequest) {
        match request {
            PlatformRequest::OpenUrl(url) => self.as_mut().emit_platform_command("open_url", &url),
            PlatformRequest::CopyText(text) => {
                self.as_mut().emit_platform_command("copy_text", &text);
            }
            PlatformRequest::TestNotification { sound_id } => {
                let sound_id =
                    sound_id.unwrap_or_else(|| self.rust().local_settings.sound_id.clone());
                self.as_mut().emit_notification(
                    "llmux Islands",
                    "Desktop notification adapter is available",
                    &sound_id,
                );
            }
            PlatformRequest::Quit => self.emit_platform_command("quit", ""),
        }
    }

    fn emit_platform_command(mut self: Pin<&mut Self>, command: &str, payload: &str) {
        self.as_mut()
            .platform_command(&QString::from(command), &QString::from(payload));
    }

    fn emit_notification(mut self: Pin<&mut Self>, title: &str, body: &str, sound_id: &str) {
        let payload = serde_json::json!({
            "title": title,
            "body": body,
        })
        .to_string();
        self.as_mut()
            .emit_platform_command("show_notification", &payload);
        spawn_notification_sound(title.to_string(), sound_id.to_string());
    }

    fn refresh_platform_state(mut self: Pin<&mut Self>) {
        let local_settings = self.rust().local_settings.clone();
        let autostart_enabled = *self.autostart_enabled();
        let install_owner = self.rust().install_owner.clone();
        self.as_mut().rust_mut().model.set_platform_state(
            local_settings,
            autostart_enabled,
            install_owner,
        );
        self.sync_state_json();
    }
}

fn execute_daemon_effect(client: Option<&DaemonClient>, effect: Effect) -> Option<Action> {
    let at_ms = now_ms();
    let Some(client) = client else {
        return effect_failure_action(effect, "daemon client is unavailable", at_ms);
    };
    let original = effect.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    match runtime {
        Ok(runtime) => match runtime.block_on(client.execute(effect, now_ms())) {
            EffectExecution::Action(action) => Some(action),
            EffectExecution::Platform(_) => {
                effect_failure_action(original, "unexpected platform effect", at_ms)
            }
        },
        Err(_) => effect_failure_action(original, "async executor is unavailable", at_ms),
    }
}

fn effect_failure_action(effect: Effect, message: &str, at_ms: u64) -> Option<Action> {
    match effect {
        Effect::FetchDashboard { request_id } => Some(Action::DashboardFailed {
            request_id,
            error: message.to_string(),
            failed_at_ms: at_ms,
        }),
        Effect::StartLogin { operation_id, .. } | Effect::PollLogin { operation_id, .. } => {
            Some(Action::LoginStatusReceived {
                operation_id,
                status: LoginStatus::Failed {
                    message: message.to_string(),
                },
                at_ms,
            })
        }
        Effect::CancelLogin { operation_id, .. } => Some(Action::LoginStatusReceived {
            operation_id,
            status: LoginStatus::CancellationFailed {
                message: message.to_string(),
            },
            at_ms,
        }),
        Effect::RunOperation { operation_id, .. }
        | Effect::UpdateSettings { operation_id, .. }
        | Effect::UpsertEvent { operation_id, .. }
        | Effect::RemoveEvent { operation_id, .. }
        | Effect::PersistSettings { operation_id, .. }
        | Effect::SetAutostart { operation_id, .. }
        | Effect::RunMaintenance { operation_id, .. } => Some(Action::OperationFinished {
            id: operation_id,
            outcome: OperationOutcome::Failed,
            message: message.to_string(),
            finished_at_ms: at_ms,
        }),
        Effect::EnsureLocalDaemon
        | Effect::ScheduleDashboardRetry { .. }
        | Effect::CancelDashboardRetry
        | Effect::StopLoginPoll { .. }
        | Effect::UpdateTray { .. } => None,
    }
}

fn queue_action(thread: &cxx_qt::CxxQtThread<qobject::IslandsController>, action: Action) {
    let _ = thread.queue(move |controller| controller.apply_action(action));
}

fn prepare_connection(
    settings: &LocalSettings,
    presentation: Presentation,
) -> Result<PreparedConnection, &'static str> {
    let api_key = settings
        .api_key
        .as_ref()
        .map(|api_key| api_key.expose_secret().to_string());
    let config = ControllerConfig::from_values(&settings.endpoint, api_key, presentation)
        .map_err(|_| "daemon connection settings are invalid")?;
    let options = config.derive_options();
    let client = match config.daemon_client() {
        Ok(client) => Some(Arc::new(client)),
        Err(error) if error.kind() == ClientErrorKind::MissingApiKey => None,
        Err(_) => return Err("daemon connection could not be applied"),
    };
    Ok(PreparedConnection {
        endpoint: config.endpoint().to_string(),
        client,
        options,
    })
}

fn apply_local_settings_change(settings: &mut LocalSettings, change: LocalSettingsChange) {
    match change {
        LocalSettingsChange::ScreenSelected { id } => settings.selected_screen_id = id,
        LocalSettingsChange::SoundSelected { id } => settings.sound_id = id,
        LocalSettingsChange::ShowFable { enabled } => settings.show_fable_weekly = enabled,
        LocalSettingsChange::ConnectionApplied { endpoint, api_key } => {
            settings.endpoint = endpoint;
            settings.api_key = api_key.map(|api_key| ApiKey::new(api_key.expose_secret()));
        }
    }
}

fn maintenance_intent(command: MaintenanceCommand) -> MaintenanceIntent {
    match command {
        MaintenanceCommand::Update => MaintenanceIntent::Update,
        MaintenanceCommand::ChangeChannel { channel } => {
            let channel = match channel {
                ReleaseChannel::Stable => "stable",
                ReleaseChannel::Preview => "preview",
            };
            MaintenanceIntent::ChangeChannel(channel.to_string())
        }
    }
}

fn install_owner_label(owner: &InstallOwner) -> String {
    match owner {
        InstallOwner::Pacman { package } => format!("pacman ({package})"),
        InstallOwner::Homebrew => "homebrew".to_string(),
        InstallOwner::SelfManaged => "self-managed".to_string(),
        InstallOwner::Unknown => "unknown".to_string(),
    }
}

fn spawn_notification_sound(summary: String, sound_id: String) {
    if matches!(sound_id.as_str(), "" | "silent" | "none") {
        return;
    }
    std::thread::spawn(move || {
        if let Some(player) = command_in_path("canberra-gtk-play") {
            let _ = Command::new(player)
                .args(["--id", sound_id.as_str(), "--description", summary.as_str()])
                .status();
        }
    });
}

fn command_in_path(command: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|directory| directory.join(command))
        .find(|candidate| candidate.is_file())
}

fn presentation(surface_mode: &str) -> Presentation {
    match surface_mode {
        "wayland-layer-shell" => Presentation::LayerShell,
        "x11-positioned" => Presentation::PositionedX11,
        _ => Presentation::Regular,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}
