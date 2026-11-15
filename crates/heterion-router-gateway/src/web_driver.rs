//! Client for the Camoufox web driver (ADR-025).
//!
//! The browser lives in a separate Python process: there is no Rust binding
//! for the anti-detect build, and the `playwright` crate would download its
//! own Firefox. The driver speaks a small JSON protocol over loopback HTTP.
//!
//! The driver's errors are deliberately distinct, because they need different
//! reactions: a dead session is fixed by signing in again, a human step is
//! fixed by the operator, and a broken selector is fixed by code.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::time::Duration;

use serde_json::{Value, json};

/// Default address of the driver process.
pub const DEFAULT_DRIVER_URL: &str = "http://127.0.0.1:20131";

/// How long one chat may take in the browser.
///
/// A web chat answers in seconds, not milliseconds — the model has to think,
/// and the page renders the answer token by token.
const CHAT_TIMEOUT: Duration = Duration::from_secs(300);

/// What the driver said went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// The operator's session is gone: sign in again.
    NotLoggedIn,
    /// A human step is required (2FA, captcha, a confirmation screen).
    NeedsOperator,
    /// The page no longer matches the adapter's selectors.
    SelectorMiss,
    /// The provider answered with an error of its own.
    Upstream(String),
    /// The driver process is not reachable.
    Unavailable(String),
    /// The driver has no adapter for this provider.
    NoAdapter,
}

impl DriverError {
    /// The wire name, as the driver spells it.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotLoggedIn => "not_logged_in",
            Self::NeedsOperator => "needs_operator",
            Self::SelectorMiss => "selector_miss",
            Self::Upstream(_) => "upstream_error",
            Self::Unavailable(_) => "unavailable",
            Self::NoAdapter => "no_adapter",
        }
    }

    /// Parse the driver's error code.
    pub fn from_code(code: &str, detail: &str) -> Self {
        match code {
            "not_logged_in" => Self::NotLoggedIn,
            "needs_operator" => Self::NeedsOperator,
            "selector_miss" => Self::SelectorMiss,
            "no_adapter" => Self::NoAdapter,
            "unavailable" => Self::Unavailable(detail.to_string()),
            _ => Self::Upstream(detail.to_string()),
        }
    }

    /// A message for the client.
    pub fn message(&self) -> String {
        match self {
            Self::NotLoggedIn => "web session is not signed in".to_string(),
            Self::NeedsOperator => "web provider needs a manual step by the operator".to_string(),
            Self::SelectorMiss => {
                "web provider page no longer matches the adapter selectors".to_string()
            }
            Self::Upstream(detail) => format!("web provider error: {detail}"),
            Self::Unavailable(detail) => format!("web driver unavailable: {detail}"),
            Self::NoAdapter => "web provider has no adapter in the driver".to_string(),
        }
    }
}

/// Whether a driver process is listening and answers its health check.
///
/// The gateway registers the web backends only when the driver is there:
/// advertising a provider that can never answer would fill the catalog with
/// models that always fail.
pub fn driver_reachable(base: &str) -> bool {
    let rest = base
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().unwrap_or(20131)),
        None => (rest, 80),
    };
    let address = format!("{host}:{port}");
    let mut stream = match std::net::TcpStream::connect(&address) {
        Ok(stream) => stream,
        Err(_) => return false,
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(800)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(800)));
    let request = format!("GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    let text = String::from_utf8_lossy(&response);
    text.contains("200") && text.contains("\"ok\"")
}

/// The providers the driver has an adapter for.
///
/// The driver answers `GET /providers` with the ids it can actually serve.
/// The gateway registers only those: a web provider without an adapter would
/// sit in the catalog and fail on every call.
///
/// The call is synchronous on purpose — the router is built before the async
/// runtime starts serving, and the answer is one small JSON document.
pub fn driver_providers(base: &str) -> BTreeSet<String> {
    let rest = base
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().unwrap_or(20131)),
        None => (rest, 80),
    };
    let address = format!("{host}:{port}");
    let mut stream = match std::net::TcpStream::connect(&address) {
        Ok(stream) => stream,
        Err(_) => return BTreeSet::new(),
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(2000)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(2000)));
    let request =
        format!("GET /providers HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return BTreeSet::new();
    }
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    parse_provider_list(&String::from_utf8_lossy(&response))
}

/// Read the provider ids out of a driver's `/providers` answer.
fn parse_provider_list(response: &str) -> BTreeSet<String> {
    let Some((_headers, body)) = response.split_once("\r\n\r\n") else {
        return BTreeSet::new();
    };
    let Ok(payload) = serde_json::from_str::<Value>(body.trim()) else {
        return BTreeSet::new();
    };
    payload
        .get("providers")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|id| id.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// An answer scraped from a web chat.
#[derive(Debug, Clone)]
pub struct DriverAnswer {
    /// The answer text.
    pub text: String,
    /// The model the page was asked to use, if the adapter could set it.
    pub model: Option<String>,
}

/// HTTP client for the driver process.
#[derive(Debug, Clone)]
pub struct WebDriverClient {
    http: reqwest::Client,
    base: String,
}

impl WebDriverClient {
    /// Build a client for the driver at `base` (e.g. `http://127.0.0.1:20130`).
    pub fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(CHAT_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
        }
    }

    /// The driver address in use.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Whether the driver answers at all.
    pub async fn healthy(&self) -> bool {
        self.http
            .get(format!("{}/health", self.base))
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false)
    }

    /// The session state of every provider the driver knows.
    pub async fn session(&self) -> Result<BTreeMap<String, String>, DriverError> {
        let response = self
            .http
            .get(format!("{}/session", self.base))
            .send()
            .await
            .map_err(|error| DriverError::Unavailable(error.to_string()))?;
        let payload: Value = response
            .json()
            .await
            .map_err(|error| DriverError::Unavailable(error.to_string()))?;
        let providers = payload
            .get("providers")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(key, value)| {
                        (key.clone(), value.as_str().unwrap_or("unknown").to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(providers)
    }

    /// Ask a provider's web chat and wait for the answer.
    pub async fn chat(
        &self,
        provider: &str,
        model: &str,
        messages: &[Value],
    ) -> Result<DriverAnswer, DriverError> {
        let body = json!({
            "provider": provider,
            "model": model,
            "messages": messages,
        });
        let response = self
            .http
            .post(format!("{}/chat", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|error| DriverError::Unavailable(error.to_string()))?;
        let status = response.status();
        let payload: Value = response
            .json()
            .await
            .map_err(|error| DriverError::Unavailable(error.to_string()))?;
        if !status.is_success() {
            let code = payload
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("upstream_error");
            let detail = payload
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            return Err(DriverError::from_code(code, &detail));
        }
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if text.trim().is_empty() {
            return Err(DriverError::SelectorMiss);
        }
        let model = payload
            .get("model")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
        Ok(DriverAnswer { text, model })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn a_driver_error_carries_its_own_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "not_logged_in"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = WebDriverClient::new(&server.uri());
        let error = client
            .chat(
                "chatgpt-web",
                "gpt-5",
                &[json!({"role": "user", "content": "hi"})],
            )
            .await
            .expect_err("the driver refused");
        assert_eq!(error, DriverError::NotLoggedIn);
        assert_eq!(error.code(), "not_logged_in");
    }

    #[tokio::test]
    async fn an_answer_is_read_from_the_driver_reply() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"text": "hello", "model": "gpt-5"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = WebDriverClient::new(&server.uri());
        let answer = client
            .chat(
                "chatgpt-web",
                "gpt-5",
                &[json!({"role": "user", "content": "hi"})],
            )
            .await
            .expect("the driver answered");
        assert_eq!(answer.text, "hello");
        assert_eq!(answer.model.as_deref(), Some("gpt-5"));
    }

    #[tokio::test]
    async fn an_empty_answer_is_a_selector_miss_not_an_answer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "   "})))
            .expect(1)
            .mount(&server)
            .await;
        let client = WebDriverClient::new(&server.uri());
        let error = client
            .chat(
                "chatgpt-web",
                "gpt-5",
                &[json!({"role": "user", "content": "hi"})],
            )
            .await
            .expect_err("an empty answer is not an answer");
        assert_eq!(error, DriverError::SelectorMiss);
    }

    #[test]
    fn a_driver_that_is_not_there_is_not_reachable() {
        // Nothing listens on port 1 of loopback.
        assert!(!driver_reachable("http://127.0.0.1:1"));
    }

    #[test]
    fn the_provider_list_of_the_driver_is_read_from_its_answer() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
            {\"providers\": [\"chatgpt-web\", \"claude-web\"]}";
        let providers = parse_provider_list(response);
        assert!(providers.contains("chatgpt-web"));
        assert!(providers.contains("claude-web"));
        assert_eq!(providers.len(), 2);
    }

    #[test]
    fn a_driver_that_says_nothing_registers_nothing() {
        assert!(parse_provider_list("HTTP/1.1 500 Internal Server Error\r\n\r\n").is_empty());
        assert!(parse_provider_list("not http at all").is_empty());
    }

    #[test]
    fn a_driver_that_is_not_there_has_no_providers() {
        assert!(driver_providers("http://127.0.0.1:1").is_empty());
    }
}
