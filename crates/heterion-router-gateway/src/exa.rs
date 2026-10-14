//! Exa search executor (`executor: "exa"` providers).
//!
//! A search tool surfaced through the chat surface, deliberately not a chat
//! combo member (ADR-016): the last user message becomes the query, the
//! `/search` answer is rendered as a cited text block. There is no upstream
//! streaming — the whole answer arrives as one delta followed by `stop`.

use async_trait::async_trait;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use heterion_router_http::HttpClient;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream, next_token};
use crate::ids;
use crate::openai::try_each_credential;

/// How many results one search asks for.
const NUM_RESULTS: u32 = 5;

/// Render one search result as a cited line block.
fn render_result(index: usize, result: &Value) -> String {
    let title = result
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("(untitled)");
    let url = result.get("url").and_then(Value::as_str).unwrap_or("");
    let mut line = format!("{}. {title}\n   {url}", index + 1);
    if let Some(date) = result.get("publishedDate").and_then(Value::as_str) {
        line.push_str(&format!("\n   published: {date}"));
    }
    let snippet_source = result.get("text").or_else(|| result.get("highlights"));
    let snippet: String = match snippet_source {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" … "),
        _ => String::new(),
    };
    if !snippet.is_empty() {
        let snippet: String = snippet.chars().take(300).collect();
        line.push_str(&format!("\n   {snippet}"));
    }
    line
}

/// Turn a `/search` payload into a cited answer.
pub fn render_search_answer(query: &str, payload: &Value) -> String {
    let results = payload
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if results.is_empty() {
        return format!("No search results for: {query}");
    }
    let rendered: Vec<String> = results
        .iter()
        .enumerate()
        .map(|(index, result)| render_result(index, result))
        .collect();
    format!(
        "Found {} result(s) for \"{}\":\n\n{}",
        results.len(),
        query,
        rendered.join("\n\n")
    )
}

/// The query is the last user message's flattened text.
fn search_query(request: &ChatCompletionRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .map(|message| heterion_router_core::content_text(message.content.as_ref()))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Exa search executor over HTTP with token rotation.
#[derive(Debug, Clone)]
pub struct ExaBackend {
    http: HttpClient,
    provider: String,
    endpoint_url: String,
    tokens: Vec<String>,
    next: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ExaBackend {
    /// Build for `provider` against `endpoint_url` (the API root, e.g.
    /// `https://api.exa.ai` — Exa's OpenAPI has no `/v1` prefix) with
    /// rotating keys.
    pub fn new(
        provider: String,
        endpoint_url: String,
        tokens: Vec<String>,
    ) -> Result<Self, heterion_router_http::client::HttpError> {
        Ok(Self {
            http: HttpClient::new(600)?,
            provider,
            endpoint_url: endpoint_url.trim_end_matches('/').to_string(),
            tokens,
            next: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    /// Pool size (number of rotated credentials).
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    /// One `/search` POST with the rotating key in `x-api-key`.
    async fn run_search(&self, query: &str) -> Result<Value, GatewayError> {
        // Highlights are requested explicitly: without `contents` the API
        // returns only title/url/date metadata (Exa OpenAPI spec).
        let body = json!({
            "query": query,
            "numResults": NUM_RESULTS,
            "contents": { "highlights": true },
        });
        let url = format!("{}/search", self.endpoint_url);
        let payload = try_each_credential(&self.provider, self.tokens.len().max(1), || {
            let body = body.clone();
            let url = url.clone();
            let token = next_token(&self.tokens, &self.next).map(str::to_string);
            async move {
                self.http
                    .post_json_with_headers(
                        &url,
                        None,
                        &[("x-api-key", token.as_deref().unwrap_or(""))],
                        &body,
                    )
                    .await
                    .map_err(|error| GatewayError::Upstream(error.to_string()))?
                    .json()
                    .await
                    .map_err(|error| GatewayError::Upstream(error.to_string()))
            }
        })
        .await?;
        Ok(payload)
    }
}

#[async_trait]
impl ChatBackend for ExaBackend {
    fn name(&self) -> &'static str {
        "exa"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        let query = search_query(&request);
        if query.is_empty() {
            return Err(GatewayError::InvalidRequest(
                "search query is empty: send the query as the last user message".into(),
            ));
        }
        let payload = self.run_search(&query).await?;
        let answer = render_search_answer(&query, &payload);
        Ok(ChatCompletionResponse {
            id: format!("chatcmpl-search-{}", ids::unix_seconds()),
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
            // Exa bills per search, not per token: an unpriced zero is the
            // honest number and the cost header stays off.
            usage: Usage::default(),
            raw: None,
        })
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        let (tx, rx) = mpsc::channel(64);
        let backend = self.clone();
        tokio::spawn(async move {
            let result = async {
                let query = search_query(&request);
                if query.is_empty() {
                    return Err(GatewayError::InvalidRequest(
                        "search query is empty: send the query as the last user message".into(),
                    ));
                }
                let payload = backend.run_search(&query).await?;
                let answer = render_search_answer(&query, &payload);
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request(text: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "exa-search/exa-search".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(Value::from(text)),
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
    async fn a_user_message_becomes_the_query_and_results_are_cited() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(header("x-api-key", "sk-exa"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {"title": "Heterion Router", "url": "https://example.com/hr",
                     "publishedDate": "2026-10-01",
                     "highlights": ["a rust model router", "priority combos"]},
                    {"title": "Exa docs", "url": "https://example.com/exa"}
                ]
            })))
            .mount(&server)
            .await;

        let backend = ExaBackend::new(
            "exa-search".to_string(),
            server.uri(),
            vec!["sk-exa".to_string()],
        )
        .unwrap();
        let response = backend
            .complete(request("heterion router news"))
            .await
            .unwrap();
        let text = response.choices[0].message.text();
        assert!(text.contains("heterion router news"), "{text}");
        assert!(text.contains("Heterion Router"), "{text}");
        assert!(text.contains("https://example.com/hr"), "{text}");
        assert!(text.contains("2026-10-01"), "{text}");
        assert!(text.contains("a rust model router"), "{text}");
        assert!(text.contains("Exa docs"), "{text}");
        assert_eq!(response.choices[0].finish_reason, "stop");
    }

    #[tokio::test]
    async fn an_empty_result_list_is_reported_not_fabricated() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "results": [] })))
            .mount(&server)
            .await;

        let backend = ExaBackend::new(
            "exa-search".to_string(),
            server.uri(),
            vec!["sk-exa".to_string()],
        )
        .unwrap();
        let response = backend.complete(request("nothing to see")).await.unwrap();
        assert!(
            response.choices[0]
                .message
                .text()
                .contains("No search results")
        );
    }

    #[tokio::test]
    async fn an_upstream_401_surfaces_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": "unauthorized"
            })))
            .mount(&server)
            .await;

        let backend = ExaBackend::new(
            "exa-search".to_string(),
            server.uri(),
            vec!["sk-bad".to_string()],
        )
        .unwrap();
        let error = backend.complete(request("query")).await.unwrap_err();
        assert!(error.to_string().contains("upstream status 401"), "{error}");
    }

    #[tokio::test]
    async fn a_missing_query_is_a_clean_client_error() {
        let server = MockServer::start().await;
        let backend = ExaBackend::new(
            "exa-search".to_string(),
            server.uri(),
            vec!["sk-exa".to_string()],
        )
        .unwrap();
        let error = backend.complete(request("   ")).await.unwrap_err();
        assert!(error.to_string().contains("query is empty"), "{error}");
    }

    #[tokio::test]
    async fn streaming_emits_one_answer_chunk_then_stop() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{"title": "T", "url": "https://example.com"}]
            })))
            .mount(&server)
            .await;

        let backend = ExaBackend::new(
            "exa-search".to_string(),
            server.uri(),
            vec!["sk-exa".to_string()],
        )
        .unwrap();
        let mut request = request("query");
        request.stream = true;
        use futures::StreamExt;
        let mut stream = backend.stream(request);
        let mut text = String::new();
        let mut finish = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            if let Some(content) = chunk.content {
                text.push_str(&content);
            }
            if let Some(reason) = chunk.finish_reason {
                finish = Some(reason);
            }
        }
        assert!(text.contains("https://example.com"), "{text}");
        assert_eq!(finish.as_deref(), Some("stop"));
    }
}
