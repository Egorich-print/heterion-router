//! Web executor (`executor: "web"` providers).
//!
//! Some subscriptions can only be used through a web application: ChatGPT,
//! Claude, Gemini, Grok, Kimi, DeepSeek and Z.ai hand out no API key, but
//! the operator's session works. This backend drives that session through the
//! Camoufox driver process (ADR-025) and returns the scraped answer as an
//! ordinary chat completion.
//!
//! The rules that make this honest:
//!
//! * usage stays zero and the cost header stays off — a web chat does not
//!   report token counts, and a made-up zero would be a fake number;
//! * streaming is one delta followed by `stop` — the page delivers a finished
//!   text, and pretending to stream it would only add latency;
//! * the driver's failures keep their own identity, because they need
//!   different repairs: a dead session is fixed by signing in, a broken
//!   selector by code, a human step by the operator.

use std::collections::BTreeSet;

use async_trait::async_trait;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{Attribution, ChatBackend, ChunkStream};
use crate::ids;
use crate::web_driver::{DriverError, WebDriverClient};

/// A provider whose answers come from a web application.
#[derive(Debug, Clone)]
pub struct WebBackend {
    provider: String,
    driver: WebDriverClient,
}

impl WebBackend {
    /// Build a backend for `provider`, talking to the driver at `driver`.
    pub fn new(provider: String, driver: WebDriverClient) -> Self {
        Self { provider, driver }
    }

    /// The provider this backend serves.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Ask the web chat and hand back the answer text.
    async fn ask(&self, request: &ChatCompletionRequest) -> Result<String, GatewayError> {
        // The driver flattens the history into one prompt, so the messages go
        // over the wire in their OpenAI shape.
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|message| serde_json::to_value(message).unwrap_or(Value::Null))
            .collect();
        let answer = self
            .driver
            .chat(&self.provider, &request.model, &messages)
            .await
            .map_err(map_driver_error)?;
        Ok(answer.text)
    }
}

/// Translate a driver failure into a gateway error.
///
/// The distinction survives: a dead session must not look like a broken
/// selector, because the two are fixed by completely different people.
fn map_driver_error(error: DriverError) -> GatewayError {
    match error {
        DriverError::NotLoggedIn => GatewayError::Upstream(
            "web session is not signed in — sign in again in the browser profile".to_string(),
        ),
        DriverError::NeedsOperator => GatewayError::Upstream(
            "web provider needs a manual step (2FA, captcha, confirmation)".to_string(),
        ),
        DriverError::SelectorMiss => GatewayError::Upstream(
            "web provider page no longer matches the adapter selectors".to_string(),
        ),
        DriverError::NoAdapter => {
            GatewayError::Upstream("web provider has no adapter in the driver".to_string())
        }
        DriverError::Upstream(detail) => GatewayError::Upstream(format!("web provider: {detail}")),
        DriverError::Unavailable(detail) => {
            GatewayError::Upstream(format!("web driver unavailable: {detail}"))
        }
    }
}

#[async_trait]
impl ChatBackend for WebBackend {
    fn name(&self) -> &'static str {
        // The trait wants a static str; the provider id lives in the struct.
        "web"
    }

    fn attribution(&self, _token: &str) -> Attribution {
        Attribution {
            provider: Some(self.provider.clone()),
            // A web chat has no credential row: the session is the credential.
            connection_id: None,
            combo_name: None,
            tokens_compressed: None,
            model: None,
        }
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        let answer = self.ask(&request).await?;
        Ok(ChatCompletionResponse {
            id: format!("chatcmpl-web-{}", ids::unix_seconds()),
            object: "chat.completion",
            created: ids::unix_seconds(),
            model: request.model.clone(),
            choices: vec![ChatChoice {
                index: 0,
                finish_reason: "stop".to_string(),
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(Value::from(answer)),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            }],
            // A web chat reports no token counts: the honest number is zero,
            // and an unpriced model produces no cost header.
            usage: Usage::default(),
            raw: None,
        })
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        let (tx, rx) = mpsc::channel(8);
        let backend = self.clone();
        tokio::spawn(async move {
            let result = async {
                let answer = backend.ask(&request).await?;
                let _ = tx
                    .send(Ok(StreamChunk {
                        content: Some(answer),
                        ..Default::default()
                    }))
                    .await;
                let _ = tx.send(Ok(StreamChunk::done("stop"))).await;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                let _ = tx.send(Err(error)).await;
            }
        });
        Box::pin(ReceiverStream::new(rx))
    }
}

/// The web providers that may be registered as backends.
///
/// The registry lists every web provider of the catalog, but only the ones the
/// driver actually has an adapter for can answer: an adapter is the code that
/// knows the page's composer and the place where the answer is rendered. A
/// provider without one would sit in the catalog and fail on every call, which
/// is worse than being absent.
///
/// `supported` is what the driver reports; when it is empty the driver could
/// not be asked at all, and nothing is registered.
pub fn registrable_providers<'a>(
    registry_ids: impl IntoIterator<Item = &'a str>,
    supported: &BTreeSet<String>,
) -> Vec<String> {
    if supported.is_empty() {
        return Vec::new();
    }
    registry_ids
        .into_iter()
        .filter(|id| supported.contains(*id))
        .map(|id| id.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn a_web_provider_without_an_adapter_is_not_registrable() {
        let supported: BTreeSet<String> = ["chatgpt-web".to_string(), "claude-web".to_string()]
            .into_iter()
            .collect();
        let registrable = registrable_providers(
            ["chatgpt-web", "claude-web", "deepseek-web", "blackbox-web"],
            &supported,
        );
        assert_eq!(
            registrable,
            vec!["chatgpt-web".to_string(), "claude-web".to_string()]
        );
    }

    #[test]
    fn a_driver_that_knows_nothing_registers_nothing() {
        let supported: BTreeSet<String> = BTreeSet::new();
        let registrable = registrable_providers(["chatgpt-web", "claude-web"], &supported);
        assert!(registrable.is_empty());
    }

    fn request(model: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.to_string(),
            messages: vec![heterion_router_core::ChatMessage {
                role: "user".to_string(),
                content: Some(json!("hi")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_answer_comes_back_as_a_chat_completion() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"text": "hello from the web"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let backend = WebBackend::new(
            "chatgpt-web".to_string(),
            WebDriverClient::new(&server.uri()),
        );
        let response = backend.complete(request("gpt-5")).await.expect("answered");
        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].finish_reason, "stop");
        assert_eq!(
            response.choices[0].message.content,
            Some(json!("hello from the web"))
        );
        // No token counts are known: usage stays zero.
        assert_eq!(response.usage.total_tokens, 0);
    }

    #[tokio::test]
    async fn a_dead_session_is_reported_as_such() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "not_logged_in"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let backend = WebBackend::new(
            "chatgpt-web".to_string(),
            WebDriverClient::new(&server.uri()),
        );
        let error = backend
            .complete(request("gpt-5"))
            .await
            .expect_err("the session is gone");
        assert!(error.to_string().contains("not signed in"));
    }

    #[tokio::test]
    async fn the_provider_is_named_in_the_attribution() {
        let backend = WebBackend::new(
            "claude-web".to_string(),
            WebDriverClient::new("http://127.0.0.1:1"),
        );
        let attribution = backend.attribution("token");
        assert_eq!(attribution.provider.as_deref(), Some("claude-web"));
        assert!(attribution.connection_id.is_none());
    }
}
