//! Design stubs (spec §Non-goals): `gemini` and `local` exist so the
//! [`Provider`] trait is compile-checked against more than one shape, but
//! every conversion hook returns [`ProviderError::NotImplemented`]. This IS
//! their complete behavior — not a todo. (`openai-codex` graduated from stub
//! to a working provider in [`super::codex`].)

use super::{
    AnthropicRequest, AnthropicResponse, Provider, ProviderError, ProviderRequest,
    ProviderResponse, UnifiedRequest, UnifiedResponse,
};
use crate::config::AccountCredential;

macro_rules! stub_provider {
    ($(#[$doc:meta])* $ty:ident, $name:literal, $endpoint:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Default)]
        pub struct $ty;

        impl Provider for $ty {
            fn name(&self) -> &'static str {
                $name
            }

            fn endpoint(&self) -> &str {
                $endpoint
            }

            async fn auth(
                &self,
                _req: &mut ProviderRequest,
                _account: &AccountCredential,
            ) -> Result<(), ProviderError> {
                Err(ProviderError::NotImplemented { provider: $name })
            }

            fn request_out(
                &self,
                _anthropic_req: AnthropicRequest,
            ) -> Result<UnifiedRequest, ProviderError> {
                Err(ProviderError::NotImplemented { provider: $name })
            }

            fn request_in(
                &self,
                _unified: UnifiedRequest,
            ) -> Result<ProviderRequest, ProviderError> {
                Err(ProviderError::NotImplemented { provider: $name })
            }

            fn response_in(
                &self,
                _provider_resp: ProviderResponse,
            ) -> Result<UnifiedResponse, ProviderError> {
                Err(ProviderError::NotImplemented { provider: $name })
            }

            fn response_out(
                &self,
                _unified: UnifiedResponse,
            ) -> Result<AnthropicResponse, ProviderError> {
                Err(ProviderError::NotImplemented { provider: $name })
            }
        }
    };
}

stub_provider!(
    /// Google Gemini behind an Anthropic-shaped front. Draft.
    Gemini,
    "gemini",
    "https://generativelanguage.googleapis.com"
);

stub_provider!(
    /// Local model server (e.g. an OpenAI-compatible llama.cpp). Draft.
    Local,
    "local",
    "http://localhost:8080"
);
