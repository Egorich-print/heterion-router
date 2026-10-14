//! Grok Build (`grok-cli`) backend: real HTTP executor.
//!
//! First Phase 3a executor. It speaks the upstream `/v1/responses` endpoint
//! and translates both directions with the ported `heterion-router-translate`
//! pair, so an OpenAI-shaped client request is served end to end:
//!
//! ```text
//! chat.request --openai_to_responses--> POST /responses --SSE/response-->
//!   --convert_event--> StreamChunk --gateway SSE--> chat.chunk
//! ```

use async_trait::async_trait;
use heterion_router_core::{
    ChatCompletionRequest, ChatCompletionResponse, GatewayError, StreamChunk,
};
use heterion_router_http::HttpClient;
use heterion_router_translate::requests::openai_to_responses;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream, openai_request_body};
use crate::responses::{
    MissingTerminal, pump_sse_stream, response_to_completion, responses_event_handler,
};

/// Pinned Grok client version (mirrors `GROK_BUILD_DEFAULT_CLIENT_VERSION`).
/// The proxy rejects missing/outdated versions with 426, so this must track
/// the JS constant.
const GROK_CLIENT_VERSION: &str = "0.2.106";
const GROK_CLIENT_IDENTIFIER: &str = "grok-shell";

fn now_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn rfc3339_from_epoch(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let seconds = epoch.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn refresh_expiry(payload: &Value) -> Option<String> {
    if let Some(expiry) = payload.get("expires_at") {
        if let Some(expiry) = expiry.as_str() {
            return Some(expiry.to_string());
        }
        if let Some(epoch) = expiry.as_i64() {
            return Some(rfc3339_from_epoch(epoch));
        }
    }
    let seconds = payload.get("expires_in").and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str()?.parse::<u64>().ok())
    })?;
    Some(rfc3339_from_epoch(now_unix_seconds() + seconds as i64))
}

fn grok_user_agent() -> String {
    format!(
        "{GROK_CLIENT_IDENTIFIER}/{GROK_CLIENT_VERSION} ({}; {})",
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// Session headers the Grok proxy requires (mirrors
/// `getGrokBuildSessionHeaders`). Without `x-grok-client-version` the
/// upstream answers 426.
fn session_headers<'a>(
    model: &'a str,
    stream: bool,
    user_agent: &'a str,
) -> Vec<(&'static str, &'a str)> {
    vec![
        (
            "Accept",
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        ),
        ("x-grok-client-version", GROK_CLIENT_VERSION),
        ("x-grok-client-identifier", GROK_CLIENT_IDENTIFIER),
        ("x-grok-client-mode", "headless"),
        ("User-Agent", user_agent),
        ("X-XAI-Token-Auth", "xai-grok-cli"),
        ("x-authenticateresponse", "authenticate-response"),
        ("x-grok-model-override", model),
    ]
}

pub type RefreshCallback = std::sync::Arc<
    dyn Fn(&str, &str, Option<&str>, Option<&str>) -> Result<(), String> + Send + Sync,
>;

type RefreshLock = Arc<tokio::sync::Mutex<()>>;
type RefreshLockMap = Arc<tokio::sync::Mutex<HashMap<usize, RefreshLock>>>;

/// Grok Build executor over HTTP.
#[derive(Clone)]
pub struct GrokCliBackend {
    http: HttpClient,
    base_url: String,
    oauth_token_url: String,
    credentials: std::sync::Arc<std::sync::Mutex<Vec<Credential>>>,
    on_refresh: Option<RefreshCallback>,
    refresh_locks: RefreshLockMap,
    next: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl std::fmt::Debug for GrokCliBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrokCliBackend")
            .field("base_url", &self.base_url)
            .field("credential_count", &self.credential_count())
            .finish()
    }
}

/// One credential for rotation: access token + optional refresh token.
#[derive(Debug, Clone)]
pub struct Credential {
    pub connection_id: Option<String>,
    pub token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<String>,
}

impl GrokCliBackend {
    /// Build against `base_url` (e.g. `https://cli-chat-proxy.grok.com/v1`)
    /// with a Bearer `token`.
    pub fn new(
        base_url: String,
        token: String,
    ) -> Result<Self, heterion_router_http::client::HttpError> {
        Self::with_credentials(
            base_url,
            vec![Credential {
                connection_id: None,
                token,
                refresh_token: None,
                expires_at: None,
            }],
        )
    }

    /// Build with a rotation pool of credentials.
    pub fn with_credentials(
        base_url: String,
        credentials: Vec<Credential>,
    ) -> Result<Self, heterion_router_http::client::HttpError> {
        if credentials.is_empty() {
            return Err(heterion_router_http::client::HttpError::Status {
                status: 0,
                body: "grok-cli: no credentials".to_string(),
            });
        }
        Ok(Self {
            http: HttpClient::new(600)?,
            base_url: base_url.trim_end_matches('/').to_string(),
            oauth_token_url: std::env::var("GROK_OAUTH_TOKEN_URL")
                .unwrap_or_else(|_| "https://auth.x.ai/oauth2/token".to_string()),
            credentials: std::sync::Arc::new(std::sync::Mutex::new(credentials)),
            on_refresh: None,
            refresh_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            next: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    pub fn with_oauth_endpoint(mut self, token_url: String) -> Self {
        self.oauth_token_url = token_url;
        self
    }

    pub fn with_refresh_callback(mut self, callback: RefreshCallback) -> Self {
        self.on_refresh = Some(callback);
        self
    }

    /// Pool size.
    pub fn credential_count(&self) -> usize {
        self.credentials
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// Round-robin credential selection.
    fn next_credential(&self) -> (usize, Credential) {
        let credentials = self
            .credentials
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let index =
            self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % credentials.len();
        (index, credentials[index].clone())
    }

    fn update_credential(&self, index: usize, credential: Credential) {
        let mut credentials = self
            .credentials
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(slot) = credentials.get_mut(index) {
            *slot = credential;
        }
    }

    fn credential_at(&self, index: usize) -> Option<Credential> {
        self.credentials
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(index)
            .cloned()
    }

    async fn refresh_after_unauthorized(
        &self,
        index: usize,
        credential: &Credential,
    ) -> Result<Credential, GatewayError> {
        let lock = {
            let mut locks = self.refresh_locks.lock().await;
            locks
                .entry(index)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        let current = self.credential_at(index).ok_or_else(|| {
            GatewayError::Upstream("grok-cli credential disappeared during refresh".to_string())
        })?;
        if current.token != credential.token
            || current.refresh_token != credential.refresh_token
            || current.expires_at != credential.expires_at
        {
            return Ok(current);
        }
        let refresh_token = current.refresh_token.as_deref().ok_or_else(|| {
            GatewayError::Upstream(
                "upstream status 401: token expired and no refresh token".to_string(),
            )
        })?;
        let refreshed = self.refresh_token(refresh_token).await?;
        let refreshed = self.persist_refreshed(&current, &refreshed)?;
        self.update_credential(index, refreshed.clone());
        Ok(refreshed)
    }

    fn persist_refreshed(
        &self,
        original: &Credential,
        refreshed: &Credential,
    ) -> Result<Credential, GatewayError> {
        let mut updated = refreshed.clone();
        updated.connection_id = original.connection_id.clone();
        if let (Some(connection_id), Some(callback)) =
            (updated.connection_id.as_deref(), &self.on_refresh)
        {
            callback(
                connection_id,
                &updated.token,
                updated.refresh_token.as_deref(),
                updated.expires_at.as_deref(),
            )
            .map_err(GatewayError::Upstream)?;
        }
        Ok(updated)
    }

    /// Call the upstream, refreshing the selected credential on 401.
    async fn call_with_refresh(
        &self,
        url: &str,
        index: usize,
        credential: &Credential,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> Result<Value, GatewayError> {
        let response = match self
            .http
            .post_json_with_headers(url, Some(&credential.token), headers, body)
            .await
        {
            Ok(response) => response,
            Err(heterion_router_http::client::HttpError::Status { status: 401, .. }) => {
                let refreshed = self.refresh_after_unauthorized(index, credential).await?;
                self.http
                    .post_json_with_headers(url, Some(&refreshed.token), headers, body)
                    .await
                    .map_err(|error| GatewayError::Upstream(error.to_string()))?
            }
            Err(error) => return Err(GatewayError::Upstream(error.to_string())),
        };
        let payload: Value = response
            .json()
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;
        if let Some(message) = payload
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
        {
            return Err(GatewayError::Upstream(message.to_string()));
        }
        Ok(payload)
    }

    async fn refresh_token(&self, refresh_token: &str) -> Result<Credential, GatewayError> {
        let client_id = std::env::var("GROK_OAUTH_CLIENT_ID")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let mut fields = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ];
        if let Some(client_id) = client_id.as_deref() {
            fields.insert(1, ("client_id", client_id));
        }
        let payload: Value = self
            .http
            .post_form(&self.oauth_token_url, &fields)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?
            .json()
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;
        let token = payload
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                GatewayError::Upstream("refresh response has no access_token".to_string())
            })?
            .to_string();
        let refresh_token = payload
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(refresh_token)
            .to_string();
        Ok(Credential {
            connection_id: None,
            token,
            refresh_token: Some(refresh_token),
            expires_at: refresh_expiry(&payload),
        })
    }

    fn responses_url(&self) -> String {
        format!("{}/responses", self.base_url)
    }

    async fn stream_response(
        &self,
        index: usize,
        credential: &Credential,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> Result<reqwest::Response, GatewayError> {
        match self
            .http
            .post_json_with_headers(
                &self.responses_url(),
                Some(&credential.token),
                headers,
                body,
            )
            .await
        {
            Ok(response) => Ok(response),
            Err(heterion_router_http::client::HttpError::Status { status: 401, .. }) => {
                let refreshed = self.refresh_after_unauthorized(index, credential).await?;
                self.http
                    .post_json_with_headers(
                        &self.responses_url(),
                        Some(&refreshed.token),
                        headers,
                        body,
                    )
                    .await
                    .map_err(|error| GatewayError::Upstream(error.to_string()))
            }
            Err(error) => Err(GatewayError::Upstream(error.to_string())),
        }
    }

    async fn drive_stream(
        &self,
        request: ChatCompletionRequest,
        tx: mpsc::Sender<Result<StreamChunk, GatewayError>>,
    ) -> Result<(), GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }

        let mut upstream = openai_to_responses(&request.model, &openai_request_body(&request));
        if let Some(input) = &request.responses_input {
            upstream["input"] = input.clone();
        }
        upstream["stream"] = Value::from(true);
        let user_agent = grok_user_agent();
        let headers = session_headers(&request.model, true, &user_agent);
        let headers_vec: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, *v)).collect();
        let (index, credential) = self.next_credential();
        let response = self
            .stream_response(index, &credential, &headers_vec, &upstream)
            .await?;

        pump_sse_stream(
            response,
            &tx,
            responses_event_handler(&request.model),
            MissingTerminal::SynthesizeStop,
        )
        .await
    }
}

#[async_trait]
impl ChatBackend for GrokCliBackend {
    fn name(&self) -> &'static str {
        "grok-cli"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        let mut upstream = openai_to_responses(&request.model, &openai_request_body(&request));
        if let Some(input) = &request.responses_input {
            upstream["input"] = input.clone();
        }
        upstream["stream"] = Value::from(false);
        let user_agent = grok_user_agent();
        let headers = session_headers(&request.model, false, &user_agent);
        let headers_vec: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, *v)).collect();

        let mut last_error: Option<GatewayError> = None;
        for _ in 0..self.credential_count() {
            let (index, credential) = self.next_credential();
            match self
                .call_with_refresh(
                    &self.responses_url(),
                    index,
                    &credential,
                    &headers_vec,
                    &upstream,
                )
                .await
            {
                Ok(payload) => return Ok(response_to_completion(&request.model, &payload)),
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            GatewayError::Upstream("grok-cli: all credentials failed".to_string())
        }))
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        let (tx, rx) = mpsc::channel(64);
        let backend = self.clone();
        tokio::spawn(async move {
            if let Err(error) = backend.drive_stream(request, tx.clone()).await {
                let _ = tx.send(Err(error)).await;
            }
        });
        Box::pin(ReceiverStream::new(rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use heterion_router_core::{ChatCompletionRequest, ChatMessage};
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, method, path},
    };

    #[test]
    fn formats_refresh_expiry() {
        assert_eq!(
            rfc3339_from_epoch(1_700_000_000),
            "2023-11-14T22:13:20.000Z"
        );
        assert_eq!(
            refresh_expiry(&json!({"expires_at": "2999-01-01T00:00:00Z"})),
            Some("2999-01-01T00:00:00Z".to_string())
        );
        assert!(refresh_expiry(&json!({"expires_in": "3600"})).is_some());
    }

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "grok-4.6".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "Hi".to_string())],
            stream: true,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn streams_translated_deltas_to_finish() {
        let server = MockServer::start().await;
        let sse = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"!\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":2,\"total_tokens\":5}}}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(body_string_contains("\"input_text\""))
            // The proxy rejects missing/outdated client versions with 426.
            .and(header("x-grok-client-version", "0.2.106"))
            .and(header("x-grok-client-identifier", "grok-shell"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let backend = GrokCliBackend::new(server.uri(), "tok".to_string()).unwrap();
        let chunks: Vec<StreamChunk> = backend
            .stream(request())
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();

        let text: String = chunks
            .iter()
            .filter_map(|chunk| chunk.content.clone())
            .collect();
        assert_eq!(text, "Hello!");
        assert_eq!(
            chunks.last().unwrap().finish_reason.as_deref(),
            Some("stop")
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn complete_converts_response_object() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_1",
                "model": "grok-4.6",
                "output": [
                    {"type": "message", "content": [{"type": "output_text", "text": "Hi there"}]}
                ],
                "usage": {"input_tokens": 4, "output_tokens": 2, "total_tokens": 6},
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = GrokCliBackend::new(server.uri(), "tok".to_string()).unwrap();
        let mut req = request();
        req.stream = false;
        let response = backend.complete(req).await.unwrap();

        assert_eq!(response.choices[0].message.text(), "Hi there");
        assert_eq!(response.usage.prompt_tokens, 4);
        assert_eq!(response.usage.total_tokens, 6);
        assert_eq!(response.choices[0].finish_reason, "stop");
        server.verify().await;
    }

    #[tokio::test]
    async fn upstream_status_error_surfaces() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({
                "error": {"message": "slow down", "code": "rate_limit"}
            })))
            .mount(&server)
            .await;

        let backend = GrokCliBackend::new(server.uri(), "tok".to_string()).unwrap();
        let mut req = request();
        req.stream = false;
        let error = backend.complete(req).await.unwrap_err();
        assert!(matches!(error, GatewayError::Upstream(_)));
        assert!(error.to_string().contains("slow down"));
    }

    #[tokio::test]
    async fn failed_event_errors_the_stream() {
        let server = MockServer::start().await;
        let sse = concat!(
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"error\":{\"message\":\"boom\"}}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;

        let backend = GrokCliBackend::new(server.uri(), "tok".to_string()).unwrap();
        let results: Vec<_> = backend.stream(request()).collect::<Vec<_>>().await;
        assert_eq!(results.len(), 1);
        let error = results.into_iter().next().unwrap().unwrap_err();
        assert!(error.to_string().contains("boom"));
    }
}

#[cfg(test)]
mod tool_tests {
    use super::*;
    use futures::StreamExt;
    use heterion_router_core::{ChatCompletionRequest, ChatMessage};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn streams_incremental_tool_call_arguments() {
        let server = MockServer::start().await;
        let sse = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_1\",\"name\":\"get_time\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"city\\\":\"}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"\\\"SF\\\"}\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_1\",\"name\":\"get_time\",\"arguments\":\"\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;

        let backend = GrokCliBackend::new(server.uri(), "tok".to_string()).unwrap();
        let request = ChatCompletionRequest {
            model: "grok-4.6".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "time?".to_string())],
            stream: true,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            ..Default::default()
        };

        let chunks: Vec<StreamChunk> = backend
            .stream(request)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();

        let calls: Vec<&serde_json::Value> = chunks
            .iter()
            .filter_map(|c| c.tool_calls.as_ref())
            .collect();
        assert_eq!(calls.len(), 2, "announce + arguments");
        assert_eq!(
            calls[0][0]["function"]["name"],
            serde_json::json!("get_time")
        );
        assert_eq!(
            calls[1][0]["function"]["arguments"],
            serde_json::json!("{\"city\":\"SF\"}")
        );
        assert_eq!(
            chunks.last().unwrap().finish_reason.as_deref(),
            Some("tool_calls")
        );
    }
}

#[cfg(test)]
mod rotation_tests {
    use super::*;
    use heterion_router_core::{ChatCompletionRequest, ChatMessage};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, method, path},
    };

    #[tokio::test]
    async fn rotates_past_a_rejected_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer bad"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"error": {"message": "invalid token"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer good"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "id": "resp_ok",
                    "output": [{"type": "message", "content": [{"type": "output_text", "text": "rotated"}]}],
                    "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
                }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let backend = GrokCliBackend::with_credentials(
            server.uri(),
            vec![
                Credential {
                    connection_id: None,
                    token: "bad".to_string(),
                    refresh_token: None,
                    expires_at: None,
                },
                Credential {
                    connection_id: None,
                    token: "good".to_string(),
                    refresh_token: None,
                    expires_at: None,
                },
            ],
        )
        .unwrap();
        assert_eq!(backend.credential_count(), 2);

        let request = ChatCompletionRequest {
            model: "grok-4.6".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            ..Default::default()
        };
        let response = backend.complete(request).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "rotated");
        server.verify().await;

        // Rotation order, not just traffic: the rejected token is tried first
        // and the second credential is the one that answers.
        let seen = server.received_requests().await.unwrap();
        let bearers: Vec<_> = seen
            .iter()
            .map(|request| {
                request.headers["authorization"]
                    .to_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(bearers, vec!["Bearer bad", "Bearer good"]);
    }

    #[tokio::test]
    async fn refreshes_the_rejected_credential_and_retries_original_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer bad"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=refresh-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fresh",
                "refresh_token": "refresh-2",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer fresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_refreshed",
                "output": [{"type": "message", "content": [{"type": "output_text", "text": "ok"}]}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let persisted = Arc::new(Mutex::new(Vec::new()));
        let persisted_callback = Arc::clone(&persisted);
        let backend = GrokCliBackend::with_credentials(
            server.uri(),
            vec![Credential {
                connection_id: Some("connection-1".to_string()),
                token: "bad".to_string(),
                refresh_token: Some("refresh-1".to_string()),
                expires_at: Some("2000-01-01T00:00:00.000Z".to_string()),
            }],
        )
        .unwrap()
        .with_oauth_endpoint(format!("{}/oauth2/token", server.uri()))
        .with_refresh_callback(Arc::new(move |id, token, refresh, expires| {
            persisted_callback.lock().unwrap().push((
                id.to_string(),
                token.to_string(),
                refresh.map(str::to_string),
                expires.map(str::to_string),
            ));
            Ok(())
        }));
        let request = ChatCompletionRequest {
            model: "grok-4.6".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
            stream: false,
            ..Default::default()
        };
        let response = backend.complete(request).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "ok");
        {
            let persisted = persisted.lock().unwrap();
            assert_eq!(persisted.len(), 1);
            assert_eq!(persisted[0].0, "connection-1");
            assert_eq!(persisted[0].1, "fresh");
            assert_eq!(persisted[0].2.as_deref(), Some("refresh-2"));
            assert!(
                persisted[0]
                    .3
                    .as_deref()
                    .is_some_and(|expiry| expiry.ends_with("Z"))
            );
        }
        server.verify().await;

        // The retry must reuse the *original* request body, not an empty or
        // reshaped one, and must go out under the refreshed bearer.
        let seen = server.received_requests().await.unwrap();
        let responses: Vec<_> = seen
            .iter()
            .filter(|request| request.url.path() == "/responses")
            .collect();
        assert_eq!(responses.len(), 2, "one rejected attempt, one retry");
        assert_eq!(
            responses[0].body, responses[1].body,
            "the retry must replay the original request body"
        );
        assert_eq!(responses[1].headers["authorization"], "Bearer fresh");
    }

    #[tokio::test]
    async fn concurrent_refresh_is_single_flight() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer bad"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_delay(std::time::Duration::from_millis(50))
                    .set_body_json(json!({"error": {"message": "invalid token"}})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fresh",
                "refresh_token": "refresh-2"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer fresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_ok",
                "output": [{"type": "message", "content": [{"type": "output_text", "text": "ok"}]}]
            })))
            .mount(&server)
            .await;

        let refreshes = Arc::new(Mutex::new(0));
        let refreshes_callback = Arc::clone(&refreshes);
        let backend = Arc::new(
            GrokCliBackend::with_credentials(
                server.uri(),
                vec![Credential {
                    connection_id: Some("connection-1".to_string()),
                    token: "bad".to_string(),
                    refresh_token: Some("refresh-1".to_string()),
                    expires_at: None,
                }],
            )
            .unwrap()
            .with_oauth_endpoint(format!("{}/oauth2/token", server.uri()))
            .with_refresh_callback(Arc::new(move |_, _, _, _| {
                *refreshes_callback.lock().unwrap() += 1;
                Ok(())
            })),
        );
        let request = ChatCompletionRequest {
            model: "grok-4.6".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
            stream: false,
            ..Default::default()
        };
        let (first, second) =
            tokio::join!(backend.complete(request.clone()), backend.complete(request));
        assert!(first.is_ok());
        assert!(second.is_ok());
        assert_eq!(*refreshes.lock().unwrap(), 1);
        server.verify().await;

        // Both callers hit the upstream, but the refresh went out exactly once:
        // two concurrent 401s collapse into a single OAuth round-trip that both
        // waiters then retry under. Observed shape:
        //   Bearer bad, Bearer bad, /oauth2/token, Bearer fresh, Bearer fresh.
        let seen = server.received_requests().await.unwrap();
        let mut calls = seen
            .iter()
            .map(|request| {
                let auth = request
                    .headers
                    .get("authorization")
                    .map(|value| value.to_str().unwrap().to_string())
                    .unwrap_or_default();
                (request.url.path().to_string(), auth)
            })
            .collect::<Vec<_>>();
        let refreshes_sent = calls
            .iter()
            .filter(|(path, _)| path == "/oauth2/token")
            .count();
        assert_eq!(refreshes_sent, 1, "the refresh must be single-flight");
        calls.sort();
        assert_eq!(
            calls,
            vec![
                ("/oauth2/token".to_string(), String::new()),
                ("/responses".to_string(), "Bearer bad".to_string()),
                ("/responses".to_string(), "Bearer bad".to_string()),
                ("/responses".to_string(), "Bearer fresh".to_string()),
                ("/responses".to_string(), "Bearer fresh".to_string()),
            ],
            "each caller is rejected once and retries once under the shared token"
        );
    }

    #[test]
    fn empty_pool_is_rejected() {
        assert!(GrokCliBackend::with_credentials("http://x".to_string(), vec![]).is_err());
    }
}

#[cfg(test)]
mod tool_completion_tests {
    use super::*;
    use heterion_router_core::{ChatCompletionRequest, ChatMessage};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn nonstream_function_call_becomes_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_1",
                "output": [
                    {"type": "function_call", "call_id": "call_1", "name": "get_time",
                     "arguments": "{\"city\":\"SF\"}"}
                ],
                "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5},
            })))
            .mount(&server)
            .await;

        let backend = GrokCliBackend::new(server.uri(), "tok".to_string()).unwrap();
        let request = ChatCompletionRequest {
            model: "grok-4.6".to_string(),
            messages: vec![ChatMessage::plain("user", "time?")],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: Some(serde_json::json!([{
                "type": "function",
                "function": {"name": "get_time", "parameters": {"type": "object"}}
            }])),
            tool_choice: None,
            ..Default::default()
        };
        let response = backend.complete(request).await.unwrap();
        assert_eq!(response.choices[0].finish_reason, "tool_calls");
        let calls = response.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0]["function"]["name"], serde_json::json!("get_time"));
        assert_eq!(
            calls[0]["function"]["arguments"],
            serde_json::json!("{\"city\":\"SF\"}")
        );
    }
}
