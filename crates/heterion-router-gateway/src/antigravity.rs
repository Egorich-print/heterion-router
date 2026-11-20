//! Antigravity (Google AI Code / Cloud Code) upstream executor.
//!
//! Antigravity is Google's IDE. Its language server talks to the Cloud Code
//! "PA" endpoint with the signed-in user's own Google OAuth credential, and
//! that backend serves models the public Gemini API does not (Claude Sonnet
//! 4.6, GPT-OSS 120B). This executor speaks the same protocol, so the router
//! can serve those models next to its own combos instead of hiding them.
//!
//! The protocol was measured against the shipped IDE; the notes live in
//! `docs/research/ANTIGRAVITY-CODEASSIST-PROTOCOL-2026-11-21.md`.
//!
//! # Where the credential comes from
//!
//! The user's token is **never** stored in this repository. It is read at
//! runtime from the macOS keychain item the Antigravity IDE writes
//! (service `gemini`, account `antigravity`), and the OAuth client the IDE
//! signs in with is read from the installed IDE. Both stay outside the tree
//! on purpose: a token committed here would be a token leaked to every
//! checkout of the project, and the IDE already keeps the credential in a
//! place only the user can read.
//!
//! The wire translation is shared with the Gemini executor: the Cloud Code
//! request body *is* a Gemini request body, so `openai_to_gemini` builds it
//! and `convert_event` reads the answer back.

use async_trait::async_trait;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use heterion_router_http::HttpClient;
use heterion_router_translate::gemini::{GeminiState, convert_event};
use heterion_router_translate::gemini_request::openai_to_gemini;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream, openai_request_body};
use crate::ids;

/// Cloud Code "PA" host the Antigravity language server talks to.
///
/// The IDE passes it as `--cloud_code_endpoint`; the "daily" host is the one
/// the account's quota buckets live on.
pub const DEFAULT_BASE_URL: &str = "https://daily-cloudcode-pa.googleapis.com";

/// Companion project every Antigravity request is billed to.
///
/// The value comes from the `loadCodeAssist` answer, which names the project
/// the signed-in account may use.
pub const DEFAULT_PROJECT: &str = "aicode-consumers";

/// Keychain service the Antigravity IDE stores its credential under.
pub const KEYCHAIN_SERVICE: &str = "gemini";

/// Keychain account the Antigravity IDE stores its credential under.
pub const KEYCHAIN_ACCOUNT: &str = "antigravity";

/// OAuth client id Antigravity signs in with.
///
/// A client *id* is a public identifier — it ships in the IDE bundle and
/// appears on the wire — so naming it here is no leak. The matching client
/// *secret* is a secret and is therefore read from the installed IDE at
/// runtime instead of being committed.
pub const OAUTH_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";

/// Google OAuth token endpoint used to renew an expired bearer.
pub const OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// IDE version the user agent was measured with.
///
/// The platform uses the user agent for telemetry only; a stale version there
/// costs nothing, while inventing one would misrepresent the client.
pub const ANTIGRAVITY_VERSION: &str = "2.32.1";

/// String that sits right before the OAuth client secret in the IDE binary.
///
/// The language server keeps its string constants in one blob, so the secret
/// is the `GOCSPX-` token that follows this Gemini API path.
const CLIENT_SECRET_MARKER: &str = "/v1beta/models/%s:generateContent";

/// Length of a Google OAuth client secret.
///
/// A Google client secret is `GOCSPX-` plus 28 characters. The IDE's string
/// table runs the next constant straight on, so the raw token read from the
/// binary is longer than the secret and has to be cut to its real length.
const CLIENT_SECRET_LENGTH: usize = 35;

/// How many bytes of the IDE binary are read per scan step.
///
/// The binary is ~200 MB; reading it in blocks keeps the router's resident
/// size flat while the credential is looked up.
const SCAN_BLOCK: usize = 1024 * 1024;

/// How much of the previous block is kept while scanning.
///
/// The needle may straddle a block boundary, so the tail of the previous
/// block is carried over.
const SCAN_OVERLAP: usize = 256;

/// Why the Antigravity credential could not be obtained.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// The macOS keychain holds no Antigravity item.
    #[error("antigravity: no keychain item {KEYCHAIN_SERVICE}/{KEYCHAIN_ACCOUNT}")]
    KeychainMissing,
    /// The keychain item could not be read (no `security` CLI, locked keychain).
    #[error("antigravity: keychain read failed: {0}")]
    KeychainRead(String),
    /// The stored item is not the credential document Antigravity writes.
    #[error("antigravity: keychain item is not a credential document")]
    Malformed,
    /// The Antigravity installation was not found where it is expected.
    #[error("antigravity: installation not found at {0}")]
    InstallationMissing(String),
    /// The IDE binary did not contain the OAuth client secret.
    #[error("antigravity: the Antigravity IDE binary holds no OAuth client secret")]
    ClientSecretMissing,
}

/// One Antigravity credential: the bearer plus the refresh token that renews it.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential {
    /// Bearer token for the Cloud Code endpoint.
    pub token: String,
    /// OAuth refresh token; `None` when the credential cannot be renewed.
    pub refresh_token: Option<String>,
}

/// The credential is a secret, so its `Debug` says what it is without saying
/// what it holds: a printed credential would put the bearer and the refresh
/// token into a log line.
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// The OAuth client Antigravity signs in with.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthClient {
    /// Public client identifier.
    pub client_id: String,
    /// Client secret; read from the installed IDE, never from this repository.
    pub client_secret: String,
}

/// The client secret is a secret: the `Debug` of the client names the client
/// and hides the secret.
impl std::fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClient")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

/// The `User-Agent` the Antigravity language server sends.
///
/// The platform groups traffic by it, so a request that pretends to be
/// another client would land in the wrong bucket.
fn user_agent() -> String {
    format!(
        "antigravity/hub/{ANTIGRAVITY_VERSION} (aidev_client; os_type={}; arch={}; cl={}; \
         datacollection_opt_in=false)",
        std::env::consts::OS,
        std::env::consts::ARCH,
        "1021296516",
    )
}

/// Decode standard or URL-safe base64.
///
/// The workspace has no base64 dependency and the credential blob is the only
/// thing that needs one, so the alphabet is spelled out here instead of
/// pulling a crate into every build of the gateway.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [255u8; 256];
    for (index, byte) in ALPHABET.iter().enumerate() {
        table[*byte as usize] = index as u8;
    }
    // URL-safe base64 swaps the last two symbols of the alphabet.
    table[b'-' as usize] = 62;
    table[b'_' as usize] = 63;

    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in input.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let value = table[byte as usize];
        if value == 255 {
            return None;
        }
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xFF) as u8);
            // Keep only the bits that did not belong to the emitted byte.
            buffer &= (1u32 << bits) - 1;
        }
    }
    Some(out)
}

/// Read the credential the Antigravity IDE stored in the macOS keychain.
///
/// The IDE keeps one generic password under service `gemini`, account
/// `antigravity`. Its value is a short prefix followed by a colon and the
/// base64 of a JSON credential document; the document carries the bearer, the
/// refresh token and the expiry.
///
/// The token is returned to the caller and never logged, never written to the
/// repository and never included in an error message.
pub fn read_keychain_credential() -> Result<Credential, CredentialError> {
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
            "-w",
        ])
        .output()
        .map_err(|error| CredentialError::KeychainRead(error.to_string()))?;
    if !output.status.success() {
        return Err(CredentialError::KeychainMissing);
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let Some((_prefix, payload)) = raw.split_once(':') else {
        return Err(CredentialError::Malformed);
    };
    let decoded = base64_decode(payload).ok_or(CredentialError::Malformed)?;
    let document: Value =
        serde_json::from_slice(&decoded).map_err(|_| CredentialError::Malformed)?;
    let token = document
        .pointer("/token/access_token")
        .and_then(Value::as_str)
        .ok_or(CredentialError::Malformed)?;
    let refresh_token = document
        .pointer("/token/refresh_token")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(Credential {
        token: token.to_string(),
        refresh_token,
    })
}

/// Path of the Antigravity language server binary.
///
/// The override exists so a machine with the IDE somewhere else (or a test)
/// can point the router at it without patching the source.
fn language_server_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("HETERION_ROUTER_ANTIGRAVITY_LANGUAGE_SERVER") {
        return std::path::PathBuf::from(path);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home)
        .join("Applications/Antigravity.app/Contents/Resources/bin/language_server")
}

/// Read the OAuth client secret out of the installed IDE.
///
/// The secret is a property of the shipped IDE, not of the user, but it is
/// still a secret — so it is read from the installation at runtime instead of
/// being committed. The scan looks for the Gemini API path that sits right
/// before it in the binary's string table.
fn read_client_secret(path: &std::path::Path) -> Result<String, CredentialError> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|_| CredentialError::InstallationMissing(path.display().to_string()))?;
    let mut block = vec![0u8; SCAN_BLOCK + SCAN_OVERLAP];
    let mut carried = 0usize;
    let marker = CLIENT_SECRET_MARKER.as_bytes();

    loop {
        let read = file
            .read(&mut block[carried..])
            .map_err(|error| CredentialError::InstallationMissing(error.to_string()))?;
        if read == 0 {
            return Err(CredentialError::ClientSecretMissing);
        }
        let window = &block[..carried + read];
        if let Some(position) = memchr::memmem::find(window, marker) {
            let tail = &window[position + marker.len()..];
            if let Some(start) = tail.windows(7).position(|window| window == b"GOCSPX-") {
                let secret = &tail[start..];
                let length = secret
                    .iter()
                    .position(|byte| {
                        !byte.is_ascii_alphanumeric() && *byte != b'-' && *byte != b'_'
                    })
                    .unwrap_or(secret.len())
                    .min(CLIENT_SECRET_LENGTH);
                if length >= CLIENT_SECRET_LENGTH {
                    return Ok(String::from_utf8_lossy(&secret[..length]).to_string());
                }
            }
            return Err(CredentialError::ClientSecretMissing);
        }
        // Carry the tail over so a needle across a block boundary is found.
        let window_len = window.len();
        carried = SCAN_OVERLAP.min(window_len);
        let start = window_len - carried;
        block.copy_within(start..window_len, 0);
    }
}

/// Find the OAuth client Antigravity signs in with.
///
/// The client id is public and named in [`OAUTH_CLIENT_ID`]; the secret is
/// read from the installed IDE by [`read_client_secret`].
pub fn discover_oauth_client() -> Result<OAuthClient, CredentialError> {
    let path = language_server_path();
    let client_secret = read_client_secret(&path)?;
    Ok(OAuthClient {
        client_id: OAUTH_CLIENT_ID.to_string(),
        client_secret,
    })
}

/// Build the Cloud Code request body for one chat turn.
///
/// The body is the Cloud Code envelope — `project`, `requestId` and the inner
/// request — around a Gemini request body, which the shared translator
/// already knows how to produce.
pub fn build_request_body(request: &ChatCompletionRequest, model: &str, project: &str) -> Value {
    let mut inner = openai_to_gemini(&openai_request_body(request));
    // The Gemini API carries the model in the URL; the Cloud Code API
    // carries it in the request body instead.
    inner["model"] = json!(model);
    json!({
        "project": project,
        "requestId": ids::call_id(),
        "request": inner,
    })
}

/// Collapse translated OpenAI chunks into one completion.
///
/// The Gemini executor does the same aggregation, but it also mints and
/// replays Gemini thought signatures, which the Cloud Code response carries
/// but the Antigravity models do not require — so the two stay apart.
fn collapse(chunks: &[Value]) -> (String, String, Vec<Value>, String) {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut finish = "stop".to_string();
    for chunk in chunks {
        let choice = &chunk["choices"][0];
        let delta = &choice["delta"];
        if let Some(part) = delta.get("content").and_then(Value::as_str) {
            text.push_str(part);
        }
        if let Some(part) = delta.get("reasoning_content").and_then(Value::as_str) {
            reasoning.push_str(part);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            tool_calls.extend(calls.iter().cloned());
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            finish = reason.to_string();
        }
    }
    (text, reasoning, tool_calls, finish)
}

/// Convert a Cloud Code answer into an OpenAI completion.
pub fn completion_from_payload(model: &str, payload: &Value) -> ChatCompletionResponse {
    let mut state = GeminiState::default();
    let chunks = convert_event(&mut state, payload, ids::unix_seconds());
    let usage = state.usage.unwrap_or_default();
    let (text, reasoning, tool_calls, finish) = collapse(&chunks);
    let message = ChatMessage {
        role: "assistant".to_string(),
        content: (!text.is_empty()).then(|| Value::from(text)),
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
        tool_calls: (!tool_calls.is_empty()).then_some(Value::Array(tool_calls)),
        tool_call_id: None,
        name: None,
    };
    ChatCompletionResponse {
        id: format!("chatcmpl-{}", ids::unix_seconds()),
        object: "chat.completion",
        created: ids::unix_seconds(),
        model: model.to_string(),
        choices: vec![ChatChoice {
            index: 0,
            message,
            finish_reason: finish,
        }],
        usage: Usage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        },
        // Handed over so the accounting layer reads the same usage block the
        // streaming path reads.
        raw: Some(payload.clone()),
    }
}

/// Antigravity executor over HTTP.
#[derive(Clone)]
pub struct AntigravityBackend {
    http: HttpClient,
    base_url: String,
    project: String,
    credentials: Vec<Credential>,
    next: Arc<std::sync::atomic::AtomicUsize>,
    oauth_client: Arc<std::sync::OnceLock<Option<OAuthClient>>>,
}

/// The backend holds the credentials, so its `Debug` names them and hides
/// them: a printed backend would put a bearer and a client secret into a log
/// line.
impl std::fmt::Debug for AntigravityBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AntigravityBackend")
            .field("base_url", &self.base_url)
            .field("project", &self.project)
            .field("credentials", &self.credentials.len())
            .finish()
    }
}

impl AntigravityBackend {
    /// Build the executor for one Cloud Code host.
    ///
    /// `oauth_client` is optional: without it an expired bearer is renewed
    /// with the client the installed Antigravity IDE uses, discovered at the
    /// moment a renewal is actually needed — the discovery reads the IDE
    /// binary, and a gateway that never serves an Antigravity request should
    /// not pay for that.
    pub fn new(
        base_url: String,
        project: String,
        credentials: Vec<Credential>,
        oauth_client: Option<OAuthClient>,
    ) -> Result<Self, heterion_router_http::client::HttpError> {
        let slot = std::sync::OnceLock::new();
        if let Some(client) = oauth_client {
            let _ = slot.set(Some(client));
        }
        Ok(Self {
            http: HttpClient::new(600)?,
            base_url: base_url.trim_end_matches('/').to_string(),
            project,
            credentials,
            next: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            oauth_client: Arc::new(slot),
        })
    }

    /// Number of credentials in the rotation pool.
    pub fn credential_count(&self) -> usize {
        self.credentials.len()
    }

    /// The next credential of the rotation pool.
    ///
    /// An empty pool is an error rather than a panic: a provider that lost
    /// its credential must say so instead of crashing the gateway.
    fn next_credential(&self) -> Option<&Credential> {
        if self.credentials.is_empty() {
            return None;
        }
        let index =
            self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.credentials.len();
        self.credentials.get(index)
    }

    /// The Cloud Code URL one method is served at.
    fn method_url(&self, method: &str, stream: bool) -> String {
        let suffix = if stream { "?alt=sse" } else { "" };
        format!("{}/v1internal:{method}{suffix}", self.base_url)
    }

    /// Headers every Cloud Code call carries.
    ///
    /// The values are owned so the borrow outlives the request builder.
    fn headers(&self, token: &str) -> Vec<(String, String)> {
        vec![
            ("authorization".to_string(), format!("Bearer {token}")),
            ("user-agent".to_string(), user_agent()),
        ]
    }

    /// Post one Cloud Code request and return the raw response.
    async fn post(
        &self,
        url: &str,
        body: &Value,
        token: &str,
    ) -> Result<reqwest::Response, heterion_router_http::client::HttpError> {
        let owned = self.headers(token);
        let headers: Vec<(&str, &str)> = owned
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        self.http
            .post_json_with_headers(url, None, &headers, body)
            .await
    }

    /// The OAuth client used to renew a bearer.
    ///
    /// A configured client wins; otherwise the installed IDE is asked, once.
    /// A failed discovery is not cached: the IDE may be installed later, and
    /// the next renewal deserves a fresh attempt.
    fn oauth_client(&self) -> Option<OAuthClient> {
        if let Some(client) = self.oauth_client.get() {
            return client.clone();
        }
        let discovered = discover_oauth_client().ok();
        if discovered.is_some() {
            let _ = self.oauth_client.set(discovered.clone());
        }
        discovered
    }

    /// Renew an expired bearer through the Google OAuth token endpoint.
    ///
    /// A refresh failure is reported without the token in the message: the
    /// secret must not reach a log line.
    async fn refresh(&self, credential: &Credential) -> Result<Credential, GatewayError> {
        let Some(client) = self.oauth_client() else {
            return Err(GatewayError::Upstream(
                "upstream status 401: antigravity token expired and no OAuth client is \
                 configured to renew it"
                    .to_string(),
            ));
        };
        let Some(refresh_token) = credential.refresh_token.as_deref() else {
            return Err(GatewayError::Upstream(
                "upstream status 401: antigravity token expired and no refresh token is \
                 available"
                    .to_string(),
            ));
        };
        let fields = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
        ];
        let response = self
            .http
            .post_form(OAUTH_TOKEN_URL, &fields)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;
        let payload: Value = response
            .json()
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;
        let token = payload
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                GatewayError::Upstream(
                    "upstream status 401: antigravity token refresh returned no access token"
                        .to_string(),
                )
            })?;
        let refreshed = payload
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| credential.refresh_token.clone());
        Ok(Credential {
            token: token.to_string(),
            refresh_token: refreshed,
        })
    }

    /// Call the upstream once, renewing the credential when it was refused.
    async fn call(&self, url: &str, body: &Value) -> Result<Value, GatewayError> {
        let Some(current) = self.next_credential().cloned() else {
            return Err(GatewayError::Upstream(
                "antigravity: no credential is configured".to_string(),
            ));
        };
        match self.post(url, body, &current.token).await {
            Ok(response) => response
                .json()
                .await
                .map_err(|error| GatewayError::Upstream(error.to_string())),
            Err(error) => {
                let message = error.to_string();
                if !message.contains("upstream status 401") {
                    return Err(GatewayError::Upstream(message));
                }
                // The bearer expired: renew it and answer with the new one.
                let refreshed = self.refresh(&current).await?;
                let response = self
                    .post(url, body, &refreshed.token)
                    .await
                    .map_err(|error| GatewayError::Upstream(error.to_string()))?;
                response
                    .json()
                    .await
                    .map_err(|error| GatewayError::Upstream(error.to_string()))
            }
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
        let model = request.model.clone();
        let body = build_request_body(&request, &model, &self.project);
        let url = self.method_url("streamGenerateContent", true);
        let credential = self.next_credential().cloned().ok_or_else(|| {
            GatewayError::Upstream("antigravity: no credential is configured".to_string())
        })?;
        let response = self
            .post(&url, &body, &credential.token)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;

        let mut translator = GeminiState::default();
        crate::responses::pump_sse_stream(
            response,
            &tx,
            |_event, data| {
                let chunks = convert_event(&mut translator, data, ids::unix_seconds());
                if let Some(error) = translator.upstream_error.take() {
                    return Err(GatewayError::Upstream(error));
                }
                Ok(chunks)
            },
            // The Cloud Code stream ends with a `finishReason`; a body that
            // ends without one never completed the answer.
            crate::responses::MissingTerminal::Fail,
        )
        .await
    }
}

#[async_trait]
impl ChatBackend for AntigravityBackend {
    fn name(&self) -> &'static str {
        "antigravity"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        let model = request.model.clone();
        let body = build_request_body(&request, &model, &self.project);
        let url = self.method_url("generateContent", false);
        let payload = self.call(&url, &body).await?;
        Ok(completion_from_payload(&model, &payload))
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

/// Shared handle the gateway keeps for the Antigravity executor.
pub type SharedBackend = Arc<AntigravityBackend>;

#[cfg(test)]
mod tests {
    use super::*;
    use heterion_router_core::ChatMessage;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gemini-3.8-flash-high".to_string(),
            messages: vec![ChatMessage::plain("user", "hi")],
            stream: false,
            ..Default::default()
        }
    }

    fn backend(server: &MockServer) -> AntigravityBackend {
        AntigravityBackend::new(
            server.uri(),
            DEFAULT_PROJECT.to_string(),
            vec![Credential {
                token: "tok".to_string(),
                refresh_token: None,
            }],
            None,
        )
        .unwrap()
    }

    #[test]
    fn base64_decodes_both_alphabets() {
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert_eq!(base64_decode("aGk").unwrap(), b"hi");
        // The URL-safe alphabet swaps `+` and `/` for `-` and `_`.
        assert_eq!(base64_decode("----").unwrap(), vec![0xFB, 0xEF, 0xBE]);
        assert_eq!(base64_decode("++++").unwrap(), vec![0xFB, 0xEF, 0xBE]);
        assert!(base64_decode("***").is_none());
    }

    #[test]
    fn the_request_body_is_the_cloud_code_envelope() {
        let body = build_request_body(&request(), "gemini-3.8-flash-high", DEFAULT_PROJECT);
        assert_eq!(body["project"], json!(DEFAULT_PROJECT));
        assert!(body["requestId"].as_str().is_some());
        assert_eq!(body["request"]["model"], json!("gemini-3.8-flash-high"));
        assert_eq!(body["request"]["contents"][0]["role"], json!("user"));
        assert_eq!(
            body["request"]["contents"][0]["parts"][0]["text"],
            json!("hi")
        );
    }

    #[test]
    fn a_system_prompt_reaches_the_inner_request() {
        let mut request = request();
        request
            .messages
            .insert(0, ChatMessage::plain("system", "be terse"));
        let body = build_request_body(&request, "gemini-3.8-flash-high", DEFAULT_PROJECT);
        assert_eq!(
            body["request"]["systemInstruction"]["parts"][0]["text"],
            json!("be terse")
        );
    }

    #[tokio::test]
    async fn completes_through_generate_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:generateContent"))
            .and(header("authorization", "Bearer tok"))
            .and(body_partial_json(json!({"project": DEFAULT_PROJECT})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": {
                    "candidates": [{
                        "content": {"role": "model", "parts": [{"text": "PONG"}]},
                        "finishReason": "STOP"
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 3,
                        "candidatesTokenCount": 2,
                        "totalTokenCount": 5
                    }
                },
                "traceId": "t1"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let response = backend(&server).complete(request()).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "PONG");
        assert_eq!(response.choices[0].finish_reason, "stop");
        assert_eq!(response.usage.total_tokens, 5);
        server.verify().await;
    }

    #[tokio::test]
    async fn a_tool_call_round_trips() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": {
                    "candidates": [{
                        "content": {"role": "model", "parts": [
                            {"functionCall": {"name": "read_file", "args": {"path": "/etc/hosts"}}}
                        ]},
                        "finishReason": "STOP"
                    }],
                    "usageMetadata": {"totalTokenCount": 9}
                }
            })))
            .mount(&server)
            .await;

        let response = backend(&server).complete(request()).await.unwrap();
        let choice = &response.choices[0];
        assert_eq!(choice.finish_reason, "tool_calls");
        let calls = choice.message.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0]["function"]["name"], json!("read_file"));
        assert!(
            calls[0]["function"]["arguments"]
                .as_str()
                .unwrap()
                .contains("/etc/hosts"),
            "the tool arguments must survive the round trip"
        );
    }

    #[tokio::test]
    async fn a_429_keeps_its_status_and_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:generateContent"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "30")
                    .set_body_string("quota exhausted"),
            )
            .mount(&server)
            .await;

        let error = backend(&server).complete(request()).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("upstream status 429"), "{message}");
        assert!(message.contains("retry-after: 30s"), "{message}");
    }

    #[tokio::test]
    async fn a_403_keeps_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:generateContent"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let error = backend(&server).complete(request()).await.unwrap_err();
        assert!(error.to_string().contains("upstream status 403"), "{error}");
    }

    #[tokio::test]
    async fn streams_a_cloud_code_answer() {
        let server = MockServer::start().await;
        let sse = concat!(
            "data: {\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\
             \"parts\":[{\"text\":\"he\"}]}}]}}\n\n",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\
             \"parts\":[{\"text\":\"llo\"}]},\"finishReason\":\"STOP\"}],\
             \"usageMetadata\":{\"totalTokenCount\":4}}}\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse),
            )
            .mount(&server)
            .await;

        let mut request = request();
        request.stream = true;
        use futures::StreamExt;
        let mut stream = backend(&server).stream(request);
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
        assert_eq!(text, "hello");
        assert_eq!(finish.as_deref(), Some("stop"));
    }
}
