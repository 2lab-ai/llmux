//! Google Antigravity (Gemini) provider (docs/gemini/spec.md §R3/§R4): serves
//! Anthropic Messages API requests from a Google Antigravity subscription
//! account through Cloud Code's `v1internal:streamGenerateContent` endpoint.
//! This is NOT the Responses wire family grok/codex share, so the module owns
//! both legs: Messages → Antigravity envelope on the way up
//! ([`GeminiProvider::build_request`]) and Gemini SSE → Anthropic SSE on the
//! way down ([`GeminiSseConverter`]).
//!
//! Gemini signs every `functionCall` part with an opaque `thoughtSignature`
//! that the NEXT turn must echo back (omitting it is a measured 400). The
//! converter hands the client a freshly minted tool id for every call and
//! records the call's signature and upstream id in a [`SignatureCache`]
//! shared with the provider, keyed by `(sessionId, client tool id)`;
//! `build_request` restores both when the client replays the call.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderValue, Method};
use serde_json::{json, Map, Value};
use sha2::{Digest as _, Sha256};

use super::responses::{self, unavailable_safeguard_results};
use super::responses_request::CompatibilityReport;
use super::{ProviderError, ProviderRequest};
use crate::config::{AccountCredential, GeminiConfig};
use crate::proxy::sse::{SseTransform, StreamUsage};

/// Request path appended to the Cloud Code base URL (`config.gemini.upstream`).
pub const GEMINI_STREAM_PATH: &str = "/v1internal:streamGenerateContent?alt=sse";

/// Placeholder signature Cloud Code accepts on a replayed `functionCall` part
/// whose real signature llmux no longer holds (daemon restart, evicted entry).
/// Live 2026-10-15: real signature → 200, this value → 200, none → 400.
pub const SKIP_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

/// Entries the in-memory signature cache keeps before evicting the oldest.
pub const SIGNATURE_CACHE_CAPACITY: usize = 4096;

/// The bare flash family id: the variant is picked from the client's effort.
const FLASH_FAMILY: &str = "gemini-3.8-flash";
/// What the `gemini` alias resolves to (the `defaultAgentModelId` upstream).
const FLASH_HIGH: &str = "gemini-3.8-flash-high";
/// What the `argon` alias resolves to.
const ARGON: &str = "gemini-4-argon";

/// The client-side context-window suffix (`gemini-4-argon[1m]`), the same
/// display metadata grok and codex strip; upstream never sees it.
const CLIENT_CONTEXT_SUFFIX: &str = "[1m]";

/// `sessionId` is a non-negative i64 rendered with a leading `-` (reference
/// parity: CLIProxyAPI `generateStableSessionID`).
const SESSION_ID_MASK: u64 = 0x7fff_ffff_ffff_ffff;

/// Base64 image media types Gemini takes as `inlineData` (spec §R3).
const IMAGE_MEDIA_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/heic",
    "image/heif",
];
const PDF_MEDIA_TYPE: &str = "application/pdf";

/// JSON Schema keywords removed from a tool's `input_schema` — at the root
/// and every subschema position, never from property names or literal data —
/// before it becomes `parametersJsonSchema`.
const STRIPPED_SCHEMA_KEYS: &[&str] = &["$schema", "$id", "$comment"];

/// Keywords whose value is one subschema (a boolean schema included; `items`
/// may also be an array of them in pre-2020-12 drafts).
const SUBSCHEMA_KEYS: &[&str] = &[
    "items",
    "additionalItems",
    "additionalProperties",
    "unevaluatedItems",
    "unevaluatedProperties",
    "not",
    "if",
    "then",
    "else",
    "contains",
    "propertyNames",
    "contentSchema",
];

/// Keywords whose value is an array of subschemas.
const SUBSCHEMA_ARRAY_KEYS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];

/// Keywords whose value maps names — kept verbatim, `$id` included — to
/// subschemas (`dependencies` values may also be arrays of property names).
const SUBSCHEMA_MAP_KEYS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
    "dependencies",
];

/// Request-shaping knobs for the Antigravity request, sourced from
/// `config.gemini`. No effort knob: the thinking level is baked into the
/// model id (`gemini-3.8-flash-{high,medium,low}`).
#[derive(Debug, Clone)]
pub struct GeminiShape {
    /// Model id requested upstream when the client's model is not
    /// gemini-shaped (empty-group fallback).
    pub model: String,
    /// Model NAME reported to the client when the request named none (same
    /// contract as `codex.client_model`, but the client's own model wins).
    pub client_model: Option<String>,
    /// `User-Agent` sent upstream (`antigravity/hub/<version> <os>/<arch>`).
    pub user_agent: String,
}

impl Default for GeminiShape {
    fn default() -> Self {
        Self::from_config(&GeminiConfig::default())
    }
}

impl GeminiShape {
    /// Build from the on-disk gemini config.
    pub fn from_config(gemini: &GeminiConfig) -> Self {
        Self {
            model: gemini.default_model.clone(),
            client_model: gemini.client_model.clone(),
            user_agent: gemini.user_agent.clone(),
        }
    }
}

/// Bounded `(sessionId, client tool id) → `[`CachedCall`] map. The key is
/// the id the converter minted for the client, never Google's: live upstream
/// call ids are small counters (`call_90071`) that repeat, and conversations
/// opening with the same user text share a session id, so an upstream-id key
/// would let one conversation's signature overwrite another's. Eviction is
/// FIFO by last insert: re-recording a key moves it to the back. In-memory
/// only — after a daemon restart the [`SKIP_THOUGHT_SIGNATURE`] dummy keeps
/// multi-turn tool use working.
pub struct SignatureCache {
    capacity: usize,
    entries: Mutex<SignatureEntries>,
}

/// What one streamed `functionCall` left to replay under its client id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedCall {
    /// The call's `thoughtSignature` (Gemini signs only the first call of a
    /// parallel step).
    pub signature: Option<String>,
    /// Google's own `functionCall.id`, sent back in place of the client id.
    pub upstream_id: Option<String>,
}

#[derive(Default)]
struct SignatureEntries {
    map: HashMap<(String, String), CachedCall>,
    order: VecDeque<(String, String)>,
}

/// Size only: thousands of ~600-char signatures would swamp any log line
/// that debug-prints a provider or converter.
impl std::fmt::Debug for SignatureCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignatureCache")
            .field("capacity", &self.capacity)
            .field("len", &self.len())
            .finish()
    }
}

impl Default for SignatureCache {
    fn default() -> Self {
        Self::with_capacity(SIGNATURE_CACHE_CAPACITY)
    }
}

impl SignatureCache {
    /// Cache holding at most `capacity` (minimum 1) calls.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: Mutex::new(SignatureEntries::default()),
        }
    }

    /// Record `call` for `client_id` in `session_id`. Empty strings count as
    /// absent; an empty client id, or a call with neither a signature nor an
    /// upstream id, is ignored (nothing to replay).
    pub fn insert(&self, session_id: &str, client_id: &str, call: CachedCall) {
        let call = CachedCall {
            signature: call.signature.filter(|signature| !signature.is_empty()),
            upstream_id: call.upstream_id.filter(|id| !id.is_empty()),
        };
        if client_id.is_empty() || (call.signature.is_none() && call.upstream_id.is_none()) {
            return;
        }
        let key = (session_id.to_string(), client_id.to_string());
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.map.insert(key.clone(), call).is_some() {
            entries.order.retain(|existing| *existing != key);
        }
        entries.order.push_back(key);
        while entries.order.len() > self.capacity {
            let Some(oldest) = entries.order.pop_front() else {
                break;
            };
            entries.map.remove(&oldest);
        }
    }

    /// The call recorded for `client_id` in `session_id`, if still held.
    pub fn get(&self, session_id: &str, client_id: &str) -> Option<CachedCall> {
        let found = {
            let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            entries
                .map
                .get(&(session_id.to_string(), client_id.to_string()))
                .cloned()
        };
        #[cfg(test)]
        self.after_read();
        found
    }

    /// The calls recorded for `client_ids` in `session_id`, read under ONE
    /// lock: a consistent snapshot no concurrent insert (or the eviction it
    /// causes) can split. Ids the cache does not hold are simply absent.
    pub fn get_many<'a>(
        &self,
        session_id: &str,
        client_ids: impl IntoIterator<Item = &'a str>,
    ) -> HashMap<String, CachedCall> {
        let found = {
            let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            let mut found = HashMap::new();
            for client_id in client_ids {
                if found.contains_key(client_id) {
                    continue;
                }
                if let Some(call) = entries
                    .map
                    .get(&(session_id.to_string(), client_id.to_string()))
                {
                    found.insert(client_id.to_string(), call.clone());
                }
            }
            found
        };
        #[cfg(test)]
        self.after_read();
        found
    }

    /// Test-only: run the hook armed by [`arm_after_next_read`] on this
    /// thread, once, after a read released the lock.
    #[cfg(test)]
    fn after_read(&self) {
        if let Some(hook) = AFTER_NEXT_READ.with(|slot| slot.borrow_mut().take()) {
            hook(self);
        }
    }

    /// Number of calls currently held.
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .map
            .len()
    }

    /// Whether the cache holds no call.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Test-only: a hook run against the cache after one read.
#[cfg(test)]
type ReadHook = Box<dyn FnOnce(&SignatureCache)>;

#[cfg(test)]
thread_local! {
    /// Test-only: what the next cache read on this thread runs once it has
    /// released the lock — another request's insert landing mid-replay.
    static AFTER_NEXT_READ: std::cell::RefCell<Option<ReadHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only: arm `hook` for the next [`SignatureCache`] read on this thread.
#[cfg(test)]
fn arm_after_next_read(hook: impl FnOnce(&SignatureCache) + 'static) {
    AFTER_NEXT_READ.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

/// The gemini provider: upstream base URL, live-mutable request shape, and
/// the thought-signature cache its converters fill and its requests read.
#[derive(Debug)]
pub struct GeminiProvider {
    base_url: String,
    shape: RwLock<GeminiShape>,
    signatures: Arc<SignatureCache>,
}

impl GeminiProvider {
    /// Construct with the default request shape (`GeminiConfig::default()`).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_shape(base_url, GeminiShape::default())
    }

    /// Construct with an explicit request shape (from `config.gemini`).
    pub fn with_shape(base_url: impl Into<String>, shape: GeminiShape) -> Self {
        Self {
            base_url: base_url.into(),
            shape: RwLock::new(shape),
            signatures: Arc::new(SignatureCache::default()),
        }
    }

    /// Snapshot the current request shape. A poisoned lock still holds a
    /// complete shape (writers only swap whole values), so it is read through.
    pub fn shape(&self) -> GeminiShape {
        self.shape
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Replace the live request shape.
    pub fn set_shape(&self, shape: GeminiShape) {
        *self.shape.write().unwrap_or_else(PoisonError::into_inner) = shape;
    }

    /// The configured fallback model (for the activity log).
    pub fn model(&self) -> String {
        self.shape
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .model
            .clone()
    }

    pub fn endpoint(&self) -> &str {
        &self.base_url
    }

    /// The PER-REQUEST `(upstream model, effort)` for `anthropic_body` — the
    /// model [`Self::build_request`] would send. Effort is always `None`: the
    /// level is part of the model id and no effort field goes upstream.
    pub fn request_meta(&self, anthropic_body: &[u8]) -> (String, Option<String>) {
        let shape = self.shape();
        let body = serde_json::from_slice::<Value>(anthropic_body).unwrap_or(Value::Null);
        (
            resolve_model(
                requested_model(&body),
                requested_effort(&body),
                &shape.model,
            ),
            None,
        )
    }

    /// The model NAME to stamp into the client-facing response for this
    /// request (pass it to [`Self::converter`]): an alias (`gemini`, `argon`,
    /// bare `gemini-3.8-flash`) becomes its `[1m]` catalog row, any other id
    /// is the client's own spelling (so Claude Code keeps its `[1m]` window),
    /// and a model-less request gets `shape.client_model`, else the upstream id.
    pub fn client_model_stamp(&self, anthropic_body: &Value) -> String {
        client_facing_model(
            requested_model(anthropic_body).unwrap_or(""),
            requested_effort(anthropic_body),
            &self.shape(),
        )
    }

    /// Build the upstream Antigravity request from an Anthropic Messages
    /// body. Returns the request plus whether the CLIENT asked for streaming
    /// (upstream always streams). The credential must already carry a
    /// project: an empty `project_id` is resolved by the caller
    /// (`loadCodeAssist`) before this is called.
    pub fn build_request(
        &self,
        anthropic_body: &[u8],
        credential: &AccountCredential,
    ) -> Result<(ProviderRequest, bool), ProviderError> {
        let AccountCredential::Gemini {
            access_token,
            project_id,
            ..
        } = credential
        else {
            return Err(ProviderError::Auth(
                "gemini provider requires a gemini credential".into(),
            ));
        };
        if project_id.trim().is_empty() {
            return Err(ProviderError::Auth(
                "gemini credential has no project; resolve via loadCodeAssist first".into(),
            ));
        }
        // Client-side fault → `InvalidRequest` (HTTP 400 locally).
        let body: Value = serde_json::from_slice(anthropic_body).map_err(|err| {
            ProviderError::InvalidRequest(format!("request body is not JSON: {err}"))
        })?;
        let shape = self.shape();
        let session_id = session_id_for(&body);
        let (upstream_body, client_stream, _) = translate_request(
            &body,
            &shape,
            project_id.trim(),
            &session_id,
            &self.signatures,
        )?;

        let mut headers = HeaderMap::new();
        let bearer = HeaderValue::from_str(&format!("Bearer {access_token}"))
            .map_err(|err| ProviderError::Auth(err.to_string()))?;
        headers.insert(AUTHORIZATION, bearer);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        let user_agent = HeaderValue::from_str(&shape.user_agent).map_err(|_| {
            ProviderError::Convert("gemini user_agent is not a valid header value".into())
        })?;
        headers.insert(USER_AGENT, user_agent);

        Ok((
            ProviderRequest {
                method: Method::POST,
                path: GEMINI_STREAM_PATH.to_string(),
                headers,
                body: Bytes::from(upstream_body.to_string()),
            },
            client_stream,
        ))
    }

    /// What [`Self::build_request`] would leave out of `anthropic_body`,
    /// as the compatibility report codex/grok produce (header names
    /// `max_tokens`, `thinking`, `redacted_thinking`, `thinking_config`,
    /// `server_tools`, `disable_parallel_tool_use`). Runs the SAME
    /// translation, so a body that validates here cannot fail there and
    /// vice versa — `Err` is the same [`ProviderError::InvalidRequest`]
    /// naming the JSON path. No project or credential is needed.
    pub fn validate_request(
        &self,
        anthropic_body: &Value,
    ) -> Result<CompatibilityReport, ProviderError> {
        translate_request(anthropic_body, &self.shape(), "", "", &self.signatures)
            .map(|(_, _, report)| report)
    }

    /// Fresh per-request stream converter. `client_model_stamp` is the model
    /// the client requested (raw, or already [`Self::client_model_stamp`]-ed —
    /// the mapping is idempotent); `session_id` is [`session_id_for`] of the
    /// same body, so streamed signatures land under the key the next turn's
    /// `build_request` reads.
    pub fn converter(&self, client_model_stamp: String, session_id: String) -> GeminiSseConverter {
        let shape = self.shape();
        let upstream = resolve_model(Some(&client_model_stamp), None, &shape.model);
        let stamp = client_facing_model(&client_model_stamp, None, &shape);
        GeminiSseConverter::with_model(upstream)
            .with_client_model(Some(stamp))
            .with_signatures(Arc::clone(&self.signatures), session_id)
    }
}

/// The `sessionId` sent upstream for `anthropic_body`: `-` + the low 63 bits
/// of the big-endian first 8 bytes of sha256(first user text), stable across
/// a conversation's turns (reference parity). "First user text" is every
/// text block of the first user message that has any, joined by `\n`. With
/// no user text it is random per call — two calls then differ, which only
/// costs a signature-cache miss (the dummy signature covers it).
pub fn session_id_for(anthropic_body: &Value) -> String {
    let seed = first_user_text(anthropic_body).unwrap_or_else(responses::uuid_v4);
    format!("-{}", session_hash(&seed))
}

fn session_hash(text: &str) -> u64 {
    let digest = Sha256::digest(text.as_bytes());
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(head) & SESSION_ID_MASK
}

fn first_user_text(body: &Value) -> Option<String> {
    body.get("messages")?
        .as_array()?
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .find_map(message_text)
}

fn message_text(message: &Value) -> Option<String> {
    let text = match message.get("content")? {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

fn requested_model(body: &Value) -> Option<&str> {
    body.get("model").and_then(Value::as_str)
}

fn requested_effort(body: &Value) -> Option<&str> {
    body.get("output_config")
        .and_then(|config| config.get("effort"))
        .and_then(Value::as_str)
}

/// One trailing [`CLIENT_CONTEXT_SUFFIX`] off `model` (case-insensitively,
/// after trimming), or `model` unchanged.
fn strip_client_context_suffix(model: &str) -> &str {
    let model = model.trim();
    match model.len().checked_sub(CLIENT_CONTEXT_SUFFIX.len()) {
        Some(cut)
            if model.is_char_boundary(cut)
                && model[cut..].eq_ignore_ascii_case(CLIENT_CONTEXT_SUFFIX) =>
        {
            &model[..cut]
        }
        _ => model,
    }
}

/// The flash variant for a client effort: `none|minimal|low` → `-low`,
/// `medium` → `-medium`, anything else (incl. none sent) → `-high`.
fn flash_variant(effort: Option<&str>) -> String {
    let level = match effort.map(|e| e.trim().to_ascii_lowercase()).as_deref() {
        Some("none" | "minimal" | "low") => "low",
        Some("medium") => "medium",
        _ => "high",
    };
    format!("{FLASH_FAMILY}-{level}")
}

/// Whether `base` (suffix-stripped, lowercase) is an alias rather than an id.
fn is_alias(base: &str) -> bool {
    matches!(base, "gemini" | "argon" | FLASH_FAMILY)
}

/// The upstream id for a gemini-shaped name (alias, bare family, or any
/// `gemini-*` id), or `None` for anything else.
fn gemini_upstream_id(name: &str, effort: Option<&str>) -> Option<String> {
    let base = strip_client_context_suffix(name).to_ascii_lowercase();
    match base.as_str() {
        "gemini" => Some(FLASH_HIGH.to_string()),
        "argon" => Some(ARGON.to_string()),
        FLASH_FAMILY => Some(flash_variant(effort)),
        id if id.starts_with("gemini-") => Some(id.to_string()),
        _ => None,
    }
}

/// Model resolution (spec §R3): trim, strip one `[1m]`, aliases `gemini` /
/// `argon`, bare `gemini-3.8-flash` + effort → variant, other `gemini-*` ids
/// verbatim; non-gemini names and model-less requests use the pin (which
/// gets the same treatment, so a config may carry the picker's spelling).
fn resolve_model(requested: Option<&str>, effort: Option<&str>, pinned: &str) -> String {
    requested
        .and_then(|name| gemini_upstream_id(name, effort))
        .or_else(|| gemini_upstream_id(pinned, effort))
        .unwrap_or_else(|| strip_client_context_suffix(pinned).to_string())
}

/// The client-facing model stamp (see [`GeminiProvider::client_model_stamp`]).
/// Idempotent: a stamp it produced maps to itself.
fn client_facing_model(requested: &str, effort: Option<&str>, shape: &GeminiShape) -> String {
    let requested = requested.trim();
    if requested.is_empty() {
        return shape
            .client_model
            .clone()
            .unwrap_or_else(|| resolve_model(None, effort, &shape.model));
    }
    let base = strip_client_context_suffix(requested).to_ascii_lowercase();
    if is_alias(&base) {
        if let Some(id) = gemini_upstream_id(&base, effort) {
            return format!("{id}{CLIENT_CONTEXT_SUFFIX}");
        }
    }
    requested.to_string()
}

fn invalid(path: &str, message: &str) -> ProviderError {
    ProviderError::InvalidRequest(format!("{path}: {message}"))
}

/// A required string field of a block (may be empty).
fn str_field<'a>(path: &str, block: &'a Value, field: &str) -> Result<&'a str, ProviderError> {
    block
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("{path}.{field}"), "expected a string"))
}

/// A required non-empty string field of a block.
fn nonempty_str<'a>(path: &str, block: &'a Value, field: &str) -> Result<&'a str, ProviderError> {
    block
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(&format!("{path}.{field}"), "expected a non-empty string"))
}

fn push_text(parts: &mut Vec<Value>, text: &str) {
    if !text.is_empty() {
        parts.push(json!({"text": text}));
    }
}

/// One Gemini content under construction.
struct Turn {
    role: &'static str,
    parts: Vec<Value>,
}

/// Anthropic Messages body → Antigravity envelope, plus the client's
/// `stream` flag and what the translation left out (the compatibility
/// report [`GeminiProvider::validate_request`] exposes).
fn translate_request(
    body: &Value,
    shape: &GeminiShape,
    project: &str,
    session_id: &str,
    signatures: &SignatureCache,
) -> Result<(Value, bool, CompatibilityReport), ProviderError> {
    if !body.is_object() {
        return Err(invalid("body", "expected a JSON object"));
    }
    let client_stream = match body.get("stream") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(invalid("stream", "expected a boolean")),
    };
    let model = resolve_model(requested_model(body), requested_effort(body), &shape.model);

    let mut report = CompatibilityReport::default();
    let mut turns = translate_messages(body.get("messages"), &mut report)?;
    attach_signatures(&mut turns, session_id, signatures);
    let contents: Vec<Value> = turns
        .into_iter()
        .map(|turn| json!({"role": turn.role, "parts": turn.parts}))
        .collect();

    let mut request = Map::new();
    request.insert("contents".into(), Value::Array(contents));
    if let Some(system) = system_instruction(body.get("system"))? {
        request.insert("systemInstruction".into(), system);
    }
    let declarations = function_declarations(body.get("tools"), &mut report)?;
    let calling_config = function_calling_config(body.get("tool_choice"))?;
    // Gemini has no switch for parallel calls: the request to forbid them
    // cannot be carried. Reported only when tools actually go upstream.
    if !declarations.is_empty()
        && body
            .pointer("/tool_choice/disable_parallel_tool_use")
            .and_then(Value::as_bool)
            == Some(true)
    {
        report.omit("disable_parallel_tool_use");
    }
    // Not sent by design (reference parity): the backend applies the
    // model's own output cap.
    if body.get("max_tokens").is_some_and(|value| !value.is_null()) {
        report.omit("max_tokens");
    }
    // Every request asks for thoughts (`includeThoughts`); the client's
    // budget / `disabled` is not carried. Same name codex/grok report.
    if body.get("thinking").is_some_and(|value| !value.is_null()) {
        report.omit("thinking_config");
    }
    if !declarations.is_empty() {
        request.insert(
            "tools".into(),
            json!([{"functionDeclarations": declarations}]),
        );
        request.insert(
            "toolConfig".into(),
            json!({"functionCallingConfig": calling_config}),
        );
    }
    request.insert("generationConfig".into(), generation_config(body)?);
    request.insert("sessionId".into(), Value::String(session_id.to_string()));

    let envelope = json!({
        "model": model,
        "project": project,
        "userAgent": "antigravity",
        "requestType": "agent",
        "requestId": format!("agent-{}", responses::uuid_v4()),
        "request": Value::Object(request),
    });
    Ok((envelope, client_stream, report.finish()))
}

/// `messages[]` → Gemini turns: roles mapped, empty turns dropped, then
/// adjacent same-role turns merged (drop BEFORE merge, so a thinking-only
/// assistant turn never ships an empty `parts`).
fn translate_messages(
    messages: Option<&Value>,
    report: &mut CompatibilityReport,
) -> Result<Vec<Turn>, ProviderError> {
    let messages = messages
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("messages", "expected an array"))?;
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut turns: Vec<Turn> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let path = format!("messages[{index}]");
        let role = match message.get("role").and_then(Value::as_str) {
            Some("user") => "user",
            Some("assistant") => "model",
            Some(other) => {
                return Err(invalid(
                    &format!("{path}.role"),
                    &format!("unsupported role `{other}` (user, assistant)"),
                ))
            }
            None => return Err(invalid(&format!("{path}.role"), "expected a string")),
        };
        let parts = message_parts(&path, message, &mut tool_names, report)?;
        if parts.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some(last) if last.role == role => last.parts.extend(parts),
            _ => turns.push(Turn { role, parts }),
        }
    }
    if turns.is_empty() {
        return Err(invalid(
            "messages",
            "no message carries content the gemini backend can take",
        ));
    }
    Ok(turns)
}

fn message_parts(
    path: &str,
    message: &Value,
    tool_names: &mut HashMap<String, String>,
    report: &mut CompatibilityReport,
) -> Result<Vec<Value>, ProviderError> {
    let mut parts = Vec::new();
    match message.get("content") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => push_text(&mut parts, text),
        Some(Value::Array(blocks)) => {
            for (block_index, block) in blocks.iter().enumerate() {
                let bpath = format!("{path}.content[{block_index}]");
                let block_type = block.get("type").and_then(Value::as_str).ok_or_else(|| {
                    invalid(&bpath, "expected a content block with a string `type`")
                })?;
                match block_type {
                    "text" => push_text(&mut parts, str_field(&bpath, block, "text")?),
                    "image" | "document" => {
                        if let Some(part) = media_part(&bpath, block_type, block)? {
                            parts.push(part);
                        }
                    }
                    "tool_use" => {
                        let id = nonempty_str(&bpath, block, "id")?;
                        let name = nonempty_str(&bpath, block, "name")?;
                        let args = match block.get("input") {
                            None => json!({}),
                            Some(input) if input.is_object() => input.clone(),
                            Some(_) => {
                                return Err(invalid(
                                    &format!("{bpath}.input"),
                                    "expected a JSON object",
                                ))
                            }
                        };
                        tool_names.insert(id.to_string(), name.to_string());
                        parts.push(json!({"functionCall": {"name": name, "args": args, "id": id}}));
                    }
                    "tool_result" => parts.push(function_response(&bpath, block, tool_names)?),
                    // Prior-turn reasoning: an Anthropic signature is not a
                    // Gemini thought signature, and v1 has no replay bridge.
                    // Left out and reported under the codex/grok names.
                    "thinking" => report.omit("thinking"),
                    "redacted_thinking" => report.omit("redacted_thinking"),
                    other => {
                        return Err(invalid(
                            &format!("{bpath}.type"),
                            &format!("unsupported content block type `{other}`"),
                        ))
                    }
                }
            }
        }
        Some(_) => {
            return Err(invalid(
                &format!("{path}.content"),
                "expected a string or an array of content blocks",
            ))
        }
    }
    Ok(parts)
}

/// An `image` / `document` block → an `inlineData` part (base64 image or
/// PDF) or a text part (text document); `None` for an empty text document.
/// URL sources and other media are refused locally — fetching would make
/// llmux an SSRF proxy, and the message never echoes the payload.
fn media_part(path: &str, kind: &str, block: &Value) -> Result<Option<Value>, ProviderError> {
    let source = block
        .get("source")
        .filter(|source| source.is_object())
        .ok_or_else(|| invalid(&format!("{path}.source"), "expected an object"))?;
    let source_path = format!("{path}.source");
    let source_type = str_field(&source_path, source, "type")?;
    match (kind, source_type) {
        (_, "base64") => {
            let media_type = str_field(&source_path, source, "media_type")?;
            let accepted = if kind == "image" {
                IMAGE_MEDIA_TYPES.contains(&media_type)
            } else {
                media_type == PDF_MEDIA_TYPE
            };
            if !accepted {
                let takes = if kind == "image" {
                    IMAGE_MEDIA_TYPES.join(", ")
                } else {
                    PDF_MEDIA_TYPE.to_string()
                };
                return Err(invalid(
                    &format!("{source_path}.media_type"),
                    &format!(
                        "unsupported {kind} media type `{media_type}` (gemini accepts {takes})"
                    ),
                ));
            }
            let data = nonempty_str(&source_path, source, "data")?;
            Ok(Some(
                json!({"inlineData": {"mimeType": media_type, "data": data}}),
            ))
        }
        ("document", "text") => {
            let data = str_field(&source_path, source, "data")?;
            Ok((!data.is_empty()).then(|| json!({"text": data})))
        }
        (_, "url") => Err(invalid(
            &format!("{source_path}.type"),
            &format!("remote {kind} URLs are not supported; send it as base64"),
        )),
        (_, other) => Err(invalid(
            &format!("{source_path}.type"),
            &format!("unsupported {kind} source type `{other}`"),
        )),
    }
}

/// A `tool_result` block → `functionResponse {name, id, response:{result |
/// error}}`; the name comes from the earlier `tool_use` with the same id.
/// Images / PDFs in its content go to `functionResponse.parts[].inlineData`.
fn function_response(
    path: &str,
    block: &Value,
    tool_names: &HashMap<String, String>,
) -> Result<Value, ProviderError> {
    let id = nonempty_str(path, block, "tool_use_id")?;
    let name = tool_names.get(id).ok_or_else(|| {
        invalid(
            &format!("{path}.tool_use_id"),
            "no earlier tool_use carries this id",
        )
    })?;
    let is_error = match block.get("is_error") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(invalid(&format!("{path}.is_error"), "expected a boolean")),
    };
    let mut texts: Vec<String> = Vec::new();
    let mut media: Vec<Value> = Vec::new();
    match block.get("content") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => texts.push(text.clone()),
        Some(Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                let ipath = format!("{path}.content[{index}]");
                let item_type = item.get("type").and_then(Value::as_str).ok_or_else(|| {
                    invalid(&ipath, "expected a content block with a string `type`")
                })?;
                match item_type {
                    "text" => texts.push(str_field(&ipath, item, "text")?.to_string()),
                    "image" | "document" => match media_part(&ipath, item_type, item)? {
                        Some(Value::Object(part)) if part.contains_key("text") => {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                texts.push(text.to_string());
                            }
                        }
                        Some(part) => media.push(part),
                        None => {}
                    },
                    other => {
                        return Err(invalid(
                            &format!("{ipath}.type"),
                            &format!("unsupported tool_result content type `{other}`"),
                        ))
                    }
                }
            }
        }
        Some(_) => {
            return Err(invalid(
                &format!("{path}.content"),
                "expected a string or an array of content blocks",
            ))
        }
    }
    let mut response = Map::new();
    response.insert(
        if is_error { "error" } else { "result" }.to_string(),
        Value::String(texts.join("\n")),
    );
    let mut function_response = json!({"name": name, "id": id, "response": response});
    if !media.is_empty() {
        function_response["parts"] = Value::Array(media);
    }
    Ok(json!({"functionResponse": function_response}))
}

/// Replay what the converter cached under each client tool id: ONE snapshot
/// of the cache ([`snapshot_replayed_calls`]), then applied to every part
/// ([`apply_replayed_calls`]). The cache is not consulted twice, so a call
/// and its response can never see different entries — two lookups could
/// straddle another request's insert evicting the entry, sending one half
/// of the pair under Google's id and the other under the client id.
fn attach_signatures(turns: &mut [Turn], session_id: &str, signatures: &SignatureCache) {
    let snapshot = snapshot_replayed_calls(turns, session_id, signatures);
    apply_replayed_calls(turns, &snapshot);
}

/// The `(part key, client tool id)` a `functionCall` / `functionResponse`
/// part replays, if it is one.
fn replayed_call(part: &Value) -> Option<(&'static str, &str)> {
    ["functionCall", "functionResponse"]
        .into_iter()
        .find_map(|kind| Some((kind, part.get(kind)?.get("id")?.as_str()?)))
}

/// Every cache entry for a client tool id replayed anywhere in `turns`
/// (`functionCall.id` or `functionResponse.id`), read in one
/// [`SignatureCache::get_many`] call. Misses are absent.
fn snapshot_replayed_calls(
    turns: &[Turn],
    session_id: &str,
    signatures: &SignatureCache,
) -> HashMap<String, CachedCall> {
    signatures.get_many(
        session_id,
        turns
            .iter()
            .flat_map(|turn| &turn.parts)
            .filter_map(replayed_call)
            .map(|(_, client_id)| client_id),
    )
}

/// Apply a [`snapshot_replayed_calls`] snapshot. Every `functionCall` and
/// `functionResponse` whose id the snapshot holds with an upstream id goes
/// back upstream under Google's own call id. Per model turn, the FIRST
/// `functionCall` part carries its cached signature or, without one,
/// [`SKIP_THOUGHT_SIGNATURE`]; later parallel calls carry their cached
/// signature or none (Gemini signs only the first call of a parallel step —
/// reference parity). A miss leaves the client id as it is.
fn apply_replayed_calls(turns: &mut [Turn], snapshot: &HashMap<String, CachedCall>) {
    for turn in turns.iter_mut() {
        let model_turn = turn.role == "model";
        let mut first = true;
        for part in &mut turn.parts {
            let Some((kind, client_id)) =
                replayed_call(part).map(|(kind, id)| (kind, id.to_string()))
            else {
                continue;
            };
            let cached = snapshot.get(&client_id).cloned();
            if let Some(upstream_id) = cached.as_ref().and_then(|call| call.upstream_id.clone()) {
                if let Some(target) = part.get_mut(kind).and_then(Value::as_object_mut) {
                    target.insert("id".into(), Value::String(upstream_id));
                }
            }
            if !model_turn || kind != "functionCall" {
                continue;
            }
            let signature = match cached.and_then(|call| call.signature) {
                Some(cached) => Some(cached),
                None if first => Some(SKIP_THOUGHT_SIGNATURE.to_string()),
                None => None,
            };
            first = false;
            if let Some(signature) = signature {
                part["thoughtSignature"] = Value::String(signature);
            }
        }
    }
}

/// `system` (string or text blocks) → `systemInstruction`, or `None` when
/// it carries no text.
fn system_instruction(system: Option<&Value>) -> Result<Option<Value>, ProviderError> {
    let mut parts = Vec::new();
    match system {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => push_text(&mut parts, text),
        Some(Value::Array(blocks)) => {
            for (index, block) in blocks.iter().enumerate() {
                let path = format!("system[{index}]");
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => push_text(&mut parts, str_field(&path, block, "text")?),
                    Some(other) => {
                        return Err(invalid(
                            &format!("{path}.type"),
                            &format!("system carries text blocks only (got `{other}`)"),
                        ))
                    }
                    None => {
                        return Err(invalid(
                            &path,
                            "expected a content block with a string `type`",
                        ))
                    }
                }
            }
        }
        Some(_) => {
            return Err(invalid(
                "system",
                "expected a string or an array of text blocks",
            ))
        }
    }
    Ok((!parts.is_empty()).then(|| json!({"role": "user", "parts": parts})))
}

/// `tools[]` → `functionDeclarations`. A tool without `input_schema` is a
/// server tool (web search, …) the backend cannot run: dropped, logged and
/// reported as `server_tools`.
fn function_declarations(
    tools: Option<&Value>,
    report: &mut CompatibilityReport,
) -> Result<Vec<Value>, ProviderError> {
    let tools = match tools {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(tools)) => tools,
        Some(_) => return Err(invalid("tools", "expected an array")),
    };
    let mut declarations = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        let path = format!("tools[{index}]");
        if !tool.is_object() {
            return Err(invalid(&path, "expected an object"));
        }
        let Some(schema) = tool.get("input_schema").filter(|schema| !schema.is_null()) else {
            let tool_name = tool.get("name").and_then(Value::as_str).unwrap_or("");
            let tool_kind = tool.get("type").and_then(Value::as_str).unwrap_or("");
            tracing::info!(
                provider = "gemini",
                tool = tool_name,
                kind = tool_kind,
                "dropping a tool without input_schema (server tool)"
            );
            report.omit("server_tools");
            continue;
        };
        if !schema.is_object() {
            return Err(invalid(
                &format!("{path}.input_schema"),
                "expected a JSON object",
            ));
        }
        let name = nonempty_str(&path, tool, "name")?;
        let mut declaration = Map::new();
        declaration.insert("name".into(), Value::String(name.to_string()));
        if let Some(description) = tool
            .get("description")
            .and_then(Value::as_str)
            .filter(|description| !description.is_empty())
        {
            declaration.insert("description".into(), Value::String(description.to_string()));
        }
        declaration.insert("parametersJsonSchema".into(), strip_schema_keys(schema));
        declarations.push(Value::Object(declaration));
    }
    Ok(declarations)
}

/// `schema` with every [`STRIPPED_SCHEMA_KEYS`] keyword removed from it and
/// from each subschema it holds ([`SUBSCHEMA_KEYS`],
/// [`SUBSCHEMA_ARRAY_KEYS`], the values of [`SUBSCHEMA_MAP_KEYS`]). Names
/// under `properties` & co. and every other keyword's value — `const`,
/// `enum`, `default`, `examples` — are data and copied verbatim. A
/// non-object (boolean schema) is returned as is.
fn strip_schema_keys(schema: &Value) -> Value {
    let Value::Object(map) = schema else {
        return schema.clone();
    };
    Value::Object(
        map.iter()
            .filter(|(key, _)| !STRIPPED_SCHEMA_KEYS.contains(&key.as_str()))
            .map(|(key, value)| {
                let keyword = key.as_str();
                let value = if SUBSCHEMA_MAP_KEYS.contains(&keyword) {
                    match value {
                        Value::Object(named) => Value::Object(
                            named
                                .iter()
                                .map(|(name, sub)| (name.clone(), strip_subschemas(sub)))
                                .collect(),
                        ),
                        other => other.clone(),
                    }
                } else if SUBSCHEMA_KEYS.contains(&keyword)
                    || SUBSCHEMA_ARRAY_KEYS.contains(&keyword)
                {
                    strip_subschemas(value)
                } else {
                    value.clone()
                };
                (key.clone(), value)
            })
            .collect(),
    )
}

/// A subschema position: one schema, or an array of them (each stripped).
fn strip_subschemas(value: &Value) -> Value {
    match value {
        Value::Array(schemas) => Value::Array(schemas.iter().map(strip_schema_keys).collect()),
        schema => strip_schema_keys(schema),
    }
}

/// `tool_choice` → `functionCallingConfig` (absent → `AUTO`).
fn function_calling_config(choice: Option<&Value>) -> Result<Value, ProviderError> {
    let Some(choice) = choice.filter(|choice| !choice.is_null()) else {
        return Ok(json!({"mode": "AUTO"}));
    };
    if !choice.is_object() {
        return Err(invalid("tool_choice", "expected an object"));
    }
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Ok(json!({"mode": "AUTO"})),
        Some("any") => Ok(json!({"mode": "ANY"})),
        Some("none") => Ok(json!({"mode": "NONE"})),
        Some("tool") => {
            let name = nonempty_str("tool_choice", choice, "name")?;
            Ok(json!({"mode": "ANY", "allowedFunctionNames": [name]}))
        }
        Some(other) => Err(invalid(
            "tool_choice.type",
            &format!("unsupported tool_choice `{other}` (auto, any, tool, none)"),
        )),
        None => Err(invalid("tool_choice.type", "expected a string")),
    }
}

/// `generationConfig`: thoughts always included; sampling knobs mapped;
/// `max_tokens` deliberately NOT sent (the backend applies the model's own
/// cap — reference parity).
fn generation_config(body: &Value) -> Result<Value, ProviderError> {
    let mut config = Map::new();
    config.insert("thinkingConfig".into(), json!({"includeThoughts": true}));
    for (from, to) in [
        ("temperature", "temperature"),
        ("top_p", "topP"),
        ("top_k", "topK"),
    ] {
        match body.get(from) {
            None | Some(Value::Null) => {}
            Some(value) if value.is_number() => {
                config.insert(to.into(), value.clone());
            }
            Some(_) => return Err(invalid(from, "expected a number")),
        }
    }
    match body.get("stop_sequences") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
            if !items.is_empty() {
                config.insert("stopSequences".into(), Value::Array(items.clone()));
            }
        }
        Some(_) => return Err(invalid("stop_sequences", "expected an array of strings")),
    }
    Ok(Value::Object(config))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    ToolUse,
}

/// One Anthropic content block, kept for the non-streaming aggregate.
#[derive(Debug)]
struct Block {
    kind: BlockKind,
    text: String,
    tool_id: String,
    tool_name: String,
    input: Value,
}

impl Block {
    fn text(kind: BlockKind) -> Self {
        Self {
            kind,
            text: String::new(),
            tool_id: String::new(),
            tool_name: String::new(),
            input: Value::Null,
        }
    }
}

/// Stateful Gemini-SSE → Anthropic-SSE converter (spec §R4). One instance per
/// upstream response; feed COMPLETE upstream events in, get well-formed
/// Anthropic SSE bytes out (`event: <type>\ndata: <json>\n\n`).
///
/// Gemini parts arrive in order and never interleave, so at most one block
/// is open and it is always the last one: consecutive same-kind text parts
/// extend it, anything else closes it. The terminal `message_delta` is
/// emitted at [`SseTransform::on_end`], not at `finishReason`, because
/// `usageMetadata` may arrive on several events and the LAST is
/// authoritative.
#[derive(Debug)]
pub struct GeminiSseConverter {
    /// Upstream model id (internal use; the stamp prefers `client_model`).
    model: String,
    /// Model NAME stamped into the client-facing `message_start` / aggregate.
    client_model: Option<String>,
    /// Signature-cache scope (`sessionId` of the request this answers).
    session_id: String,
    signatures: Option<Arc<SignatureCache>>,
    started: bool,
    finished: bool,
    message_id: String,
    next_index: usize,
    /// Wire index of the open block (always the last entry of `blocks`).
    open_index: Option<usize>,
    blocks: Vec<Block>,
    saw_tool_use: bool,
    finish_reason: Option<String>,
    finish_message: Option<String>,
    stop_reason: Option<String>,
    error: Option<String>,
    /// `input_tokens` is the FRESH prompt count (prompt − cached), matching
    /// Anthropic's convention.
    usage: StreamUsage,
    /// Verbatim last `usageMetadata`, for the trace.
    raw_usage: Option<Value>,
    events_seen: u64,
    /// The client asked for tool-use review; see
    /// [`Self::with_safeguards_requested`].
    safeguards_requested: bool,
    /// Tool ids announced in a streamed `content_block_start`, in order.
    announced_tool_ids: Vec<String>,
}

impl GeminiSseConverter {
    /// Converter that stamps `model` into the synthesized Anthropic response.
    pub fn with_model(model: String) -> Self {
        Self {
            model,
            client_model: None,
            session_id: String::new(),
            signatures: None,
            started: false,
            finished: false,
            message_id: String::new(),
            next_index: 0,
            open_index: None,
            blocks: Vec::new(),
            saw_tool_use: false,
            finish_reason: None,
            finish_message: None,
            stop_reason: None,
            error: None,
            usage: StreamUsage::default(),
            raw_usage: None,
            events_seen: 0,
            safeguards_requested: false,
            announced_tool_ids: Vec::new(),
        }
    }

    /// Set the client-facing model-name override. Builder-style.
    pub fn with_client_model(mut self, client_model: Option<String>) -> Self {
        self.client_model = client_model;
        self
    }

    /// Answer the client's `dangerous_tool_use` safeguard with
    /// `safeguard_results` on the terminal `message_delta` and the aggregate:
    /// every tool call per-call `unavailable` (no reviewer behind this
    /// backend). Silence would latch Claude Code's whole session — Anthropic
    /// turns included — to local classification.
    pub fn with_safeguards_requested(mut self, requested: bool) -> Self {
        self.safeguards_requested = requested;
        self
    }

    /// Record each streamed `functionCall`'s signature and upstream id into
    /// `signatures` under (`session_id`, minted client id). Builder-style.
    pub fn with_signatures(mut self, signatures: Arc<SignatureCache>, session_id: String) -> Self {
        self.signatures = Some(signatures);
        self.session_id = session_id;
        self
    }

    fn client_facing_model(&self) -> &str {
        self.client_model.as_deref().unwrap_or(&self.model)
    }

    fn emit(out: &mut Vec<u8>, event_type: &str, data: &Value) {
        out.extend_from_slice(format!("event: {event_type}\ndata: {data}\n\n").as_bytes());
    }

    fn ensure_started(&mut self, out: &mut Vec<u8>, response: &Value) {
        if self.started {
            return;
        }
        self.started = true;
        self.message_id = match response
            .get("responseId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            Some(id) => format!("msg_{id}"),
            None => format!(
                "msg_gemini_{}",
                ulid::Ulid::new().to_string().to_lowercase()
            ),
        };
        Self::emit(
            out,
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.client_facing_model(),
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": 0, "output_tokens": 0},
                },
            }),
        );
    }

    fn open_block(&mut self, out: &mut Vec<u8>, block: Block) {
        let index = self.next_index;
        self.next_index += 1;
        let content_block = match block.kind {
            BlockKind::Text => json!({"type": "text", "text": ""}),
            BlockKind::Thinking => json!({"type": "thinking", "thinking": "", "signature": ""}),
            BlockKind::ToolUse => json!({
                "type": "tool_use",
                "id": block.tool_id,
                "name": block.tool_name,
                "input": {},
            }),
        };
        if block.kind == BlockKind::ToolUse {
            self.announced_tool_ids.push(block.tool_id.clone());
        }
        self.blocks.push(block);
        self.open_index = Some(index);
        Self::emit(
            out,
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": index,
                "content_block": content_block,
            }),
        );
    }

    fn delta(out: &mut Vec<u8>, index: usize, delta: Value) {
        Self::emit(
            out,
            "content_block_delta",
            &json!({"type": "content_block_delta", "index": index, "delta": delta}),
        );
    }

    /// Close the open block. A thinking block gets an EMPTY `signature_delta`
    /// first: it is unsigned, so the Anthropic path's `strip_foreign_thinking`
    /// removes it after a model switch.
    fn close_open(&mut self, out: &mut Vec<u8>) {
        let Some(index) = self.open_index.take() else {
            return;
        };
        if self
            .blocks
            .last()
            .is_some_and(|block| block.kind == BlockKind::Thinking)
        {
            Self::delta(
                out,
                index,
                json!({"type": "signature_delta", "signature": ""}),
            );
        }
        Self::emit(
            out,
            "content_block_stop",
            &json!({"type": "content_block_stop", "index": index}),
        );
    }

    fn push_text(&mut self, out: &mut Vec<u8>, kind: BlockKind, text: &str) {
        let continues =
            self.open_index.is_some() && self.blocks.last().is_some_and(|block| block.kind == kind);
        if !continues {
            self.close_open(out);
            self.open_block(out, Block::text(kind));
        }
        let Some(index) = self.open_index else {
            return;
        };
        if let Some(block) = self.blocks.last_mut() {
            block.text.push_str(text);
        }
        let delta = match kind {
            BlockKind::Thinking => json!({"type": "thinking_delta", "thinking": text}),
            _ => json!({"type": "text_delta", "text": text}),
        };
        Self::delta(out, index, delta);
    }

    /// A `functionCall` part → one complete `tool_use` block (start, a single
    /// `input_json_delta` with the full args, stop) under a freshly minted
    /// client id, recording the call's signature and upstream id under it.
    fn push_call(&mut self, out: &mut Vec<u8>, part: &Value, call: &Value) {
        let Some(name) = call
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        else {
            self.fail(out, "gemini upstream sent a functionCall without a name");
            return;
        };
        let input = match call.get("args") {
            None | Some(Value::Null) => json!({}),
            Some(args) if args.is_object() => args.clone(),
            Some(_) => {
                self.fail(
                    out,
                    &format!("gemini upstream tool call {name:?} sent non-object args"),
                );
                return;
            }
        };
        // Never Google's id: it repeats (`call_7`) across conversations that
        // share a session id, and the cache key must not. The upstream id
        // rides in the cache and goes back up when the client replays.
        let id = format!("toolu_gemini_{}", responses::uuid_v4().replace('-', ""));
        if let Some(cache) = &self.signatures {
            cache.insert(
                &self.session_id,
                &id,
                CachedCall {
                    signature: part
                        .get("thoughtSignature")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    upstream_id: call.get("id").and_then(Value::as_str).map(str::to_string),
                },
            );
        }
        self.close_open(out);
        self.saw_tool_use = true;
        let args_json = input.to_string();
        self.open_block(
            out,
            Block {
                kind: BlockKind::ToolUse,
                text: String::new(),
                tool_id: id,
                tool_name: name.to_string(),
                input,
            },
        );
        if let Some(index) = self.open_index {
            Self::delta(
                out,
                index,
                json!({"type": "input_json_delta", "partial_json": args_json}),
            );
        }
        self.close_open(out);
    }

    /// Fold one `usageMetadata` (the last one wins). An absent
    /// `cachedContentTokenCount` is a real zero (proto3 JSON omits zeros).
    fn record_usage(&mut self, usage: &Value) {
        let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let prompt = count("promptTokenCount");
        let cached = count("cachedContentTokenCount");
        if cached > prompt {
            tracing::warn!(
                provider = "gemini",
                cached_tokens = cached,
                prompt_tokens = prompt,
                "malformed upstream usage (cachedContentTokenCount > promptTokenCount); clamping"
            );
        }
        let cached = cached.min(prompt);
        self.raw_usage = Some(usage.clone());
        self.usage = StreamUsage {
            input_tokens: prompt - cached,
            output_tokens: count("candidatesTokenCount")
                .saturating_add(count("thoughtsTokenCount")),
            cache_read_input_tokens: Some(cached),
            cache_creation_input_tokens: None,
        };
    }

    fn usage_json(&self) -> Value {
        json!({
            "input_tokens": self.usage.input_tokens,
            "cache_read_input_tokens": self.usage.cache_read_input_tokens.unwrap_or(0),
            "cache_creation_input_tokens": self.usage.cache_creation_input_tokens.unwrap_or(0),
            "output_tokens": self.usage.output_tokens,
        })
    }

    fn fail(&mut self, out: &mut Vec<u8>, message: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.error = Some(message.to_string());
        Self::emit(
            out,
            "error",
            &json!({
                "type": "error",
                "error": {"type": "api_error", "message": message},
            }),
        );
    }

    /// Terminal Anthropic tail: close the open block, `message_delta` (stop
    /// reason, usage, `safeguard_results` when asked) and `message_stop`.
    fn complete(&mut self, out: &mut Vec<u8>, stop_reason: &str) {
        self.close_open(out);
        self.stop_reason = Some(stop_reason.to_string());
        let mut delta = json!({"stop_reason": stop_reason, "stop_sequence": null});
        if self.safeguards_requested {
            delta["safeguard_results"] =
                unavailable_safeguard_results(self.announced_tool_ids.iter().map(String::as_str));
        }
        Self::emit(
            out,
            "message_delta",
            &json!({"type": "message_delta", "delta": delta, "usage": self.usage_json()}),
        );
        Self::emit(out, "message_stop", &json!({"type": "message_stop"}));
        self.finished = true;
    }

    /// Build the single (non-streaming) Anthropic Messages response from the
    /// fully consumed stream. `None` when the stream ended in an error —
    /// callers surface [`Self::error_message`] instead.
    pub fn into_message_json(self) -> Option<Value> {
        if self.error.is_some() {
            return None;
        }
        let content: Vec<Value> = self
            .blocks
            .iter()
            .map(|block| match block.kind {
                BlockKind::Text => json!({"type": "text", "text": block.text}),
                BlockKind::Thinking => {
                    json!({"type": "thinking", "thinking": block.text, "signature": ""})
                }
                BlockKind::ToolUse => json!({
                    "type": "tool_use",
                    "id": block.tool_id,
                    "name": block.tool_name,
                    "input": block.input,
                }),
            })
            .collect();
        let mut message = json!({
            "id": if self.message_id.is_empty() {
                format!("msg_gemini_{}", ulid::Ulid::new().to_string().to_lowercase())
            } else {
                self.message_id.clone()
            },
            "type": "message",
            "role": "assistant",
            "model": self.client_facing_model(),
            "content": content,
            "stop_reason": self.stop_reason.as_deref().unwrap_or("end_turn"),
            "stop_sequence": null,
            "usage": self.usage_json(),
        });
        if self.safeguards_requested {
            message["safeguard_results"] = unavailable_safeguard_results(
                self.blocks
                    .iter()
                    .filter(|block| block.kind == BlockKind::ToolUse)
                    .map(|block| block.tool_id.as_str()),
            );
        }
        Some(message)
    }

    /// Upstream error message, when the stream ended in an error.
    pub fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Verbatim last upstream `usageMetadata`, for the trace.
    pub fn raw_usage(&self) -> Option<&Value> {
        self.raw_usage.as_ref()
    }

    /// Count of real upstream SSE events parsed so far (keepalives, `[DONE]`
    /// and unparseable lines excluded).
    pub fn events_seen(&self) -> u64 {
        self.events_seen
    }
}

/// Concatenated `data:` payload of one SSE event.
fn event_data(event: &str) -> Option<String> {
    let lines: Vec<&str> = event
        .lines()
        .filter_map(|line| {
            line.strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
        })
        .collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Human-readable message of an upstream `error` value (Google RPC object
/// `{code, message, status}` or a plain string).
fn upstream_error_message(error: &Value) -> String {
    let message = error
        .as_str()
        .or_else(|| error.get("message").and_then(Value::as_str))
        .unwrap_or("upstream error");
    match error.get("status").and_then(Value::as_str) {
        Some(status) => format!("gemini upstream error ({status}): {message}"),
        None => format!("gemini upstream error: {message}"),
    }
}

impl SseTransform for GeminiSseConverter {
    fn on_event(&mut self, event: &str) -> Vec<u8> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        let Some(data) = event_data(event) else {
            return out; // comment/keepalive lines
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return out;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            tracing::debug!(provider = "gemini", "unparseable upstream SSE data dropped");
            return out;
        };
        self.events_seen += 1;
        if let Some(error) = value.get("error") {
            self.fail(&mut out, &upstream_error_message(error));
            return out;
        }
        // Cloud Code wraps the GenerateContentResponse in `response`; accept
        // the bare shape too.
        let response = value.get("response").unwrap_or(&value);
        self.ensure_started(&mut out, response);
        if let Some(usage) = response.get("usageMetadata") {
            self.record_usage(usage);
        }
        if let Some(reason) = response
            .get("promptFeedback")
            .and_then(|feedback| feedback.get("blockReason"))
            .and_then(Value::as_str)
        {
            self.fail(
                &mut out,
                &format!("gemini upstream blocked the prompt ({reason})"),
            );
            return out;
        }
        let Some(candidate) = response
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
        else {
            return out;
        };
        if let Some(parts) = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                if self.finished {
                    return out;
                }
                if let Some(call) = part.get("functionCall") {
                    self.push_call(&mut out, part, call);
                    continue;
                }
                match part.get("text").and_then(Value::as_str) {
                    // Empty texts (the trailing `{thoughtSignature, text:""}`
                    // part) never open a block.
                    Some("") => {}
                    Some(text) => {
                        let kind = if part.get("thought").and_then(Value::as_bool) == Some(true) {
                            BlockKind::Thinking
                        } else {
                            BlockKind::Text
                        };
                        self.push_text(&mut out, kind, text);
                    }
                    None => {
                        tracing::debug!(provider = "gemini", "unsupported response part dropped")
                    }
                }
            }
        }
        if let Some(reason) = candidate
            .get("finishReason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty())
        {
            self.finish_reason = Some(reason.to_string());
            self.finish_message = candidate
                .get("finishMessage")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        out
    }

    fn on_end(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        if !self.started {
            // A 2xx whose body was not SSE at all: terminate it with a clean
            // Anthropic error rather than ending silently.
            self.fail(&mut out, "gemini upstream returned no SSE events");
            return out;
        }
        let reason = self.finish_reason.clone();
        match reason.as_deref() {
            None => self.fail(
                &mut out,
                "gemini upstream stream ended before a finishReason",
            ),
            Some("STOP") => {
                let stop = if self.saw_tool_use {
                    "tool_use"
                } else {
                    "end_turn"
                };
                self.complete(&mut out, stop);
            }
            Some("MAX_TOKENS") => self.complete(&mut out, "max_tokens"),
            Some("SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII") => {
                self.complete(&mut out, "refusal")
            }
            // MALFORMED_FUNCTION_CALL and anything unknown: no faithful
            // Anthropic stop reason, so never a synthesized success.
            Some(other) => {
                let detail = self
                    .finish_message
                    .as_deref()
                    .map(|message| format!(": {message}"))
                    .unwrap_or_default();
                self.fail(
                    &mut out,
                    &format!("gemini upstream finished with {other}{detail}"),
                );
            }
        }
        out
    }

    fn usage(&self) -> StreamUsage {
        self.usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured-shape fixtures (live upstream, 2026-10-15).
    const LIVE_SIGNATURE: &str = "CiQBjz1rX-captured-thought-signature-584-chars-elided";

    fn credential(project: &str) -> AccountCredential {
        AccountCredential::Gemini {
            email: "user@example.com".into(),
            access_token: "ya29.token".into(),
            refresh_token: "1//refresh".into(),
            expires_at_ms: 0,
            project_id: project.into(),
            last_refresh_ms: None,
        }
    }

    fn build(provider: &GeminiProvider, body: &Value) -> (ProviderRequest, Value, bool) {
        let (request, stream) = provider
            .build_request(body.to_string().as_bytes(), &credential("proj-123"))
            .expect("build_request");
        let upstream: Value = serde_json::from_slice(&request.body).expect("upstream json");
        (request, upstream, stream)
    }

    fn build_err(body: &Value) -> String {
        match GeminiProvider::new("https://x")
            .build_request(body.to_string().as_bytes(), &credential("proj-123"))
        {
            Err(ProviderError::InvalidRequest(message)) => message,
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    /// One upstream Cloud Code event in the captured wire shape.
    fn chunk(parts: Value, finish: Option<&str>, usage: Option<Value>) -> Value {
        let mut candidate = json!({"content": {"role": "model", "parts": parts}});
        if let Some(finish) = finish {
            candidate["finishReason"] = json!(finish);
        }
        let mut response = json!({
            "candidates": [candidate],
            "modelVersion": "gemini-3.8-flash-n",
            "responseId": "resp-abc",
        });
        if let Some(usage) = usage {
            response["usageMetadata"] = usage;
        }
        json!({"response": response, "traceId": "trace-1", "metadata": {}})
    }

    fn usage_meta(prompt: u64, cached: Option<u64>, candidates: u64, thoughts: u64) -> Value {
        let mut usage = json!({
            "promptTokenCount": prompt,
            "candidatesTokenCount": candidates,
            "thoughtsTokenCount": thoughts,
            "totalTokenCount": prompt + candidates + thoughts,
        });
        if let Some(cached) = cached {
            usage["cachedContentTokenCount"] = json!(cached);
        }
        usage
    }

    /// Parse emitted Anthropic SSE bytes into `(event, data)` pairs, checking
    /// the `event:` name always matches `data.type`.
    fn parse_sse(bytes: &[u8]) -> Vec<(String, Value)> {
        String::from_utf8_lossy(bytes)
            .split("\n\n")
            .filter(|event| !event.trim().is_empty())
            .map(|event| {
                let mut name = String::new();
                let mut data = Value::Null;
                for line in event.lines() {
                    if let Some(rest) = line.strip_prefix("event: ") {
                        name = rest.to_string();
                    } else if let Some(rest) = line.strip_prefix("data: ") {
                        data = serde_json::from_str(rest).expect("data json");
                    }
                }
                assert_eq!(data["type"], json!(name), "event name matches data.type");
                (name, data)
            })
            .collect()
    }

    /// Feed `events` as SSE `data:` lines, then end the stream.
    fn run(converter: &mut GeminiSseConverter, events: &[Value]) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        for event in events {
            out.extend(converter.on_event(&format!("data: {event}")));
        }
        out.extend(converter.on_end());
        parse_sse(&out)
    }

    fn names(events: &[(String, Value)]) -> Vec<&str> {
        events.iter().map(|(name, _)| name.as_str()).collect()
    }

    fn find<'a>(events: &'a [(String, Value)], name: &str) -> &'a Value {
        &events
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no {name} event"))
            .1
    }

    /// Client tool ids announced by `content_block_start`, in order.
    fn tool_ids(events: &[(String, Value)]) -> Vec<String> {
        events
            .iter()
            .filter(|(name, _)| name == "content_block_start")
            .filter_map(|(_, data)| data["content_block"]["id"].as_str())
            .map(str::to_string)
            .collect()
    }

    fn tool_call_part(id: &str, name: &str, args: Value, signature: Option<&str>) -> Value {
        let mut part = json!({"functionCall": {"name": name, "args": args, "id": id}});
        if let Some(signature) = signature {
            part["thoughtSignature"] = json!(signature);
        }
        part
    }

    // ---- G2: model resolution + client-facing stamp ----

    #[test]
    fn g2_resolve_model_aliases_suffix_variants_and_fallback() {
        let pin = "gemini-3.8-flash-high";
        assert_eq!(resolve_model(Some("argon"), None, pin), "gemini-4-argon");
        assert_eq!(
            resolve_model(Some(" Argon[1M] "), None, pin),
            "gemini-4-argon"
        );
        assert_eq!(
            resolve_model(Some("gemini"), Some("low"), pin),
            "gemini-3.8-flash-high"
        );
        assert_eq!(
            resolve_model(Some("gemini-4-argon[1m]"), None, pin),
            "gemini-4-argon"
        );
        assert_eq!(
            resolve_model(Some("GEMINI-3.8-flash-low[1m]"), None, pin),
            "gemini-3.8-flash-low"
        );
        for (effort, variant) in [
            (Some("none"), "gemini-3.8-flash-low"),
            (Some("minimal"), "gemini-3.8-flash-low"),
            (Some("low"), "gemini-3.8-flash-low"),
            (Some("medium"), "gemini-3.8-flash-medium"),
            (Some("high"), "gemini-3.8-flash-high"),
            (Some("xhigh"), "gemini-3.8-flash-high"),
            (None, "gemini-3.8-flash-high"),
        ] {
            assert_eq!(
                resolve_model(Some("gemini-3.8-flash"), effort, pin),
                variant,
                "effort {effort:?}"
            );
        }
        assert_eq!(
            resolve_model(Some("gemini-3.8-flash[1m]"), Some("medium"), pin),
            "gemini-3.8-flash-medium"
        );
        // Any other gemini-* id passes verbatim (minus the suffix).
        assert_eq!(
            resolve_model(Some("gemini-9-future"), None, pin),
            "gemini-9-future"
        );
        // Non-gemini names and model-less requests use the pin, which gets
        // the same suffix/alias treatment.
        assert_eq!(resolve_model(Some("claude-sonnet-4-5"), None, pin), pin);
        assert_eq!(resolve_model(None, None, pin), pin);
        assert_eq!(
            resolve_model(
                Some("claude-sonnet-4-5"),
                None,
                "gemini-3.8-flash-medium[1m]"
            ),
            "gemini-3.8-flash-medium"
        );
        assert_eq!(resolve_model(None, None, "argon"), "gemini-4-argon");
    }

    #[test]
    fn g2_client_stamp_maps_aliases_to_1m_rows_and_keeps_ids() {
        let provider = GeminiProvider::new("https://x");
        let stamp = |body: Value| provider.client_model_stamp(&body);
        assert_eq!(stamp(json!({"model": "argon"})), "gemini-4-argon[1m]");
        assert_eq!(
            stamp(json!({"model": "gemini"})),
            "gemini-3.8-flash-high[1m]"
        );
        assert_eq!(
            stamp(json!({"model": "gemini-3.8-flash", "output_config": {"effort": "low"}})),
            "gemini-3.8-flash-low[1m]"
        );
        assert_eq!(
            stamp(json!({"model": "gemini-3.8-flash-medium[1m]"})),
            "gemini-3.8-flash-medium[1m]"
        );
        assert_eq!(
            stamp(json!({"model": "gemini-3.8-flash-medium"})),
            "gemini-3.8-flash-medium"
        );
        // Non-gemini fallback: the client's own model is stamped.
        assert_eq!(stamp(json!({"model": "claude-x"})), "claude-x");
        // Model-less: the upstream id, or the configured client_model.
        assert_eq!(stamp(json!({})), "gemini-3.8-flash-high");
        provider.set_shape(GeminiShape {
            client_model: Some("gemini-3.8-flash-high[1m]".into()),
            ..GeminiShape::default()
        });
        assert_eq!(stamp(json!({})), "gemini-3.8-flash-high[1m]");
        // Idempotent: a produced stamp maps to itself.
        let shape = GeminiShape::default();
        for raw in [
            "argon",
            "gemini",
            "gemini-3.8-flash-low",
            "gemini-4-argon[1m]",
        ] {
            let once = client_facing_model(raw, None, &shape);
            assert_eq!(client_facing_model(&once, None, &shape), once, "{raw}");
        }
    }

    #[test]
    fn g2_request_meta_and_converter_stamp() {
        let provider = GeminiProvider::new("https://x");
        let body = json!({"model": "argon", "output_config": {"effort": "max"}});
        assert_eq!(
            provider.request_meta(body.to_string().as_bytes()),
            ("gemini-4-argon".to_string(), None)
        );
        assert_eq!(
            provider.request_meta(b"not json"),
            ("gemini-3.8-flash-high".to_string(), None)
        );
        assert_eq!(provider.model(), "gemini-3.8-flash-high");
        assert_eq!(provider.endpoint(), "https://x");
        let mut converter = provider.converter("argon".into(), "-1".into());
        let events = run(
            &mut converter,
            &[chunk(json!([{"text": "hi"}]), Some("STOP"), None)],
        );
        assert_eq!(events[0].1["message"]["model"], "gemini-4-argon[1m]");
        assert_eq!(events[0].1["message"]["id"], "msg_resp-abc");
    }

    // ---- G3: envelope, headers, generation config, session id ----

    #[test]
    fn g3_envelope_headers_and_generation_config() {
        let provider = GeminiProvider::new("https://daily-cloudcode-pa.googleapis.com");
        let body = json!({
            "model": "gemini",
            "stream": true,
            "max_tokens": 32000,
            "temperature": 0.5,
            "top_p": 0.9,
            "top_k": 40,
            "stop_sequences": ["END"],
            "system": [{"type": "text", "text": "be brief", "cache_control": {"type": "ephemeral"}}],
            "messages": [{"role": "user", "content": "ping"}],
        });
        let (request, upstream, stream) = build(&provider, &body);
        assert!(stream);
        assert_eq!(request.method, Method::POST);
        assert_eq!(request.path, "/v1internal:streamGenerateContent?alt=sse");
        assert_eq!(request.headers.len(), 4, "no other headers");
        assert_eq!(request.headers[AUTHORIZATION], "Bearer ya29.token");
        assert_eq!(request.headers[CONTENT_TYPE], "application/json");
        assert_eq!(request.headers[ACCEPT], "text/event-stream");
        assert_eq!(
            request.headers[USER_AGENT],
            "antigravity/hub/2.23.1 darwin/arm64"
        );

        assert_eq!(upstream["model"], "gemini-3.8-flash-high");
        assert_eq!(upstream["project"], "proj-123");
        assert_eq!(upstream["userAgent"], "antigravity");
        assert_eq!(upstream["requestType"], "agent");
        let request_id = upstream["requestId"].as_str().expect("requestId");
        assert!(
            request_id.starts_with("agent-") && request_id.len() == 42,
            "{request_id}"
        );
        let inner = &upstream["request"];
        assert_eq!(
            inner["systemInstruction"],
            json!({"role": "user", "parts": [{"text": "be brief"}]})
        );
        assert_eq!(
            inner["contents"],
            json!([{"role": "user", "parts": [{"text": "ping"}]}])
        );
        assert_eq!(
            inner["generationConfig"],
            json!({
                "thinkingConfig": {"includeThoughts": true},
                "temperature": 0.5,
                "topP": 0.9,
                "topK": 40,
                "stopSequences": ["END"],
            })
        );
        assert!(!request.body.windows(15).any(|w| w == b"maxOutputTokens"));
        assert!(inner.get("tools").is_none() && inner.get("toolConfig").is_none());
        assert_eq!(inner["sessionId"], json!(session_id_for(&body)));

        // Non-streaming client: still a streamed upstream.
        let (_, _, stream) = build(
            &provider,
            &json!({"messages": [{"role": "user", "content": "ping"}]}),
        );
        assert!(!stream);
    }

    #[test]
    fn g3_session_id_is_stable_for_equal_first_user_text() {
        let turn1 = json!({"messages": [
            {"role": "user", "content": [{"type": "text", "text": "<reminder>"}, {"type": "text", "text": "hello"}]},
        ]});
        let turn2 = json!({"messages": [
            {"role": "user", "content": [{"type": "text", "text": "<reminder>"}, {"type": "text", "text": "hello"}]},
            {"role": "assistant", "content": "hi"},
            {"role": "user", "content": "next"},
        ]});
        let id = session_id_for(&turn1);
        assert_eq!(id, session_id_for(&turn2));
        // `-` + the masked big-endian u64 of sha256("<reminder>\nhello").
        assert_eq!(id, format!("-{}", session_hash("<reminder>\nhello")));
        let digits = id.strip_prefix('-').expect("leading dash");
        assert!(digits.parse::<u64>().expect("decimal") <= SESSION_ID_MASK);
        // A string first message hashes its text verbatim.
        assert_eq!(
            session_id_for(&json!({"messages": [{"role": "user", "content": "pong"}]})),
            format!("-{}", session_hash("pong"))
        );
        assert_ne!(
            id,
            session_id_for(&json!({"messages": [{"role": "user", "content": "other"}]}))
        );
        // No user text → random per call.
        let empty = json!({"messages": []});
        let random = session_id_for(&empty);
        assert!(random.starts_with('-'));
        assert_ne!(random, session_id_for(&empty));
    }

    #[test]
    fn g3_credential_and_body_errors_are_typed() {
        let provider = GeminiProvider::new("https://x");
        let body = json!({"messages": [{"role": "user", "content": "x"}]}).to_string();
        match provider.build_request(body.as_bytes(), &credential("")) {
            Err(ProviderError::Auth(message)) => assert_eq!(
                message,
                "gemini credential has no project; resolve via loadCodeAssist first"
            ),
            other => panic!("expected Auth, got {other:?}"),
        }
        let apikey = AccountCredential::Apikey {
            api_key: "sk".into(),
        };
        assert!(matches!(
            provider.build_request(body.as_bytes(), &apikey),
            Err(ProviderError::Auth(_))
        ));
        assert!(matches!(
            provider.build_request(b"{nope", &credential("p")),
            Err(ProviderError::InvalidRequest(_))
        ));
        assert!(build_err(&json!({"messages": []})).starts_with("messages:"));
        assert!(
            build_err(&json!({"messages": [{"role": "system", "content": "x"}]}))
                .starts_with("messages[0].role:")
        );
    }

    #[test]
    fn g3_content_blocks_translate_and_turns_merge() {
        let provider = GeminiProvider::new("https://x");
        let body = json!({"messages": [
            {"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "text", "text": ""},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0K"}},
                {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBERi0x"}},
                {"type": "document", "source": {"type": "text", "media_type": "text/plain", "data": "notes"}},
            ]},
            // Thinking-only assistant turn: dropped, so the users merge.
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hmm", "signature": "anthropic-sig"},
                {"type": "redacted_thinking", "data": "xyz"},
            ]},
            {"role": "user", "content": "and this"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "ok", "signature": "s"},
                {"type": "text", "text": "done"},
            ]},
        ]});
        let (_, upstream, _) = build(&provider, &body);
        assert_eq!(
            upstream["request"]["contents"],
            json!([
                {"role": "user", "parts": [
                    {"text": "look"},
                    {"inlineData": {"mimeType": "image/png", "data": "iVBORw0K"}},
                    {"inlineData": {"mimeType": "application/pdf", "data": "JVBERi0x"}},
                    {"text": "notes"},
                    {"text": "and this"},
                ]},
                {"role": "model", "parts": [{"text": "done"}]},
            ])
        );
    }

    #[test]
    fn g3_unsupported_inputs_are_local_400s_naming_the_path() {
        let user = |block: Value| json!({"messages": [{"role": "user", "content": [block]}]});
        let message = build_err(&user(
            json!({"type": "image", "source": {"type": "url", "url": "https://secret.example/x.png"}}),
        ));
        assert!(
            message.starts_with("messages[0].content[0].source.type:"),
            "{message}"
        );
        assert!(
            !message.contains("secret.example"),
            "never echoes the payload"
        );
        let message = build_err(&user(
            json!({"type": "image", "source": {"type": "base64", "media_type": "image/gif", "data": "R0lGOD"}}),
        ));
        assert!(
            message.starts_with("messages[0].content[0].source.media_type:"),
            "{message}"
        );
        assert!(!message.contains("R0lGOD"));
        let message = build_err(&user(
            json!({"type": "document", "source": {"type": "base64", "media_type": "text/csv", "data": "YQ=="}}),
        ));
        assert!(
            message.starts_with("messages[0].content[0].source.media_type:"),
            "{message}"
        );
        let message = build_err(&user(
            json!({"type": "server_tool_use", "id": "s", "name": "web_search", "input": {}}),
        ));
        assert!(
            message.starts_with("messages[0].content[0].type:"),
            "{message}"
        );
        let message = build_err(&user(
            json!({"type": "tool_result", "tool_use_id": "call_1", "content": "x"}),
        ));
        assert!(
            message.starts_with("messages[0].content[0].tool_use_id:"),
            "{message}"
        );
        let message = build_err(&json!({
            "system": [{"type": "image"}],
            "messages": [{"role": "user", "content": "x"}],
        }));
        assert!(message.starts_with("system[0].type:"), "{message}");
    }

    /// Compatibility report parity with codex/grok: `validate_request` names
    /// exactly what the translation leaves out, refuses exactly what
    /// `build_request` refuses (same message), and needs no project.
    #[test]
    fn validate_request_reports_what_the_translation_leaves_out() {
        let provider = GeminiProvider::new("https://x");
        let faithful = json!({
            "model": "gemini",
            "temperature": 0.3,
            "stop_sequences": ["END"],
            "tools": [{"name": "ls", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "auto", "disable_parallel_tool_use": false},
            "messages": [{"role": "user", "content": "hi"}],
        });
        let report = provider.validate_request(&faithful).expect("valid");
        assert!(report.is_empty(), "nothing left out: {report:?}");

        let lossy = json!({
            "model": "gemini",
            "max_tokens": 1024,
            "thinking": {"type": "disabled"},
            "tools": [
                {"name": "ls", "input_schema": {"type": "object"}},
                {"type": "web_search_20250305", "name": "web_search"},
            ],
            "tool_choice": {"type": "any", "disable_parallel_tool_use": true},
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "t", "signature": "s"},
                    {"type": "redacted_thinking", "data": "opaque"},
                    {"type": "text", "text": "ok"},
                ]},
                {"role": "user", "content": "again"},
            ],
        });
        let report = provider.validate_request(&lossy).expect("valid");
        let expected = vec![
            "disable_parallel_tool_use",
            "max_tokens",
            "redacted_thinking",
            "server_tools",
            "thinking",
            "thinking_config",
        ];
        assert_eq!(report.omitted_fields, expected, "sorted, deduped");
        assert_eq!(report.warnings, expected, "warnings = omissions");
        // The forwarded request really lacks them.
        let (_, upstream, _) = build(&provider, &lossy);
        let request = &upstream["request"];
        assert!(request["generationConfig"].get("maxOutputTokens").is_none());
        assert_eq!(
            request["tools"][0]["functionDeclarations"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );

        // Parallel-off with every tool dropped forbids nothing upstream.
        let only_server = json!({
            "tools": [{"type": "web_search_20250305", "name": "web_search"}],
            "tool_choice": {"type": "auto", "disable_parallel_tool_use": true},
            "messages": [{"role": "user", "content": "hi"}],
        });
        assert_eq!(
            provider
                .validate_request(&only_server)
                .expect("valid")
                .omitted_fields,
            vec!["server_tools"]
        );

        // Same refusal, same message, as `build_request`.
        let refused = json!({"messages": [{"role": "user", "content": [
            {"type": "image", "source": {"type": "url", "url": "https://x/y.png"}},
        ]}]});
        let Err(ProviderError::InvalidRequest(message)) = provider.validate_request(&refused)
        else {
            panic!("expected InvalidRequest");
        };
        assert_eq!(message, build_err(&refused));
    }

    // ---- G4: stream conversion ----

    #[test]
    fn g4_thought_then_text_stream_and_aggregate() {
        let mut converter = GeminiSseConverter::with_model("gemini-3.8-flash-high".into())
            .with_client_model(Some("gemini-3.8-flash-high[1m]".into()));
        let events = run(
            &mut converter,
            &[
                chunk(
                    json!([{"text": "Considering", "thought": true}]),
                    None,
                    None,
                ),
                chunk(json!([{"text": " the ping", "thought": true}]), None, None),
                chunk(json!([{"text": "po"}]), None, None),
                chunk(
                    json!([{"text": "ng"}, {"thoughtSignature": "sig", "text": ""}]),
                    Some("STOP"),
                    Some(usage_meta(100, Some(40), 5, 7)),
                ),
            ],
        );
        assert_eq!(
            names(&events),
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        let start = &events[0].1["message"];
        assert_eq!(start["id"], "msg_resp-abc");
        assert_eq!(start["model"], "gemini-3.8-flash-high[1m]");
        assert_eq!(
            events[1].1["content_block"],
            json!({"type": "thinking", "thinking": "", "signature": ""})
        );
        assert_eq!(
            events[2].1["delta"],
            json!({"type": "thinking_delta", "thinking": "Considering"})
        );
        assert_eq!(
            events[4].1["delta"],
            json!({"type": "signature_delta", "signature": ""})
        );
        assert_eq!(events[6].1["index"], 1);
        assert_eq!(
            events[6].1["content_block"],
            json!({"type": "text", "text": ""})
        );
        assert_eq!(
            events[8].1["delta"],
            json!({"type": "text_delta", "text": "ng"})
        );
        let delta = &events[10].1;
        assert_eq!(
            delta["delta"],
            json!({"stop_reason": "end_turn", "stop_sequence": null})
        );
        // input = prompt − cached; output = candidates + thoughts.
        assert_eq!(
            delta["usage"],
            json!({
                "input_tokens": 60,
                "cache_read_input_tokens": 40,
                "cache_creation_input_tokens": 0,
                "output_tokens": 12,
            })
        );
        assert_eq!(
            converter.usage(),
            StreamUsage {
                input_tokens: 60,
                output_tokens: 12,
                cache_read_input_tokens: Some(40),
                cache_creation_input_tokens: None,
            }
        );
        assert_eq!(converter.events_seen(), 4);
        assert_eq!(
            converter.raw_usage(),
            Some(&usage_meta(100, Some(40), 5, 7))
        );
        assert_eq!(converter.error_message(), None);
        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(
            message,
            json!({
                "id": "msg_resp-abc",
                "type": "message",
                "role": "assistant",
                "model": "gemini-3.8-flash-high[1m]",
                "content": [
                    {"type": "thinking", "thinking": "Considering the ping", "signature": ""},
                    {"type": "text", "text": "pong"},
                ],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 60,
                    "cache_read_input_tokens": 40,
                    "cache_creation_input_tokens": 0,
                    "output_tokens": 12,
                },
            })
        );
    }

    #[test]
    fn g4_last_usage_metadata_wins_even_after_finish_reason() {
        let mut converter = GeminiSseConverter::with_model("m".into());
        let events = run(
            &mut converter,
            &[
                chunk(
                    json!([{"text": "a"}]),
                    None,
                    Some(usage_meta(10, None, 1, 0)),
                ),
                chunk(
                    json!([{"text": "b"}]),
                    Some("STOP"),
                    Some(usage_meta(10, None, 2, 0)),
                ),
                json!({"response": {"usageMetadata": usage_meta(12, Some(2), 3, 4)}}),
            ],
        );
        let delta = find(&events, "message_delta");
        assert_eq!(delta["usage"]["input_tokens"], 10);
        assert_eq!(delta["usage"]["cache_read_input_tokens"], 2);
        assert_eq!(delta["usage"]["output_tokens"], 7);
        assert_eq!(converter.raw_usage(), Some(&usage_meta(12, Some(2), 3, 4)));
        // Absent cachedContentTokenCount is a real zero, not "unknown".
        let mut plain = GeminiSseConverter::with_model("m".into());
        run(
            &mut plain,
            &[chunk(
                json!([{"text": "a"}]),
                Some("STOP"),
                Some(usage_meta(9, None, 1, 0)),
            )],
        );
        assert_eq!(plain.usage().cache_read_input_tokens, Some(0));
        assert_eq!(plain.usage().input_tokens, 9);
    }

    #[test]
    fn g4_finish_reason_mapping() {
        for (reason, stop) in [
            ("STOP", "end_turn"),
            ("MAX_TOKENS", "max_tokens"),
            ("SAFETY", "refusal"),
            ("RECITATION", "refusal"),
            ("BLOCKLIST", "refusal"),
            ("PROHIBITED_CONTENT", "refusal"),
            ("SPII", "refusal"),
        ] {
            let mut converter = GeminiSseConverter::with_model("m".into());
            let events = run(
                &mut converter,
                &[chunk(json!([{"text": "x"}]), Some(reason), None)],
            );
            assert_eq!(
                find(&events, "message_delta")["delta"]["stop_reason"],
                stop,
                "{reason}"
            );
            assert_eq!(events.last().map(|(n, _)| n.as_str()), Some("message_stop"));
            assert_eq!(
                converter.into_message_json().expect("aggregate")["stop_reason"],
                stop
            );
        }
        for reason in ["MALFORMED_FUNCTION_CALL", "SOMETHING_NEW"] {
            let mut converter = GeminiSseConverter::with_model("m".into());
            let mut event = chunk(json!([{"text": "x"}]), Some(reason), None);
            event["response"]["candidates"][0]["finishMessage"] = json!("bad call");
            let events = run(&mut converter, &[event]);
            assert_eq!(events.last().map(|(n, _)| n.as_str()), Some("error"));
            assert!(!names(&events).contains(&"message_delta"));
            assert_eq!(
                converter.error_message(),
                Some(format!("gemini upstream finished with {reason}: bad call").as_str())
            );
            assert!(converter.into_message_json().is_none());
        }
    }

    #[test]
    fn g4_mid_stream_error_becomes_an_anthropic_error() {
        let mut converter = GeminiSseConverter::with_model("m".into());
        let events = run(
            &mut converter,
            &[
                chunk(json!([{"text": "partial"}]), None, None),
                json!({"error": {"code": 429, "message": "Resource has been exhausted", "status": "RESOURCE_EXHAUSTED"}}),
                chunk(json!([{"text": "ignored"}]), Some("STOP"), None),
            ],
        );
        let (name, error) = events.last().expect("events");
        assert_eq!(name, "error");
        assert_eq!(
            error["error"],
            json!({"type": "api_error", "message": "gemini upstream error (RESOURCE_EXHAUSTED): Resource has been exhausted"})
        );
        assert_eq!(names(&events).iter().filter(|n| **n == "error").count(), 1);
        assert!(converter.error_message().is_some());
        assert!(converter.into_message_json().is_none());
    }

    #[test]
    fn g4_truncated_or_empty_streams_end_in_errors() {
        let mut empty = GeminiSseConverter::with_model("m".into());
        let events = run(&mut empty, &[]);
        assert_eq!(names(&events), ["error"]);
        assert_eq!(
            empty.error_message(),
            Some("gemini upstream returned no SSE events")
        );

        let mut cut = GeminiSseConverter::with_model("m".into());
        let mut out = cut.on_event(": keepalive");
        out.extend(cut.on_event("data: [DONE]"));
        out.extend(cut.on_event(&format!(
            "data: {}",
            chunk(json!([{"text": "x"}]), None, None)
        )));
        out.extend(cut.on_end());
        let events = parse_sse(&out);
        assert_eq!(events.last().map(|(n, _)| n.as_str()), Some("error"));
        assert_eq!(
            cut.error_message(),
            Some("gemini upstream stream ended before a finishReason")
        );
        assert_eq!(cut.events_seen(), 1);
    }

    #[test]
    fn g4_function_call_becomes_a_complete_tool_use_block() {
        let mut converter = GeminiSseConverter::with_model("m".into());
        let events = run(
            &mut converter,
            &[
                chunk(json!([{"text": "Checking"}]), None, None),
                chunk(
                    json!([
                        tool_call_part("call_90071", "get_weather", json!({"city": "Seoul"}), Some(LIVE_SIGNATURE)),
                        {"text": ""},
                    ]),
                    Some("STOP"),
                    Some(usage_meta(50, None, 20, 0)),
                ),
            ],
        );
        assert_eq!(
            names(&events),
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        // The client id is minted, never Google's `call_90071`.
        let id = events[4].1["content_block"]["id"]
            .as_str()
            .expect("tool id")
            .to_string();
        assert!(id.starts_with("toolu_gemini_"), "{id}");
        assert_eq!(id.len(), "toolu_gemini_".len() + 32, "uuid without dashes");
        assert_eq!(
            events[4].1["content_block"],
            json!({"type": "tool_use", "id": id, "name": "get_weather", "input": {}})
        );
        assert_eq!(
            events[5].1["delta"],
            json!({"type": "input_json_delta", "partial_json": "{\"city\":\"Seoul\"}"})
        );
        assert_eq!(
            find(&events, "message_delta")["delta"]["stop_reason"],
            "tool_use"
        );
        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(
            message["content"][1],
            json!({"type": "tool_use", "id": id, "name": "get_weather", "input": {"city": "Seoul"}})
        );
        assert_eq!(message["stop_reason"], "tool_use");
        assert!(
            message.get("safeguard_results").is_none(),
            "off unless asked"
        );

        // No upstream id → a minted toolu_gemini_ id all the same.
        let mut converter = GeminiSseConverter::with_model("m".into());
        let events = run(
            &mut converter,
            &[chunk(
                json!([{"functionCall": {"name": "ls"}}]),
                Some("STOP"),
                None,
            )],
        );
        let block = &find(&events, "content_block_start")["content_block"];
        assert!(block["id"]
            .as_str()
            .expect("id")
            .starts_with("toolu_gemini_"));
        assert_eq!(
            find(&events, "content_block_delta")["delta"]["partial_json"],
            "{}"
        );
    }

    // ---- G4b: safeguards ----

    #[test]
    fn g4b_safeguard_results_cover_every_announced_tool() {
        let mut converter =
            GeminiSseConverter::with_model("m".into()).with_safeguards_requested(true);
        let events = run(
            &mut converter,
            &[chunk(
                json!([
                    tool_call_part("call_1", "read", json!({"path": "a"}), Some("sig-1")),
                    tool_call_part("call_2", "read", json!({"path": "b"}), None),
                ]),
                Some("STOP"),
                None,
            )],
        );
        // Keyed by the client ids the stream announced.
        let ids = tool_ids(&events);
        assert_eq!(ids.len(), 2);
        let mut tool_uses = Map::new();
        for id in &ids {
            tool_uses.insert(
                id.clone(),
                json!({"type": "unavailable", "reason": "unsupported"}),
            );
        }
        let expected = json!([{
            "type": "dangerous_tool_use",
            "status": {"type": "available", "tool_uses": tool_uses},
        }]);
        assert_eq!(
            find(&events, "message_delta")["delta"]["safeguard_results"],
            expected
        );
        let message = converter.into_message_json().expect("aggregate");
        assert_eq!(message["safeguard_results"], expected);

        // Asked for, but a text-only turn: still answered (empty map), never silent.
        let mut text_only =
            GeminiSseConverter::with_model("m".into()).with_safeguards_requested(true);
        let events = run(
            &mut text_only,
            &[chunk(json!([{"text": "hi"}]), Some("STOP"), None)],
        );
        assert_eq!(
            find(&events, "message_delta")["delta"]["safeguard_results"][0]["status"]["tool_uses"],
            json!({})
        );

        // Not asked: no key on either leg.
        let mut plain = GeminiSseConverter::with_model("m".into());
        let events = run(
            &mut plain,
            &[chunk(
                json!([tool_call_part("call_1", "read", json!({}), None)]),
                Some("STOP"),
                None,
            )],
        );
        assert!(find(&events, "message_delta")["delta"]
            .get("safeguard_results")
            .is_none());
        assert!(plain
            .into_message_json()
            .expect("aggregate")
            .get("safeguard_results")
            .is_none());
    }

    // ---- G5: tools ----

    #[test]
    fn g5_tools_become_function_declarations() {
        let provider = GeminiProvider::new("https://x");
        let body = json!({
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [
                {
                    "name": "get_weather",
                    "description": "Weather for a city",
                    "input_schema": {
                        "$schema": "http://json-schema.org/draft-07/schema#",
                        "$id": "weather",
                        "type": "object",
                        "properties": {
                            "city": {"type": "string", "$comment": "free text"},
                            "units": {"anyOf": [{"type": "string", "$id": "u"}]},
                        },
                        "required": ["city"],
                        "additionalProperties": false,
                    },
                },
                {"type": "web_search_20250305", "name": "web_search", "max_uses": 5},
            ],
        });
        let (_, upstream, _) = build(&provider, &body);
        assert_eq!(
            upstream["request"]["tools"],
            json!([{"functionDeclarations": [{
                "name": "get_weather",
                "description": "Weather for a city",
                "parametersJsonSchema": {
                    "type": "object",
                    "properties": {
                        "city": {"type": "string"},
                        "units": {"anyOf": [{"type": "string"}]},
                    },
                    "required": ["city"],
                    "additionalProperties": false,
                },
            }]}])
        );
        assert_eq!(
            upstream["request"]["toolConfig"],
            json!({"functionCallingConfig": {"mode": "AUTO"}})
        );

        // Only server tools → no tools / toolConfig at all.
        let only_server = json!({
            "messages": [{"role": "user", "content": "x"}],
            "tools": [{"type": "web_search_20250305", "name": "web_search"}],
        });
        let (_, upstream, _) = build(&provider, &only_server);
        assert!(upstream["request"].get("tools").is_none());
        assert!(upstream["request"].get("toolConfig").is_none());
    }

    #[test]
    fn g5_schema_strip_keeps_property_names_and_literal_data() {
        // A property NAMED `$id` is data, not the `$id` keyword.
        let named = json!({
            "type": "object",
            "properties": {"$id": {"type": "string"}},
            "required": ["$id"],
        });
        assert_eq!(strip_schema_keys(&named), named);

        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "root",
            "$comment": "root",
            "type": "object",
            "properties": {
                "$schema": {"type": "string", "$comment": "a property named $schema"},
                "config": {
                    "$id": "nested",
                    "type": "object",
                    "const": {"$id": "literal", "$schema": "literal"},
                    "default": {"$id": "default", "$comment": "default"},
                    "enum": [{"$id": "member"}],
                    "examples": [{"$comment": "example"}],
                    "properties": {"$comment": {"$comment": "keyword", "type": "string"}},
                },
                "list": {
                    "type": "array",
                    "items": {"$id": "item", "type": "string"},
                    "prefixItems": [{"$comment": "first", "type": "string"}],
                    "contains": {"$id": "contains"},
                },
            },
            "patternProperties": {"^x-": {"$schema": "pattern", "type": "string"}},
            "additionalProperties": {"$comment": "extra", "type": "string"},
            "propertyNames": {"$comment": "names", "pattern": "^[a-z$]+$"},
            "$defs": {"$id": {"$id": "def", "type": "integer"}},
            "allOf": [{"$comment": "all"}],
            "anyOf": [true, {"$id": "any"}],
            "oneOf": [{"not": {"$comment": "not"}}],
            "if": {"$comment": "if"},
            "then": {"$comment": "then"},
            "else": {"$comment": "else"},
            "required": ["$schema", "config"],
        });
        assert_eq!(
            strip_schema_keys(&schema),
            json!({
                "type": "object",
                "properties": {
                    "$schema": {"type": "string"},
                    "config": {
                        "type": "object",
                        "const": {"$id": "literal", "$schema": "literal"},
                        "default": {"$id": "default", "$comment": "default"},
                        "enum": [{"$id": "member"}],
                        "examples": [{"$comment": "example"}],
                        "properties": {"$comment": {"type": "string"}},
                    },
                    "list": {
                        "type": "array",
                        "items": {"type": "string"},
                        "prefixItems": [{"type": "string"}],
                        "contains": {},
                    },
                },
                "patternProperties": {"^x-": {"type": "string"}},
                "additionalProperties": {"type": "string"},
                "propertyNames": {"pattern": "^[a-z$]+$"},
                "$defs": {"$id": {"type": "integer"}},
                "allOf": [{}],
                "anyOf": [true, {}],
                "oneOf": [{"not": {}}],
                "if": {},
                "then": {},
                "else": {},
                "required": ["$schema", "config"],
            })
        );
    }

    #[test]
    fn g5_tool_choice_mapping() {
        let provider = GeminiProvider::new("https://x");
        let with_choice = |choice: Value| {
            json!({
                "messages": [{"role": "user", "content": "x"}],
                "tools": [{"name": "ls", "input_schema": {"type": "object"}}],
                "tool_choice": choice,
            })
        };
        for (choice, expected) in [
            (json!({"type": "auto"}), json!({"mode": "AUTO"})),
            (json!({"type": "any"}), json!({"mode": "ANY"})),
            (json!({"type": "none"}), json!({"mode": "NONE"})),
            (
                json!({"type": "tool", "name": "ls"}),
                json!({"mode": "ANY", "allowedFunctionNames": ["ls"]}),
            ),
        ] {
            let (_, upstream, _) = build(&provider, &with_choice(choice.clone()));
            assert_eq!(
                upstream["request"]["toolConfig"]["functionCallingConfig"], expected,
                "{choice}"
            );
        }
        let message = build_err(&with_choice(json!({"type": "sometimes"})));
        assert!(message.starts_with("tool_choice.type:"), "{message}");
    }

    // ---- G7 / G7b: thought signatures ----

    #[test]
    fn g7_signature_round_trip_through_the_shared_cache() {
        let provider = GeminiProvider::new("https://x");
        let turn1 = json!({
            "model": "gemini-3.8-flash-low",
            "messages": [{"role": "user", "content": "Weather in Seoul?"}],
            "tools": [{"name": "get_weather", "input_schema": {"type": "object"}}],
        });
        let session = session_id_for(&turn1);
        let mut converter = provider.converter(provider.client_model_stamp(&turn1), session);
        let events = run(
            &mut converter,
            &[chunk(
                json!([
                    tool_call_part("call_90071", "get_weather", json!({"city": "Seoul"}), Some(LIVE_SIGNATURE)),
                    {"text": ""},
                ]),
                Some("STOP"),
                None,
            )],
        );
        assert_eq!(provider.signatures.len(), 1);
        let client_id = tool_ids(&events).remove(0);

        // The client replays the minted id; upstream gets Google's id back.
        let turn2 = json!({
            "model": "gemini-3.8-flash-low",
            "messages": [
                {"role": "user", "content": "Weather in Seoul?"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": client_id, "name": "get_weather", "input": {"city": "Seoul"}},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": client_id, "content": "Sunny, 21C"},
                ]},
            ],
            "tools": [{"name": "get_weather", "input_schema": {"type": "object"}}],
        });
        let (_, upstream, _) = build(&provider, &turn2);
        let contents = &upstream["request"]["contents"];
        assert_eq!(
            contents[1],
            json!({"role": "model", "parts": [{
                "thoughtSignature": LIVE_SIGNATURE,
                "functionCall": {"name": "get_weather", "args": {"city": "Seoul"}, "id": "call_90071"},
            }]})
        );
        assert_eq!(
            contents[2],
            json!({"role": "user", "parts": [{"functionResponse": {
                "name": "get_weather",
                "id": "call_90071",
                "response": {"result": "Sunny, 21C"},
            }}]})
        );
        assert_eq!(
            upstream["request"]["sessionId"],
            json!(session_id_for(&turn1))
        );

        // Same client id in ANOTHER session: a miss, so the dummy and the id
        // as the client sent it.
        let other = json!({"messages": [
            {"role": "user", "content": "Different conversation"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": client_id, "name": "get_weather", "input": {}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": client_id, "content": "x"},
            ]},
        ]});
        let (_, upstream, _) = build(&provider, &other);
        let part = &upstream["request"]["contents"][1]["parts"][0];
        assert_eq!(part["thoughtSignature"], SKIP_THOUGHT_SIGNATURE);
        assert_eq!(part["functionCall"]["id"], json!(client_id));
    }

    #[test]
    fn g7_conversations_sharing_a_session_replay_their_own_signatures() {
        // Codex / Claude Code sessions in one repo open with the same
        // instructions block, so their first user text — hence their
        // session id — match, and Google repeats its call ids.
        let provider = GeminiProvider::new("https://x");
        let opening = "<environment>cwd=/repo</environment>";
        let tools = json!([{"name": "get_weather", "input_schema": {"type": "object"}}]);
        let session = session_id_for(&json!({"messages": [{"role": "user", "content": opening}]}));
        let stream_call = |signature: &str| -> String {
            let mut converter = provider.converter("gemini".into(), session.clone());
            let events = run(
                &mut converter,
                &[chunk(
                    json!([tool_call_part(
                        "call_7",
                        "get_weather",
                        json!({"city": "Seoul"}),
                        Some(signature)
                    )]),
                    Some("STOP"),
                    None,
                )],
            );
            tool_ids(&events).remove(0)
        };
        let first = stream_call("sig-conversation-1");
        let second = stream_call("sig-conversation-2");

        for (client_id, signature) in [
            (&first, "sig-conversation-1"),
            (&second, "sig-conversation-2"),
        ] {
            let next_turn = json!({
                "messages": [
                    {"role": "user", "content": opening},
                    {"role": "assistant", "content": [
                        {"type": "tool_use", "id": client_id, "name": "get_weather", "input": {"city": "Seoul"}},
                    ]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": client_id, "content": "Sunny"},
                    ]},
                ],
                "tools": tools,
            });
            assert_eq!(session_id_for(&next_turn), session, "one shared session id");
            let (_, upstream, _) = build(&provider, &next_turn);
            let contents = &upstream["request"]["contents"];
            assert_eq!(
                contents[1]["parts"][0]["thoughtSignature"], signature,
                "the conversation that got {client_id} replays its OWN signature: {contents}"
            );
            assert_eq!(contents[1]["parts"][0]["functionCall"]["id"], "call_7");
            assert_eq!(contents[2]["parts"][0]["functionResponse"]["id"], "call_7");
        }
        assert_ne!(first, second, "each streamed call gets its own client id");
        assert!(first.starts_with("toolu_gemini_"), "{first}");
    }

    #[test]
    fn g7_cache_miss_replays_the_client_id_with_the_dummy_signature() {
        let provider = GeminiProvider::new("https://x");
        let opening = "Weather in Seoul?";
        let session = session_id_for(&json!({"messages": [{"role": "user", "content": opening}]}));
        // The cache holds this session's streamed call — under the client id
        // the converter minted, not under Google's `call_7`.
        let mut converter = provider.converter("gemini".into(), session);
        run(
            &mut converter,
            &[chunk(
                json!([tool_call_part(
                    "call_7",
                    "get_weather",
                    json!({}),
                    Some(LIVE_SIGNATURE)
                )]),
                Some("STOP"),
                None,
            )],
        );
        assert_eq!(provider.signatures.len(), 1);

        // Ids the converter never handed out (a pre-mint history, or one
        // minted before a daemon restart): a miss — sent as-is, with the dummy.
        for client_id in ["call_7", "toolu_gemini_0123456789abcdef0123456789abcdef"] {
            let body = json!({"messages": [
                {"role": "user", "content": opening},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": client_id, "name": "get_weather", "input": {}},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": client_id, "content": "Sunny"},
                ]},
            ]});
            let (_, upstream, _) = build(&provider, &body);
            let contents = &upstream["request"]["contents"];
            assert_eq!(
                contents[1]["parts"][0],
                json!({
                    "thoughtSignature": SKIP_THOUGHT_SIGNATURE,
                    "functionCall": {"name": "get_weather", "args": {}, "id": client_id},
                }),
                "{client_id}"
            );
            assert_eq!(contents[2]["parts"][0]["functionResponse"]["id"], client_id);
        }
    }

    #[test]
    fn g7_unsigned_parallel_calls_replay_their_upstream_ids() {
        let provider = GeminiProvider::new("https://x");
        let opening = "read both files";
        let session = session_id_for(&json!({"messages": [{"role": "user", "content": opening}]}));
        let mut converter = provider.converter("gemini".into(), session);
        // Gemini signs only the first call of a parallel step.
        let events = run(
            &mut converter,
            &[chunk(
                json!([
                    tool_call_part("call_1", "read", json!({"path": "a"}), Some("sig-1")),
                    tool_call_part("call_2", "read", json!({"path": "b"}), None),
                ]),
                Some("STOP"),
                None,
            )],
        );
        let ids = tool_ids(&events);
        assert_eq!(ids.len(), 2);
        let body = json!({"messages": [
            {"role": "user", "content": opening},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": ids[0], "name": "read", "input": {"path": "a"}},
                {"type": "tool_use", "id": ids[1], "name": "read", "input": {"path": "b"}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": ids[0], "content": "A"},
                {"type": "tool_result", "tool_use_id": ids[1], "content": "B"},
            ]},
        ]});
        let (_, upstream, _) = build(&provider, &body);
        let contents = &upstream["request"]["contents"];
        assert_eq!(
            contents[1]["parts"],
            json!([
                {"thoughtSignature": "sig-1",
                 "functionCall": {"name": "read", "args": {"path": "a"}, "id": "call_1"}},
                {"functionCall": {"name": "read", "args": {"path": "b"}, "id": "call_2"}},
            ])
        );
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["id"], "call_1");
        assert_eq!(contents[2]["parts"][1]["functionResponse"]["id"], "call_2");
    }

    #[test]
    fn g7_call_and_response_replay_one_entry_while_another_request_evicts_it() {
        // A one-entry cache: the next insert evicts this conversation's call.
        let cache = SignatureCache::with_capacity(1);
        let session = "-42";
        cache.insert(
            session,
            "toolu_gemini_own",
            CachedCall {
                signature: Some("sig-own".into()),
                upstream_id: Some("call_7".into()),
            },
        );
        // Another request records its call right after this request's first
        // cache read, evicting the entry this request is replaying.
        arm_after_next_read(|cache| {
            cache.insert(
                "-42",
                "toolu_gemini_other",
                CachedCall {
                    signature: Some("sig-other".into()),
                    upstream_id: Some("call_7".into()),
                },
            );
        });
        let body = json!({"messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_gemini_own", "name": "run", "input": {}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_gemini_own", "content": "ok"},
            ]},
        ]});
        let (upstream, _, _) =
            translate_request(&body, &GeminiShape::default(), "proj", session, &cache)
                .expect("translate");
        assert_eq!(
            cache.get(session, "toolu_gemini_own"),
            None,
            "the racing insert evicted the entry mid-replay"
        );
        let contents = &upstream["request"]["contents"];
        let call = &contents[1]["parts"][0];
        let response = &contents[2]["parts"][0]["functionResponse"];
        assert_eq!(
            response["id"], call["functionCall"]["id"],
            "the call and its response must carry the same id: {contents}"
        );
        assert_eq!(call["functionCall"]["id"], "call_7");
        assert_eq!(call["thoughtSignature"], "sig-own");
        assert_eq!(response["name"], "run");
    }

    #[test]
    fn g7_a_snapshot_applies_unchanged_after_the_cache_moves_on() {
        let cache = SignatureCache::with_capacity(2);
        let session = "-7";
        let cached = |signature: Option<&str>, upstream_id: &str| CachedCall {
            signature: signature.map(str::to_string),
            upstream_id: Some(upstream_id.to_string()),
        };
        cache.insert(session, "toolu_gemini_a", cached(Some("sig-a"), "call_1"));
        cache.insert(session, "toolu_gemini_b", cached(None, "call_2"));
        let body = json!({"messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_gemini_a", "name": "read", "input": {}},
                {"type": "tool_use", "id": "toolu_gemini_b", "name": "read", "input": {}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_gemini_a", "content": "A"},
                {"type": "tool_result", "tool_use_id": "toolu_gemini_b", "content": "B"},
            ]},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_gemini_miss", "name": "run", "input": {}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_gemini_miss", "content": "C"},
            ]},
        ]});
        let mut report = CompatibilityReport::default();
        let mut turns = translate_messages(body.get("messages"), &mut report).expect("turns");

        let snapshot = snapshot_replayed_calls(&turns, session, &cache);
        assert_eq!(
            snapshot,
            HashMap::from([
                (
                    "toolu_gemini_a".to_string(),
                    cached(Some("sig-a"), "call_1")
                ),
                ("toolu_gemini_b".to_string(), cached(None, "call_2")),
            ]),
            "one entry per cached id; the miss is absent"
        );

        // The cache moves on before the snapshot is applied: `a` is
        // overwritten, `b` is evicted.
        cache.insert(
            session,
            "toolu_gemini_a",
            cached(Some("sig-new"), "call_99"),
        );
        cache.insert(session, "toolu_gemini_c", cached(Some("sig-c"), "call_3"));
        assert_eq!(cache.get(session, "toolu_gemini_b"), None);

        apply_replayed_calls(&mut turns, &snapshot);
        assert_eq!(
            Value::Array(turns[1].parts.clone()),
            json!([
                {"thoughtSignature": "sig-a",
                 "functionCall": {"name": "read", "args": {}, "id": "call_1"}},
                {"functionCall": {"name": "read", "args": {}, "id": "call_2"}},
            ])
        );
        assert_eq!(
            Value::Array(turns[2].parts.clone()),
            json!([
                {"functionResponse": {"name": "read", "id": "call_1", "response": {"result": "A"}}},
                {"functionResponse": {"name": "read", "id": "call_2", "response": {"result": "B"}}},
            ])
        );
        // A miss: the client id as sent, and the dummy on the turn's first call.
        assert_eq!(
            turns[3].parts[0],
            json!({"thoughtSignature": SKIP_THOUGHT_SIGNATURE,
                   "functionCall": {"name": "run", "args": {}, "id": "toolu_gemini_miss"}})
        );
        assert_eq!(
            turns[4].parts[0]["functionResponse"]["id"],
            "toolu_gemini_miss"
        );
    }

    #[test]
    fn g7_tool_results_carry_errors_and_images() {
        let provider = GeminiProvider::new("https://x");
        let body = json!({"messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "call_1", "name": "shot", "input": {}},
                {"type": "tool_use", "id": "call_2", "name": "run", "input": {}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "call_1", "content": [
                    {"type": "text", "text": "captured"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "/9j/"}},
                ]},
                {"type": "tool_result", "tool_use_id": "call_2", "is_error": true, "content": [
                    {"type": "text", "text": "exit 1"},
                    {"type": "text", "text": "boom"},
                ]},
            ]},
        ]});
        let (_, upstream, _) = build(&provider, &body);
        assert_eq!(
            upstream["request"]["contents"][2]["parts"],
            json!([
                {"functionResponse": {
                    "name": "shot",
                    "id": "call_1",
                    "response": {"result": "captured"},
                    "parts": [{"inlineData": {"mimeType": "image/jpeg", "data": "/9j/"}}],
                }},
                {"functionResponse": {
                    "name": "run",
                    "id": "call_2",
                    "response": {"error": "exit 1\nboom"},
                }},
            ])
        );
    }

    #[test]
    fn g7b_dummy_signature_only_on_the_first_uncached_call_of_a_model_turn() {
        let provider = GeminiProvider::new("https://x");
        let body = json!({"messages": [
            {"role": "user", "content": "parallel please"},
            {"role": "assistant", "content": [
                {"type": "text", "text": "running both"},
                {"type": "tool_use", "id": "call_1", "name": "a", "input": {}},
                {"type": "tool_use", "id": "call_2", "name": "b", "input": {}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "call_1", "content": "1"},
                {"type": "tool_result", "tool_use_id": "call_2", "content": "2"},
            ]},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "call_3", "name": "a", "input": {}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "call_3", "content": "3"},
            ]},
        ]});
        let (_, upstream, _) = build(&provider, &body);
        let contents = &upstream["request"]["contents"];
        let parts = &contents[1]["parts"];
        assert!(
            parts[0].get("thoughtSignature").is_none(),
            "text part untouched"
        );
        assert_eq!(parts[1]["thoughtSignature"], SKIP_THOUGHT_SIGNATURE);
        assert!(
            parts[2].get("thoughtSignature").is_none(),
            "later parallel call carries no signature"
        );
        // Every model turn's first call gets its own dummy.
        assert_eq!(
            contents[3]["parts"][0]["thoughtSignature"],
            SKIP_THOUGHT_SIGNATURE
        );
        // Function responses never carry signatures.
        assert!(contents[2]["parts"][0].get("thoughtSignature").is_none());

        // A cached later call keeps its real signature; the uncached first
        // call still gets the dummy.
        provider
            .signatures
            .insert(&session_id_for(&body), "call_2", signed("real-2"));
        let (_, upstream, _) = build(&provider, &body);
        let parts = &upstream["request"]["contents"][1]["parts"];
        assert_eq!(parts[1]["thoughtSignature"], SKIP_THOUGHT_SIGNATURE);
        assert_eq!(parts[2]["thoughtSignature"], "real-2");
    }

    #[test]
    fn g7b_signature_cache_is_bounded_and_scoped_by_session() {
        assert_eq!(SignatureCache::default().capacity, SIGNATURE_CACHE_CAPACITY);
        let cache = SignatureCache::with_capacity(2);
        assert!(cache.is_empty());
        cache.insert("s", "a", signed("sig-a"));
        cache.insert("s", "b", signed("sig-b"));
        cache.insert("s", "c", signed("sig-c"));
        assert_eq!(cache.get("s", "a"), None, "oldest evicted");
        // Re-recording refreshes the entry's position.
        cache.insert("s", "b", signed("sig-b2"));
        cache.insert("s", "d", signed("sig-d"));
        assert_eq!(cache.get("s", "c"), None);
        assert_eq!(cache.get("s", "b"), Some(signed("sig-b2")));
        assert_eq!(cache.get("s", "d"), Some(signed("sig-d")));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get("other", "d"), None, "scoped by session");

        // Empty strings are absent; a call with nothing to replay is skipped,
        // an unsigned call still keeps its upstream id.
        let cache = SignatureCache::with_capacity(4);
        cache.insert("s", "e", signed(""));
        cache.insert("s", "", signed("sig"));
        cache.insert(
            "s",
            "f",
            CachedCall {
                signature: None,
                upstream_id: Some(String::new()),
            },
        );
        assert!(cache.is_empty(), "nothing to replay → nothing held");
        cache.insert(
            "s",
            "g",
            CachedCall {
                signature: Some(String::new()),
                upstream_id: Some("call_2".into()),
            },
        );
        assert_eq!(
            cache.get("s", "g"),
            Some(CachedCall {
                signature: None,
                upstream_id: Some("call_2".into()),
            })
        );
    }

    /// A cache entry holding only a signature.
    fn signed(signature: &str) -> CachedCall {
        CachedCall {
            signature: Some(signature.to_string()),
            upstream_id: None,
        }
    }
}
