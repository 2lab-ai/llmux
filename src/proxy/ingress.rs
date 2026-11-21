//! Client ingress: which client protocol a request speaks, and what that
//! protocol needs from the shared forward path.
//!
//! llmux serves three client frontends on one scheduler/lease/retry loop:
//! Claude Code on the Anthropic Messages ingress (`/v1/messages`), Codex on
//! the OpenAI Responses ingress (`/v1/responses`) and the Antigravity CLI
//! (`agy`) on the Google Gemini API ingress (`/v1beta/models/…`). The Messages ingress is the
//! proxy's own wire format, so its body flows through untouched. Every other
//! ingress carries a [`NativeInput`]: the client's body as it arrived, which
//! is served natively on its [`native_group`] and translated to a Messages
//! body ([`messages_request`]) for every other backend, while
//! [`decode`]'s metadata summary feeds the shared routing/activity paths.
//! [`adapt`] converts the shared loop's response back to the client protocol.
//!
//! A new client protocol is one more [`Endpoint`] case in [`origin`], one more
//! [`NativeInput`] variant, and one more arm in each function here; the
//! compiler flags those arms and the native-backend builder in `forward.rs`.
//! It does NOT flag these, which a new protocol must check by hand: the
//! translated-backend arms in `forward.rs` (they chain `if served == …` and
//! fall through to OpenRouter, with no Codex arm, because Codex is native on
//! Responses), the Responses-only checks behind `responses_input()`, the relay's
//! native-stream handling (keyed on the `x-llmux-native-responses` header in
//! `sse.rs`), the `client_auth` key extraction (Bearer or `x-api-key` only),
//! the count-tokens path test, and `translate_converter` reading `ctx.body`.

use axum::response::Response;
use bytes::Bytes;
use http::{Method, StatusCode};
use serde_json::Value;

use super::{genai, responses};
use crate::routing::BackendGroup;
use crate::tui::Endpoint;

pub(crate) use super::responses::Capture;

/// The client protocol a request path speaks. Recorded on every activity row
/// as the endpoint origin, independently of the model and the backend that
/// serves it.
pub(crate) fn origin(path: &str) -> Endpoint {
    if responses::is_responses_path(path) {
        Endpoint::OpenAi
    } else if genai::is_genai_path(path) {
        Endpoint::Google
    } else {
        Endpoint::Anthropic
    }
}

/// Does the client-auth gate read this path's client key the OpenAI way
/// (`Authorization: Bearer`, or a matching `x-api-key`)? True for the
/// Responses ingress, the OpenAI model list and the Gemini API ingress (agy
/// sends `Authorization: Bearer <AGY_LLM_GATEWAY_API_KEY>`; `x-goog-api-key`
/// is read there too); every other path presents `x-api-key` only.
pub(crate) fn accepts_bearer_client_key(path: &str) -> bool {
    responses::is_responses_path(path) || path == "/v1/models" || genai::is_genai_path(path)
}

/// A non-Messages client body, kept exactly as the client sent it (after
/// validation) for its native backend and the return conversion.
#[derive(Debug, Clone)]
pub(crate) enum NativeInput {
    /// An OpenAI Responses request (a JSON object).
    Responses(Value),
    /// A Gemini API request: the model and method from the URL path, the
    /// `GenerateContentRequest` body.
    GenAi {
        /// The model the request is routed and served as
        /// ([`genai::served_model`] of the path's model).
        model: String,
        method: genai::Method,
        body: Value,
    },
}

impl NativeInput {
    /// The Responses request body, when this is the Responses ingress.
    pub(crate) fn responses(&self) -> Option<&Value> {
        match self {
            NativeInput::Responses(input) => Some(input),
            NativeInput::GenAi { .. } => None,
        }
    }
}

/// Decode the buffered ingress body for `origin`: the Messages-shaped body the
/// shared routing/metering/activity paths read, plus the native input when the
/// client did not speak Messages. `Err` is the `(status, detail)` of a request
/// refused before anything else runs (wrong method, not a JSON object).
///
/// - Messages: the body as-is, no native input.
/// - Responses: POST only; the body must be a JSON object; the metadata body
///   is [`responses::metadata_request`]'s summary (model, effort, service tier,
///   instructions, last user text) — never forwarded upstream.
/// - Gemini API: the model and method come from `path`
///   (`/v1beta/models/{model}:{method}`; anything else is a 404); POST only;
///   `streamGenerateContent` / `generateContent` only (`countTokens` is
///   answered before the forward path); the body must be a
///   `GenerateContentRequest`; the metadata body is
///   [`genai::metadata_request`]'s summary.
pub(crate) fn decode(
    origin: Endpoint,
    method: &Method,
    path: &str,
    body: &Bytes,
) -> Result<(Bytes, Option<NativeInput>), (StatusCode, String)> {
    match origin {
        Endpoint::Anthropic => Ok((body.clone(), None)),
        Endpoint::OpenAi => {
            if method != Method::POST {
                return Err((
                    StatusCode::METHOD_NOT_ALLOWED,
                    "Responses requires POST".to_string(),
                ));
            }
            let input =
                responses::validate(body).map_err(|detail| (StatusCode::BAD_REQUEST, detail))?;
            Ok((
                responses::metadata_request(&input),
                Some(NativeInput::Responses(input)),
            ))
        }
        Endpoint::Google => {
            let (model, genai_method) = genai::parse_target(path)?;
            if method != Method::POST {
                return Err((
                    StatusCode::METHOD_NOT_ALLOWED,
                    format!("models/{model} requires POST"),
                ));
            }
            if genai_method == genai::Method::CountTokens {
                return Err((
                    StatusCode::NOT_FOUND,
                    format!("models/{model}:countTokens is answered by its own route"),
                ));
            }
            let body = genai::validate(body).map_err(|detail| (StatusCode::BAD_REQUEST, detail))?;
            let model = genai::served_model(&model);
            Ok((
                genai::metadata_request(&model, &body),
                Some(NativeInput::GenAi {
                    model,
                    method: genai_method,
                    body,
                }),
            ))
        }
    }
}

/// A local `invalid_request_error` in the error envelope `origin`'s clients
/// read: the OpenAI `{"error":{…,"code":null,"param":null}}` shape on the
/// Responses ingress, the Google `{"error":{"code","message","status"}}`
/// shape on the Gemini API ingress, the Anthropic `{"type":"error",…}` shape
/// on Messages.
///
/// The client-auth gate's key-registry refusals (denied / suspended / revoked)
/// deliberately do NOT come through here: they keep the Anthropic
/// `authentication_error` shape on every path, as they always have.
pub(crate) fn error(origin: Endpoint, status: StatusCode, detail: &str) -> Response {
    match origin {
        Endpoint::OpenAi => responses::error(status, detail),
        Endpoint::Google => genai::error(status, detail),
        Endpoint::Anthropic => {
            super::forward::error_response(status, "invalid_request_error", detail)
        }
    }
}

/// The backend group that serves this input natively (its own wire format,
/// no Messages round trip): Codex for Responses, gemini for the Gemini API.
/// Every other group gets [`messages_request`].
pub(crate) fn native_group(native: &NativeInput) -> Option<BackendGroup> {
    match native {
        NativeInput::Responses(_) => Some(BackendGroup::Codex),
        NativeInput::GenAi { .. } => Some(BackendGroup::Gemini),
    }
}

/// The native input translated to an Anthropic Messages body for `served`,
/// a backend that does not serve it natively (claude via the Agent SDK, and
/// grok, gemini, openrouter for Responses; claude, codex, grok, openrouter
/// for the Gemini API, which also drops what `served` refuses but its
/// clients cannot set). `Err` is a client-facing detail for a local 400.
pub(crate) fn messages_request(
    native: &NativeInput,
    served: BackendGroup,
) -> Result<Value, String> {
    match native {
        NativeInput::Responses(input) => responses::messages_request(input),
        NativeInput::GenAi { model, body, .. } => {
            genai::request::messages_request(model, body, served)
        }
    }
}

/// Mark a successful response from the native backend so [`adapt`] relays its
/// native stream instead of converting a Messages one: on Responses the
/// subscription gateway streams Responses SSE even when its content-type is
/// absent. The gemini group's Cloud Code stream goes through the relay's
/// [`genai::response::NativeStream`] transform, whose frames carry no
/// Messages `type` — [`adapt`] tells them apart per event, so only the
/// content-type is set.
pub(crate) fn mark_native_response(native: &NativeInput, headers: &mut http::HeaderMap) {
    match native {
        NativeInput::GenAi { .. } => {
            headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("text/event-stream"),
            );
        }
        NativeInput::Responses(_) => {
            headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("text/event-stream"),
            );
            headers.insert(
                "x-llmux-native-responses",
                http::HeaderValue::from_static("1"),
            );
        }
    }
}

/// Convert the shared loop's response back to the client protocol of
/// `native`, capturing both sides at that boundary when `capture` is set.
/// Messages requests have no native input and are relayed as they are.
pub(crate) async fn adapt(
    response: Response,
    native: NativeInput,
    capture: Option<Capture>,
) -> Response {
    match native {
        NativeInput::Responses(input) => responses::adapt(response, input, capture).await,
        NativeInput::GenAi { model, method, .. } => {
            genai::response::adapt(response, model, method, capture).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_follows_the_responses_paths() {
        for path in ["/v1/responses", "/responses", "/v1/responses?x=1"] {
            assert_eq!(origin(path), Endpoint::OpenAi, "{path}");
        }
        for path in [
            "/v1beta/models",
            "/v1beta/models/gpt-6.2-sol:streamGenerateContent?alt=sse",
            "/v1beta:fetchDefaultConfig",
        ] {
            assert_eq!(origin(path), Endpoint::Google, "{path}");
        }
        for path in [
            "/v1/messages",
            "/v1/messages/count_tokens",
            "/v1/models",
            "/",
        ] {
            assert_eq!(origin(path), Endpoint::Anthropic, "{path}");
        }
    }

    #[test]
    fn bearer_client_keys_are_read_on_the_openai_paths_only() {
        assert!(accepts_bearer_client_key("/v1/responses"));
        assert!(accepts_bearer_client_key("/responses"));
        assert!(accepts_bearer_client_key("/v1/models"));
        assert!(accepts_bearer_client_key("/v1beta/models"));
        assert!(accepts_bearer_client_key(
            "/v1beta/models/x:streamGenerateContent"
        ));
        assert!(!accepts_bearer_client_key("/v1/messages"));
        assert!(!accepts_bearer_client_key("/models"));
    }

    #[test]
    fn decode_passes_messages_through_and_summarizes_responses() {
        let messages = Bytes::from_static(br#"{"model":"opus","messages":[]}"#);
        let (body, native) = decode(
            Endpoint::Anthropic,
            &Method::POST,
            "/v1/messages",
            &messages,
        )
        .unwrap();
        assert_eq!(body, messages);
        assert!(native.is_none());

        let input = Bytes::from_static(br#"{"model":"gpt-x","input":"hi"}"#);
        let (body, native) =
            decode(Endpoint::OpenAi, &Method::POST, "/v1/responses", &input).unwrap();
        let native = native.expect("Responses carries its native input");
        assert_eq!(native.responses().unwrap()["input"], "hi");
        assert_eq!(native_group(&native), Some(BackendGroup::Codex));
        let summary: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(summary["model"], "gpt-x");
        assert_eq!(summary["_llmux_native_responses"], true);

        assert_eq!(
            decode(Endpoint::OpenAi, &Method::GET, "/v1/responses", &input).unwrap_err(),
            (
                StatusCode::METHOD_NOT_ALLOWED,
                "Responses requires POST".to_string()
            )
        );
        assert_eq!(
            decode(
                Endpoint::OpenAi,
                &Method::POST,
                "/v1/responses",
                &Bytes::from_static(b"[]")
            )
            .unwrap_err(),
            (
                StatusCode::BAD_REQUEST,
                "body must be a JSON object".to_string()
            )
        );
    }

    #[test]
    fn decode_reads_the_gemini_model_and_method_from_the_path() {
        let body = Bytes::from_static(
            br#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"thinkingConfig":{"thinkingLevel":"LOW"}}}"#,
        );
        let path = "/v1beta/models/claude-opus-4-8:streamGenerateContent?alt=sse";
        let (summary, native) = decode(Endpoint::Google, &Method::POST, path, &body).unwrap();
        let native = native.expect("the Gemini API carries its native input");
        let NativeInput::GenAi {
            model,
            method,
            body: input,
        } = &native
        else {
            panic!("{native:?}");
        };
        assert_eq!(model, "claude-opus-4-8[1m]", "served as its catalog row");
        assert_eq!(*method, genai::Method::StreamGenerateContent);
        assert_eq!(input["contents"][0]["parts"][0]["text"], "hi");
        assert!(native.responses().is_none());
        assert_eq!(native_group(&native), Some(BackendGroup::Gemini));
        let summary: Value = serde_json::from_slice(&summary).unwrap();
        assert_eq!(summary["model"], "claude-opus-4-8[1m]");
        assert_eq!(summary["output_config"]["effort"], "low");
        let messages = messages_request(&native, BackendGroup::Claude).unwrap();
        assert_eq!(messages["model"], "claude-opus-4-8[1m]");
        assert_eq!(messages["messages"][0]["content"][0]["text"], "hi");

        assert_eq!(
            decode(Endpoint::Google, &Method::GET, path, &body)
                .unwrap_err()
                .0,
            StatusCode::METHOD_NOT_ALLOWED
        );
        for bad in ["/v1beta:fetchDefaultConfig", "/v1beta/models/x:countTokens"] {
            assert_eq!(
                decode(Endpoint::Google, &Method::POST, bad, &body)
                    .unwrap_err()
                    .0,
                StatusCode::NOT_FOUND,
                "{bad}"
            );
        }
        assert_eq!(
            decode(
                Endpoint::Google,
                &Method::POST,
                path,
                &Bytes::from_static(b"{}")
            )
            .unwrap_err(),
            (
                StatusCode::BAD_REQUEST,
                "contents must be an array".to_string()
            )
        );
    }

    #[tokio::test]
    async fn error_picks_the_envelope_of_the_client_protocol() {
        let body = |response: Response| async move {
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
                .await
                .unwrap();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        };
        let openai = error(Endpoint::OpenAi, StatusCode::BAD_REQUEST, "nope");
        assert_eq!(openai.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body(openai).await,
            serde_json::json!({"error":{"type":"invalid_request_error","message":"nope","code":null,"param":null}})
        );
        let google = error(Endpoint::Google, StatusCode::BAD_REQUEST, "nope");
        assert_eq!(google.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body(google).await,
            serde_json::json!({"error":{"code":400,"message":"nope","status":"INVALID_ARGUMENT"}})
        );
        let anthropic = error(Endpoint::Anthropic, StatusCode::BAD_REQUEST, "nope");
        assert_eq!(anthropic.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body(anthropic).await,
            serde_json::json!({"type":"error","error":{"type":"invalid_request_error","message":"nope"}})
        );
    }
}
