use std::fmt;
use std::net::IpAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use llmux::dashboard::DashboardDoc;
use reqwest::{Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::contract::{
    Action, Effect, EventDraft, LoginStatus, OperationOutcome, OperationRequest, Provider,
    SecretString,
};
use crate::privacy::{sanitize_endpoint, sanitize_text};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientErrorKind {
    InvalidEndpoint,
    InsecureEndpoint,
    MissingApiKey,
    InvalidRequest,
    Timeout,
    Transport,
    HttpStatus,
    Decode,
    UnsupportedEffect,
}

pub struct ClientError {
    kind: ClientErrorKind,
    status: Option<u16>,
    message: String,
}

impl ClientError {
    fn new(kind: ClientErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            message: sanitize_text(&message.into()),
        }
    }

    fn http(status: StatusCode) -> Self {
        Self {
            kind: ClientErrorKind::HttpStatus,
            status: Some(status.as_u16()),
            // The response body is daemon-controlled and can echo account
            // names or arbitrary credentials. Keep only the typed status
            // diagnostic in production errors.
            message: format!("daemon returned HTTP {}", status.as_u16()),
        }
    }

    fn from_reqwest(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::new(ClientErrorKind::Timeout, "daemon request timed out")
        } else {
            Self::new(ClientErrorKind::Transport, "daemon request failed")
        }
    }

    pub const fn kind(&self) -> ClientErrorKind {
        self.kind
    }

    pub const fn status(&self) -> Option<u16> {
        self.status
    }
}

impl fmt::Debug for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientError")
            .field("kind", &self.kind)
            .field("status", &self.status)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClientError {}

pub struct ClientConfig {
    endpoint: Url,
    remote: bool,
    api_key: Option<SecretString>,
    timeout: Duration,
}

impl ClientConfig {
    pub fn new(endpoint: impl AsRef<str>) -> Result<Self, ClientError> {
        Self::parse(endpoint.as_ref(), false)
    }

    /// Explicit development/test escape hatch for a remote daemon that cannot
    /// offer TLS. Production callers should use [`Self::new`].
    pub fn new_insecure_remote_http(endpoint: impl AsRef<str>) -> Result<Self, ClientError> {
        Self::parse(endpoint.as_ref(), true)
    }

    fn parse(raw: &str, allow_insecure_remote_http: bool) -> Result<Self, ClientError> {
        let authority = raw
            .strip_prefix("http://")
            .or_else(|| raw.strip_prefix("https://"));
        if authority.is_none_or(|value| value.is_empty() || value.starts_with('/')) {
            return Err(ClientError::new(
                ClientErrorKind::InvalidEndpoint,
                "invalid daemon endpoint",
            ));
        }
        let endpoint = Url::parse(raw).map_err(|_| {
            ClientError::new(ClientErrorKind::InvalidEndpoint, "invalid daemon endpoint")
        })?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || endpoint.port_or_known_default().is_none()
            || endpoint.port() == Some(0)
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/"
        {
            return Err(ClientError::new(
                ClientErrorKind::InvalidEndpoint,
                "invalid daemon endpoint",
            ));
        }
        let remote = !is_loopback_host(endpoint.host_str().unwrap_or_default());
        if remote && endpoint.scheme() != "https" && !allow_insecure_remote_http {
            return Err(ClientError::new(
                ClientErrorKind::InsecureEndpoint,
                "remote daemon endpoint requires HTTPS",
            ));
        }
        Ok(Self {
            endpoint,
            remote,
            api_key: None,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    pub fn with_api_key(mut self, api_key: SecretString) -> Self {
        self.api_key = Some(api_key);
        self
    }

    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub const fn is_remote(&self) -> bool {
        self.remote
    }
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("endpoint", &sanitize_endpoint(self.endpoint.as_str()))
            .field("remote", &self.remote)
            .field("api_key_configured", &self.api_key.is_some())
            .field("timeout", &self.timeout)
            .finish()
    }
}

fn is_loopback_host(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

pub struct DaemonClient {
    endpoint: Url,
    remote: bool,
    api_key: Option<SecretString>,
    timeout: Duration,
    http: reqwest::Client,
}

impl DaemonClient {
    pub fn new(config: ClientConfig) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                ClientError::new(ClientErrorKind::Transport, "could not create HTTP client")
            })?;
        Self::with_http_client(config, http)
    }

    pub fn with_http_client(
        config: ClientConfig,
        http: reqwest::Client,
    ) -> Result<Self, ClientError> {
        if config.timeout.is_zero() {
            return Err(ClientError::new(
                ClientErrorKind::InvalidRequest,
                "request timeout must be greater than zero",
            ));
        }
        if config.remote && config.api_key.as_ref().is_none_or(|key| key.is_empty()) {
            return Err(ClientError::new(
                ClientErrorKind::MissingApiKey,
                "remote daemon requires an x-api-key",
            ));
        }
        Ok(Self {
            endpoint: config.endpoint,
            remote: config.remote,
            api_key: config.api_key,
            timeout: config.timeout,
            http,
        })
    }

    pub async fn fetch_dashboard(&self) -> Result<DashboardDoc, ClientError> {
        self.get_json("/llmux/dashboard", &[]).await
    }

    pub async fn run_operation(
        &self,
        request: &OperationRequest,
    ) -> Result<OperationAck, ClientError> {
        if let Some(message) = request.validation_error() {
            return Err(ClientError::new(ClientErrorKind::InvalidRequest, message));
        }

        let mut outcome = OperationOutcome::Succeeded;
        let (response, message) = match request {
            OperationRequest::AddAccount { name, api_key } => {
                let body = AddAccountBody {
                    name: name.as_deref(),
                    api_key: api_key.expose_secret(),
                };
                let response: Value = self.post_json("/llmux/add-account", &body).await?;
                require_true(&response, "ok")?;
                require_string(&response, "name")?;
                let added = require_bool(&response, "added")?;
                if added {
                    (response, "account added")
                } else {
                    outcome = OperationOutcome::NoChange;
                    (response, "account already exists")
                }
            }
            OperationRequest::RemoveAccount {
                account_id,
                confirmed,
            } => {
                let body = json!({ "name": account_id, "confirm": confirmed });
                let response: Value = self.post_json("/llmux/remove-account", &body).await?;
                require_true(&response, "ok")?;
                require_true(&response, "removed")?;
                (response, "account removed")
            }
            OperationRequest::PauseAccount { account_id, paused } => {
                let body = json!({ "account": account_id, "paused": paused });
                let response: Value = self.post_json("/llmux/pause-account", &body).await?;
                require_true(&response, "ok")?;
                if response.get("paused").and_then(Value::as_bool) != Some(*paused) {
                    return Err(invalid_response());
                }
                (
                    response,
                    if *paused {
                        "account paused"
                    } else {
                        "account resumed"
                    },
                )
            }
            OperationRequest::UpdateSettings { email_anonymous } => {
                let body = json!({ "email_anonymous": email_anonymous });
                let response: Value = self.post_json("/llmux/settings", &body).await?;
                require_true(&response, "ok")?;
                if response.get("email_anonymous").and_then(Value::as_bool)
                    != Some(*email_anonymous)
                {
                    return Err(invalid_response());
                }
                (response, "settings updated")
            }
            OperationRequest::UpsertEvent { event } => {
                let body = EventBody::from(event);
                let response: Value = self.post_json("/llmux/events", &body).await?;
                require_true(&response, "ok")?;
                require_array(&response, "events")?;
                (response, "event updated")
            }
            OperationRequest::RemoveEvent { event_id } => {
                let body = json!({ "remove": event_id });
                let response: Value = self.post_json("/llmux/events", &body).await?;
                require_true(&response, "ok")?;
                require_array(&response, "events")?;
                (response, "event removed")
            }
            OperationRequest::PersistLocalSettings { .. }
            | OperationRequest::SetAutostart { .. }
            | OperationRequest::RunMaintenance { .. } => {
                return Err(ClientError::new(
                    ClientErrorKind::UnsupportedEffect,
                    "operation belongs to the platform executor",
                ));
            }
        };
        drop(response);
        Ok(OperationAck {
            outcome,
            message: message.to_string(),
        })
    }

    pub async fn start_login(&self, provider: Provider) -> Result<LoginStart, ClientError> {
        if matches!(provider, Provider::Api | Provider::Unknown) {
            return Err(ClientError::new(
                ClientErrorKind::InvalidRequest,
                "unsupported login provider",
            ));
        }
        let body = json!({ "provider": provider.key() });
        let response: LoginStartWire = self.post_json("/llmux/login/start", &body).await?;
        if !response.ok || response.state.trim().is_empty() || response.provider != provider.key() {
            return Err(invalid_response());
        }
        Ok(LoginStart {
            state: response.state,
            provider,
        })
    }

    pub async fn login_status(&self, state: &str) -> Result<LoginStatus, ClientError> {
        if state.trim().is_empty() {
            return Err(ClientError::new(
                ClientErrorKind::InvalidRequest,
                "login state is required",
            ));
        }
        let response: LoginStatusWire = self
            .get_json("/llmux/login/status", &[("state", state)])
            .await?;
        match response.phase.as_str() {
            "pending" => Ok(LoginStatus::Pending {
                state: state.to_string(),
                verification_uri: response
                    .verification_uri
                    .as_deref()
                    .map(validate_verification_uri)
                    .transpose()?,
                user_code: response.user_code.as_deref().map(sanitize_text),
                message: None,
            }),
            "done" => Ok(LoginStatus::Succeeded {
                target_display: response.account,
                message: "login succeeded".to_string(),
            }),
            // A 2xx login-status envelope can still carry a daemon error
            // body. Do not promote that untrusted free text into UiState or a
            // persistent verification receipt.
            "error" => Ok(LoginStatus::Failed {
                message: "login failed".to_string(),
            }),
            _ => Err(invalid_response()),
        }
    }

    pub async fn cancel_login(&self, state: &str) -> Result<bool, ClientError> {
        if state.trim().is_empty() {
            return Err(ClientError::new(
                ClientErrorKind::InvalidRequest,
                "login state is required",
            ));
        }
        let body = json!({ "state": state });
        let response: LoginCancelWire = self.post_json("/llmux/login/cancel", &body).await?;
        if !response.ok {
            return Err(invalid_response());
        }
        Ok(response.cancelled)
    }

    pub async fn execute(&self, effect: Effect, at_ms: u64) -> EffectExecution {
        match effect {
            Effect::FetchDashboard { request_id } => {
                let result = self.fetch_dashboard().await;
                let completed_at_ms = completion_timestamp_ms(at_ms);
                let action = match result {
                    Ok(document) => Action::DashboardReceived {
                        request_id,
                        document: Box::new(document),
                        received_at_ms: completed_at_ms,
                    },
                    Err(error) => Action::DashboardFailed {
                        request_id,
                        error: error.to_string(),
                        failed_at_ms: completed_at_ms,
                    },
                };
                EffectExecution::Action(action)
            }
            Effect::StartLogin {
                operation_id,
                provider,
            } => {
                let result = self.start_login(provider).await;
                let completed_at_ms = completion_timestamp_ms(at_ms);
                let status = match result {
                    Ok(start) => LoginStatus::Pending {
                        state: start.state,
                        verification_uri: None,
                        user_code: None,
                        message: Some("waiting for login".to_string()),
                    },
                    Err(error) => LoginStatus::Failed {
                        message: error.to_string(),
                    },
                };
                EffectExecution::Action(Action::LoginStatusReceived {
                    operation_id,
                    status,
                    at_ms: completed_at_ms,
                })
            }
            Effect::PollLogin {
                operation_id,
                state,
            } => {
                let result = self.login_status(&state).await;
                let completed_at_ms = completion_timestamp_ms(at_ms);
                let status = match result {
                    Ok(status) => status,
                    Err(error) if is_transient_login_error(&error) => LoginStatus::Pending {
                        state: state.clone(),
                        verification_uri: None,
                        user_code: None,
                        message: Some("login status unavailable; retrying".to_string()),
                    },
                    Err(error) => LoginStatus::Failed {
                        message: error.to_string(),
                    },
                };
                EffectExecution::Action(Action::LoginStatusReceived {
                    operation_id,
                    status,
                    at_ms: completed_at_ms,
                })
            }
            Effect::CancelLogin {
                operation_id,
                state,
            } => {
                let result = self.cancel_login(&state).await;
                let completed_at_ms = completion_timestamp_ms(at_ms);
                let status = match result {
                    Ok(true) => LoginStatus::CancellationAcknowledged {
                        message: "login cancelled".to_string(),
                    },
                    Ok(false) => LoginStatus::CancellationFailed {
                        message: "login cancellation was not applied".to_string(),
                    },
                    Err(error) => LoginStatus::CancellationFailed {
                        message: error.to_string(),
                    },
                };
                EffectExecution::Action(Action::LoginStatusReceived {
                    operation_id,
                    status,
                    at_ms: completed_at_ms,
                })
            }
            Effect::RunOperation {
                operation_id,
                request,
            } => self.execute_operation(operation_id, request, at_ms).await,
            Effect::UpdateSettings {
                operation_id,
                email_anonymous,
            } => {
                self.execute_operation(
                    operation_id,
                    OperationRequest::UpdateSettings { email_anonymous },
                    at_ms,
                )
                .await
            }
            Effect::UpsertEvent {
                operation_id,
                event,
            } => {
                self.execute_operation(operation_id, OperationRequest::UpsertEvent { event }, at_ms)
                    .await
            }
            Effect::RemoveEvent {
                operation_id,
                event_id,
            } => {
                self.execute_operation(
                    operation_id,
                    OperationRequest::RemoveEvent { event_id },
                    at_ms,
                )
                .await
            }
            platform @ (Effect::EnsureLocalDaemon
            | Effect::ScheduleDashboardRetry { .. }
            | Effect::CancelDashboardRetry
            | Effect::StopLoginPoll { .. }
            | Effect::PersistSettings { .. }
            | Effect::SetAutostart { .. }
            | Effect::RunMaintenance { .. }
            | Effect::UpdateTray { .. }) => EffectExecution::Platform(platform),
        }
    }

    async fn execute_operation(
        &self,
        operation_id: String,
        request: OperationRequest,
        at_ms: u64,
    ) -> EffectExecution {
        let (outcome, message) = match self.run_operation(&request).await {
            Ok(ack) => (ack.outcome, ack.message),
            Err(error) => (OperationOutcome::Failed, error.to_string()),
        };
        let finished_at_ms = completion_timestamp_ms(at_ms);
        EffectExecution::Action(Action::OperationFinished {
            id: operation_id,
            outcome,
            message,
            finished_at_ms,
        })
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, ClientError> {
        let mut url = self.endpoint.join(path).map_err(|_| {
            ClientError::new(ClientErrorKind::InvalidEndpoint, "invalid daemon endpoint")
        })?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let request = self.request_url(Method::GET, url)?;
        self.send_json(request).await
    }

    async fn post_json<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ClientError> {
        let request = self.request(Method::POST, path)?.json(body);
        self.send_json(request).await
    }

    fn request(&self, method: Method, path: &str) -> Result<reqwest::RequestBuilder, ClientError> {
        let url = self.endpoint.join(path).map_err(|_| {
            ClientError::new(ClientErrorKind::InvalidEndpoint, "invalid daemon endpoint")
        })?;
        self.request_url(method, url)
    }

    fn request_url(
        &self,
        method: Method,
        url: Url,
    ) -> Result<reqwest::RequestBuilder, ClientError> {
        let mut request = self.http.request(method, url).timeout(self.timeout);
        if self.remote {
            let api_key = self.api_key.as_ref().ok_or_else(|| {
                ClientError::new(
                    ClientErrorKind::MissingApiKey,
                    "remote daemon requires an x-api-key",
                )
            })?;
            request = request.header("x-api-key", api_key.expose_secret());
        }
        Ok(request)
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ClientError> {
        let response = request
            .send()
            .await
            .map_err(|error| ClientError::from_reqwest(&error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ClientError::http(status));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| ClientError::from_reqwest(&error))?;
        serde_json::from_slice(&body).map_err(|_| invalid_response())
    }
}

impl fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DaemonClient")
            .field("endpoint", &sanitize_endpoint(self.endpoint.as_str()))
            .field("remote", &self.remote)
            .field("api_key_configured", &self.api_key.is_some())
            .field("timeout", &self.timeout)
            .finish()
    }
}

pub enum EffectExecution {
    Action(Action),
    Platform(Effect),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationAck {
    pub outcome: OperationOutcome,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginStart {
    pub state: String,
    pub provider: Provider,
}

#[derive(Serialize)]
struct AddAccountBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    api_key: &'a str,
}

#[derive(Serialize)]
struct EventBody<'a> {
    id: &'a str,
    from: &'a str,
    to: &'a str,
    content: &'a str,
}

impl<'a> From<&'a EventDraft> for EventBody<'a> {
    fn from(event: &'a EventDraft) -> Self {
        Self {
            id: &event.id,
            from: &event.from,
            to: &event.to,
            content: &event.content,
        }
    }
}

#[derive(Deserialize)]
struct LoginStartWire {
    ok: bool,
    state: String,
    provider: String,
}

#[derive(Deserialize)]
struct LoginStatusWire {
    phase: String,
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    user_code: Option<String>,
}

#[derive(Deserialize)]
struct LoginCancelWire {
    ok: bool,
    cancelled: bool,
}

fn require_true(value: &Value, field: &str) -> Result<(), ClientError> {
    if value.get(field).and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err(invalid_response())
    }
}

fn require_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, ClientError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(invalid_response)
}

fn require_bool(value: &Value, field: &str) -> Result<bool, ClientError> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(invalid_response)
}

fn require_array<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], ClientError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(invalid_response)
}

fn invalid_response() -> ClientError {
    ClientError::new(
        ClientErrorKind::Decode,
        "daemon returned an invalid response",
    )
}

fn is_transient_login_error(error: &ClientError) -> bool {
    matches!(
        error.kind(),
        ClientErrorKind::Timeout | ClientErrorKind::Transport
    ) || (error.kind() == ClientErrorKind::HttpStatus
        && error
            .status()
            .is_some_and(|status| status == 408 || status == 425 || status == 429 || status >= 500))
}

fn validate_verification_uri(uri: &str) -> Result<String, ClientError> {
    let mut parsed = Url::parse(uri).map_err(|_| invalid_response())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(invalid_response());
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    Ok(parsed.to_string())
}

fn completion_timestamp_ms(floor: u64) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(floor, |duration| {
            u64::try_from(duration.as_millis())
                .unwrap_or(u64::MAX)
                .max(floor)
        })
}
