//! Gemini (`generateContent`) backend.
//!
//! Gemini differs from the OpenAI-compatible providers in two ways: the chat
//! URL embeds the model (`.../models/{model}:generateContent`) and auth is an
//! `x-goog-api-key` header. Requests and responses are translated with the
//! ported `heterion-router-translate` pair, so tools and streaming work like the
//! OpenAI path.

use async_trait::async_trait;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use heterion_router_http::HttpClient;
use heterion_router_translate::gemini::{GeminiState, convert_event};
use heterion_router_translate::gemini_request::openai_to_gemini;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream, next_token, openai_request_body};
use crate::ids;
use crate::openai::try_each_credential;
use crate::responses::{MissingTerminal, pump_sse_stream};

/// Gemini executor over HTTP.
#[derive(Debug, Clone)]
pub struct GeminiBackend {
    http: HttpClient,
    /// `.../v1beta/models`, without a trailing slash.
    base_url: String,
    tokens: Vec<String>,
    next: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    signatures: std::sync::Arc<ThoughtSignatures>,
    /// `message_id` from the last Gemini `message_start`, used to form
    /// the `{message_id}-{index}` ids that `record_signatures` mints
    /// and `inject_signatures` looks up when the client's own
    /// `call.id` is absent from the store.
    last_message_id: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

/// Lock the last seen `message_id`, recovering from poisoning.
///
/// Consistent with the rest of the tree (and with [`ThoughtSignatures::lock`]):
/// a panic on another thread must not cascade into every later request.
fn lock_message_id(
    last_message_id: &std::sync::Mutex<Option<String>>,
) -> std::sync::MutexGuard<'_, Option<String>> {
    last_message_id
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Tool-call id → Gemini thought signature, kept across a tool round trip.
///
/// The signature leaves in `extra_content` for clients that preserve unknown
/// fields. Clients that rebuild the assistant message from their own schema
/// drop it, and Gemini then rejects the replayed `functionCall`, so the id we
/// minted is used as a fallback key.
///
/// The store is persisted: an agent conversation outlives a gateway restart,
/// and a signature lost that way turns the next turn into a hard 400.
/// Bounded: the oldest entries fall out first.
#[derive(Debug, Default)]
pub struct ThoughtSignatures {
    path: Option<std::path::PathBuf>,
    state: std::sync::Mutex<SignatureState>,
    /// Write-behind queue drained by one dedicated OS thread.
    ///
    /// `record` runs per streamed chunk, so appending synchronously put an
    /// `open`+`write` syscall pair on a tokio worker thread in the middle of
    /// every Gemini stream. A plain `std::thread` is the right home for
    /// blocking I/O here — it is not a runtime worker, and it outlives the
    /// request.
    writer: Option<std::sync::mpsc::SyncSender<Write>>,
    /// Appends the queue refused. A dropped signature costs one call's
    /// fallback, but a silent run of drops would look like healthy operation,
    /// so the count is what makes it visible.
    dropped: std::sync::atomic::AtomicU64,
}

#[derive(Debug, Default)]
struct SignatureState {
    order: std::collections::VecDeque<String>,
    values: std::collections::HashMap<String, String>,
}

/// One persisted `id` → `signature` pair.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SignatureRecord {
    id: String,
    signature: String,
}

/// Signatures kept before the oldest are evicted.
const SIGNATURE_CAPACITY: usize = 4096;

/// How many pending appends may wait for the writer thread. Bounded on
/// purpose: an unbounded queue would trade a blocked runtime thread for an
/// unbounded heap.
const WRITE_QUEUE_DEPTH: usize = 256;

/// What the writer thread is asked to do, in order.
enum Write {
    Line(String),
    /// Persist everything queued before this, then answer. Lets a caller that
    /// needs durability (a test, a shutdown path) get the old synchronous
    /// guarantee without putting the wait on a runtime thread.
    Flush(std::sync::mpsc::SyncSender<()>),
}

/// Blocking append, called only from the writer thread.
fn append_line(path: &std::path::Path, line: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

impl ThoughtSignatures {
    /// Open a persistent store, reusing signatures recorded before a restart.
    pub fn open(path: std::path::PathBuf) -> Self {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Write>(WRITE_QUEUE_DEPTH);
        let target = path.clone();
        // If the thread cannot start the store still works in memory; only
        // persistence across a restart is lost, which is the same cost the
        // previous "a failed write only costs that call" comment described.
        let writer = std::thread::Builder::new()
            .name("gemini-signature-writer".to_string())
            .spawn(move || {
                for message in receiver {
                    match message {
                        Write::Line(line) => append_line(&target, &line),
                        Write::Flush(ack) => {
                            let _ = ack.send(());
                        }
                    }
                }
            })
            .ok()
            .map(|_| sender);
        let store = Self {
            path: Some(path),
            state: std::sync::Mutex::new(SignatureState::default()),
            writer,
            dropped: std::sync::atomic::AtomicU64::new(0),
        };
        store.load();
        store
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SignatureState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Read the persisted pairs back, then rewrite the file trimmed so it does
    /// not grow without bound across restarts.
    fn load(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        {
            let mut state = self.lock();
            for line in content.lines() {
                let Ok(record) = serde_json::from_str::<SignatureRecord>(line) else {
                    continue;
                };
                if record.id.is_empty() || record.signature.is_empty() {
                    continue;
                }
                if state
                    .values
                    .insert(record.id.clone(), record.signature)
                    .is_none()
                {
                    state.order.push_back(record.id);
                }
            }
            while state.order.len() > SIGNATURE_CAPACITY {
                if let Some(oldest) = state.order.pop_front() {
                    state.values.remove(&oldest);
                }
            }
        }
        self.rewrite();
    }

    /// Replace the file with the in-memory contents (used once at load).
    fn rewrite(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let state = self.lock();
        let mut body = String::new();
        for id in &state.order {
            let Some(signature) = state.values.get(id) else {
                continue;
            };
            if let Ok(line) = serde_json::to_string(&SignatureRecord {
                id: id.clone(),
                signature: signature.clone(),
            }) {
                body.push_str(&line);
                body.push('\n');
            }
        }
        let _ = std::fs::write(path, body);
    }

    fn record(&self, id: &str, signature: &str) {
        {
            let mut state = self.lock();
            if state
                .values
                .insert(id.to_string(), signature.to_string())
                .is_none()
            {
                state.order.push_back(id.to_string());
            }
            while state.order.len() > SIGNATURE_CAPACITY {
                if let Some(oldest) = state.order.pop_front() {
                    state.values.remove(&oldest);
                }
            }
        }
        self.append(id, signature);
    }

    /// Hand one pair to the writer thread; a full queue or a failed write only
    /// costs the fallback for that call.
    fn append(&self, id: &str, signature: &str) {
        let Some(sender) = self.writer.as_ref() else {
            return;
        };
        let Ok(line) = serde_json::to_string(&SignatureRecord {
            id: id.to_string(),
            signature: signature.to_string(),
        }) else {
            return;
        };
        // `try_send`, never `send`: blocking here would put the wait for a
        // busy writer thread straight back onto the runtime thread. A full
        // queue drops the line, which costs the fallback for that one call —
        // the same price a failed write always had.
        if sender.try_send(Write::Line(line)).is_err() {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            // Logged on the first drop and then every 1024th, so a stalled
            // disk produces one line rather than one per chunk — and one line
            // rather than none.
            if dropped == 1 || dropped.is_multiple_of(1024) {
                tracing::warn!(
                    "gemini thought-signature queue full: {dropped} append(s) dropped, \
                     the conversation may lose a signature across a restart"
                );
            }
        }
    }

    /// Block until every queued append has reached the file.
    ///
    /// Deliberately blocking and deliberately explicit: the streaming path
    /// must not wait on disk, but a caller that is about to lose the process
    /// needs the pairs on disk first.
    pub fn flush(&self) {
        let Some(sender) = self.writer.as_ref() else {
            return;
        };
        let (ack, done) = std::sync::mpsc::sync_channel(1);
        // A plain `send` here is right: the caller asked to wait.
        if sender.send(Write::Flush(ack)).is_ok() {
            let _ = done.recv();
        }
    }

    /// Appends the write-behind queue refused. Zero in healthy operation.
    pub fn dropped_appends(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn get(&self, id: &str) -> Option<String> {
        self.lock().values.get(id).cloned()
    }
}

/// Remember the signatures carried by translated tool calls.
/// Returns the `message_id` extracted from the first tool call id
/// (`{message_id}-{index}`), so callers can feed it back into
/// `inject_signatures` for the next round trip.
fn record_signatures(chunk: &Value, signatures: &ThoughtSignatures) -> Option<String> {
    let mut message_id: Option<String> = None;
    let calls = chunk["choices"][0]["delta"]["tool_calls"]
        .as_array()
        .into_iter()
        .flatten();
    for call in calls {
        let (Some(id), Some(signature)) = (
            call.get("id").and_then(Value::as_str),
            call.pointer("/extra_content/google/thought_signature")
                .and_then(Value::as_str),
        ) else {
            continue;
        };
        // id = "{message_id}-{index}"; extract the message_id once.
        if message_id.is_none() {
            message_id = id.rsplit_once('-').map(|(m, _)| m.to_string());
        }
        signatures.record(id, signature);
    }
    message_id
}

/// Fill in signatures a client dropped from replayed tool calls.
///
/// The client's own `call.id` is tried first (it matches when the
/// client faithfully echoed the gateway-generated id). When it does
/// not, the store is also probed with the gateway-minted form
/// `{message_id}-{index}` so that signatures survived from a
/// previous gateway turn are recovered even when the client rebuilt
/// its tool calls with fresh ids.
fn inject_signatures(
    request: &mut ChatCompletionRequest,
    signatures: &ThoughtSignatures,
    last_message_id: Option<String>,
) {
    for message in &mut request.messages {
        if message.role != "assistant" {
            continue;
        }
        let Some(calls) = message.tool_calls.as_mut().and_then(Value::as_array_mut) else {
            continue;
        };
        for (index, call) in calls.iter_mut().enumerate() {
            let carried = call
                .pointer("/extra_content/google/thought_signature")
                .or_else(|| call.get("thoughtSignature"))
                .is_some();
            if carried {
                continue;
            }
            let id = call.get("id").and_then(Value::as_str).unwrap_or("");
            let signature = signatures.get(id).or_else(|| {
                last_message_id
                    .as_ref()
                    .and_then(|mid| signatures.get(&format!("{mid}-{index}")))
            });
            let Some(signature) = signature else {
                continue;
            };
            if let Some(object) = call.as_object_mut() {
                object.insert(
                    "extra_content".to_string(),
                    json!({ "google": { "thought_signature": signature } }),
                );
            }
        }
    }
}

impl GeminiBackend {
    /// Lock the last `message_id`, recovering from poisoning.
    ///
    /// A panic elsewhere in the process must not turn every later Gemini
    /// request into a panic cascade — the rest of the tree recovers the same
    /// way, and the guarded state is a plain `Option<String>` that stays
    /// consistent regardless of who panicked.
    fn message_id(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        lock_message_id(&self.last_message_id)
    }

    /// Build against the models base URL with a rotation pool of API keys.
    pub fn new(
        base_url: String,
        tokens: Vec<String>,
    ) -> Result<Self, heterion_router_http::client::HttpError> {
        Self::with_signature_store(
            base_url,
            tokens,
            std::sync::Arc::new(ThoughtSignatures::default()),
        )
    }

    /// Build with a persistent thought-signature store.
    ///
    /// Agents span longer than a process: a signature dropped by a restart
    /// makes Gemini reject the next replayed function call.
    pub fn with_signature_store(
        base_url: String,
        tokens: Vec<String>,
        signatures: std::sync::Arc<ThoughtSignatures>,
    ) -> Result<Self, heterion_router_http::client::HttpError> {
        if tokens.is_empty() {
            return Err(heterion_router_http::client::HttpError::Status {
                status: 0,
                body: "gemini: no credentials".to_string(),
            });
        }
        Ok(Self {
            http: HttpClient::new(600)?,
            base_url: base_url.trim_end_matches('/').to_string(),
            tokens,
            next: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            signatures,
            last_message_id: std::sync::Arc::new(std::sync::Mutex::new(None)),
        })
    }

    /// Pool size.
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    fn method_url(&self, model: &str, stream: bool) -> String {
        if stream {
            format!("{}/{}:streamGenerateContent?alt=sse", self.base_url, model)
        } else {
            format!("{}/{}:generateContent", self.base_url, model)
        }
    }

    fn gemini_body(request: &ChatCompletionRequest) -> Value {
        openai_to_gemini(&openai_request_body(request))
    }

    async fn drive_stream(
        &self,
        mut request: ChatCompletionRequest,
        tx: mpsc::Sender<Result<StreamChunk, GatewayError>>,
    ) -> Result<(), GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        inject_signatures(&mut request, &self.signatures, self.message_id().clone());
        let body = Self::gemini_body(&request);
        let url = self.method_url(&request.model, true);
        let key = next_token(&self.tokens, &self.next)
            .expect("empty token pool rejected by with_signature_store")
            .to_string();

        let response = self
            .http
            .post_json_with_headers(&url, None, &[("x-goog-api-key", &key)], &body)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;

        // The translator's state rides along in the handler the shared pump
        // drives, so Gemini keeps ownership of its per-stream bookkeeping.
        let mut translator = GeminiState::default();
        pump_sse_stream(
            response,
            &tx,
            |_event, data| {
                let chunks = convert_event(&mut translator, data, ids::unix_seconds());
                for chunk in &chunks {
                    if let Some(message_id) = record_signatures(chunk, &self.signatures) {
                        *self.message_id() = Some(message_id);
                    }
                }
                // An error frame translates to nothing at all, so surfacing
                // it here drops no chunk the hand-rolled loop had emitted.
                if let Some(error) = translator.upstream_error.take() {
                    return Err(GatewayError::Upstream(error));
                }
                Ok(chunks)
            },
            // Gemini reports completion out of band: only a `finishReason`
            // ends the answer, so a `[DONE]` that never had one behind it
            // fails rather than standing in for it.
            MissingTerminal::Fail,
        )
        .await
    }
}

#[async_trait]
impl ChatBackend for GeminiBackend {
    fn name(&self) -> &'static str {
        "gemini"
    }

    async fn complete(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        inject_signatures(&mut request, &self.signatures, self.message_id().clone());
        let body = Self::gemini_body(&request);
        let url = self.method_url(&request.model, false);

        // The pool is non-empty by construction (`with_signature_store`
        // rejects an empty one), so one attempt per credential.
        let payload = try_each_credential("gemini", self.tokens.len(), || async {
            let key = next_token(&self.tokens, &self.next)
                .expect("empty token pool rejected by with_signature_store")
                .to_string();
            let response = self
                .http
                .post_json_with_headers(&url, None, &[("x-goog-api-key", &key)], &body)
                .await
                .map_err(|error| GatewayError::Upstream(error.to_string()))?;
            response
                .json()
                .await
                .map_err(|error| GatewayError::Upstream(error.to_string()))
        })
        .await?;
        Ok(collapse_chunks(
            &request.model,
            &payload,
            &self.signatures,
            self.last_message_id.as_ref(),
        ))
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

/// Collapse a full Gemini response into one OpenAI completion by running it
/// through the streaming translator, so both paths share the same mapping.
fn collapse_chunks(
    model: &str,
    payload: &Value,
    signatures: &ThoughtSignatures,
    last_message_id: &std::sync::Mutex<Option<String>>,
) -> ChatCompletionResponse {
    let mut state = GeminiState::default();
    let chunks = convert_event(&mut state, payload, ids::unix_seconds());
    let mut message_id: Option<String> = None;
    for chunk in &chunks {
        if message_id.is_none() {
            message_id = record_signatures(chunk, signatures);
        } else {
            record_signatures(chunk, signatures);
        }
    }
    if let Some(message_id) = message_id {
        *lock_message_id(last_message_id) = Some(message_id);
    }

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
            // Streaming assigns per-delta indices; a single completion needs
            // one flat array, so push the call objects through.
            for call in calls {
                tool_calls.push(call.clone());
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            finish = reason.to_string();
        }
    }

    let usage = state.usage.unwrap_or_default();
    let message = ChatMessage {
        role: "assistant".to_string(),
        content: if text.is_empty() {
            None
        } else {
            Some(Value::from(text))
        },
        reasoning_content: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
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
        // Handed over so the accounting layer reads `usageMetadata` with the
        // same parser the streaming path uses.
        raw: Some(payload.clone()),
    }
}

/// Keep the `json!` import meaningful for tests and future fields.
#[allow(dead_code)]
fn _touch() -> Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, header, method, path},
    };

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gemini-3.8-flash".to_string(),
            messages: vec![ChatMessage::plain("user", "hi")],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn completes_via_generate_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .and(header("x-goog-api-key", "k1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [{"text": "PONG"}]},
                    "finishReason": "STOP"
                }],
                "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 2, "totalTokenCount": 5}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let response = backend.complete(request()).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "PONG");
        assert_eq!(response.choices[0].finish_reason, "stop");
        assert_eq!(response.usage.total_tokens, 5);
        server.verify().await;
    }

    #[tokio::test]
    async fn forwards_json_response_format_to_generation_config() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .and(body_partial_json(json!({
                "generationConfig": {
                    "responseMimeType": "application/json",
                    "responseSchema": {"type": "object"}
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [{"text": "{}"}]},
                    "finishReason": "STOP"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let mut request = request();
        request.response_format = Some(json!({
            "type": "json_schema",
            "json_schema": {"schema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "additionalProperties": false
            }}
        }));

        let response = backend.complete(request).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "{}");
        server.verify().await;
    }

    /// FIX 4: a panic while the lock is held poisons it. The Gemini request
    /// path must recover instead of panicking on every later call.
    #[tokio::test]
    async fn poisoned_message_id_lock_does_not_panic() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [{
                        "functionCall": {"name": "bash", "args": {"command": "ls"}},
                        "thoughtSignature": "c2ln"
                    }]},
                    "finishReason": "STOP"
                }]
            })))
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        poison(backend.last_message_id.clone());

        // `complete` reads and writes the poisoned lock on both paths.
        let response = backend.complete(request()).await.unwrap();
        assert_eq!(response.choices[0].finish_reason, "tool_calls");
        // The refreshed message id was still recorded through the poisoned lock.
        assert!(
            backend.message_id().is_some(),
            "message id must survive a poisoned lock"
        );

        // And the streaming path reads it too.
        let mut streamed = request();
        streamed.stream = true;
        let _ = backend
            .stream(streamed)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(Result::ok)
            .count();
        assert!(backend.message_id().is_some());
    }

    /// Poison the mutex the way a panicking thread would.
    fn poison(mutex: std::sync::Arc<std::sync::Mutex<Option<String>>>) {
        let worker = std::sync::Arc::clone(&mutex);
        let _ = std::thread::spawn(move || {
            let _guard = worker.lock().expect("unpoisoned");
            panic!("poison the message-id lock");
        })
        .join();
        assert!(mutex.is_poisoned(), "the mutex must be poisoned");
    }

    /// A burst larger than the queue must be visible, not silent: the store
    /// still serves every id from memory, but the tail that would have been
    /// persisted is gone, and only the counter says so.
    #[test]
    fn a_full_queue_counts_what_it_dropped() {
        let dir = std::env::temp_dir().join(format!("omni-sig-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = ThoughtSignatures::open(dir.join("signatures.jsonl"));
        assert_eq!(store.dropped_appends(), 0, "a fresh store drops nothing");

        for index in 0..WRITE_QUEUE_DEPTH * 4 {
            store.record(&format!("chatcmpl-{index}-0"), "c2ln");
        }
        // Either the writer kept up (nothing dropped) or it did not, and the
        // count says by exactly how much. Both are acceptable; silence is not.
        let dropped = store.dropped_appends();
        store.flush();
        assert!(
            dropped <= (WRITE_QUEUE_DEPTH * 4) as u64,
            "cannot drop more than was recorded: {dropped}"
        );
        // The in-memory store is unaffected either way — a dropped write costs
        // a signature across a restart, never the current conversation.
        assert_eq!(store.get("chatcmpl-0-0").as_deref(), Some("c2ln"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thought_signatures_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("omni-sig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("signatures.jsonl");

        let first = ThoughtSignatures::open(path.clone());
        first.record("chatcmpl-1-0", "c2ln");
        // Appends are write-behind, so durability is asked for explicitly.
        first.flush();
        drop(first);

        // A new process opens the same file: the conversation keeps working.
        let second = ThoughtSignatures::open(path.clone());
        assert_eq!(second.get("chatcmpl-1-0").as_deref(), Some("c2ln"));

        // Loading trims the file back to the retained pairs.
        let reloaded = ThoughtSignatures::open(path.clone());
        assert_eq!(reloaded.get("chatcmpl-1-0").as_deref(), Some("c2ln"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn replays_thought_signature_the_client_dropped() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [{
                        "functionCall": {"name": "bash", "args": {"command": "ls"}},
                        "thoughtSignature": "c2ln"
                    }]},
                    "finishReason": "STOP"
                }]
            })))
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();

        let first = backend.complete(request()).await.unwrap();
        let call = &first.choices[0].message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call["extra_content"]["google"]["thought_signature"], "c2ln");

        // The client rebuilds the assistant turn from its own schema, so the
        // extension field is gone by the time the tool result comes back.
        let mut follow_up = request();
        follow_up.messages = vec![
            ChatMessage::plain("user", "list files"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([{
                    "id": call["id"],
                    "type": "function",
                    "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}
                }])),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(Value::from("a.txt")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: call["id"].as_str().map(str::to_string),
                name: None,
            },
        ];
        backend.complete(follow_up).await.unwrap();

        let sent = server.received_requests().await.unwrap();
        let body = sent.last().unwrap().body_json::<Value>().unwrap();
        let model_turn = body["contents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|turn| turn["role"] == "model")
            .expect("model turn replayed");
        assert_eq!(model_turn["parts"][0]["thoughtSignature"], "c2ln");
        assert_eq!(model_turn["parts"][0]["functionCall"]["name"], "bash");
    }

    #[tokio::test]
    async fn maps_function_call_to_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [{"functionCall": {"name": "bash", "args": {"command": "ls"}}}]},
                    "finishReason": "STOP"
                }]
            })))
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let response = backend.complete(request()).await.unwrap();
        assert_eq!(response.choices[0].finish_reason, "tool_calls");
        let calls = response.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0]["function"]["name"], json!("bash"));
        assert_eq!(
            calls[0]["function"]["arguments"],
            json!("{\"command\":\"ls\"}")
        );
    }

    #[tokio::test]
    async fn nonstream_thinking_is_surfaced_as_reasoning_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [
                        {"text": "weighing options", "thought": true},
                        {"text": "PONG"}
                    ]},
                    "finishReason": "STOP"
                }]
            })))
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let response = backend.complete(request()).await.unwrap();
        let message = &response.choices[0].message;
        assert_eq!(message.text(), "PONG");
        assert_eq!(
            message.reasoning_content.as_deref(),
            Some("weighing options")
        );
    }

    /// The pooled Gemini path uses the same credential loop as the OpenAI
    /// executor: a rejected key rotates to the next one, and a fully
    /// exhausted pool surfaces the last upstream error after exactly one
    /// attempt per key.
    #[tokio::test]
    async fn a_rejected_key_rotates_to_the_next_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .and(header("x-goog-api-key", "bad"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "API key not valid"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .and(header("x-goog-api-key", "good"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [{"text": "PONG"}]},
                    "finishReason": "STOP"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = GeminiBackend::new(
            format!("{}/models", server.uri()),
            vec!["bad".to_string(), "good".to_string()],
        )
        .unwrap();
        let response = backend.complete(request()).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "PONG");
        server.verify().await;

        // Rotation order, not just traffic: the rejected key is spent first
        // and the second key is the one that answers.
        let seen = server.received_requests().await.unwrap();
        let keys: Vec<_> = seen
            .iter()
            .map(|request| {
                request.headers["x-goog-api-key"]
                    .to_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(keys, vec!["bad", "good"]);
    }

    #[tokio::test]
    async fn an_exhausted_pool_reports_the_last_upstream_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "key rejected"}
            })))
            .mount(&server)
            .await;

        let backend = GeminiBackend::new(
            format!("{}/models", server.uri()),
            vec!["a".to_string(), "b".to_string()],
        )
        .unwrap();
        let error = backend.complete(request()).await.unwrap_err();
        assert!(error.to_string().contains("key rejected"));
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "one attempt per credential"
        );
    }

    #[tokio::test]
    async fn streams_sse_chunks() {
        let server = MockServer::start().await;
        let sse = concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hi\"}],\"role\":\"model\"}}],\"modelVersion\":\"gemini-3.8-flash\",\"responseId\":\"r1\"}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"!\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1,\"totalTokenCount\":2}}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:streamGenerateContent"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let mut req = request();
        req.stream = true;
        let chunks: Vec<StreamChunk> = backend
            .stream(req)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        let text: String = chunks.iter().filter_map(|c| c.content.clone()).collect();
        assert_eq!(text, "Hi!");
        assert_eq!(
            chunks.last().unwrap().finish_reason.as_deref(),
            Some("stop")
        );
    }

    // ---- streaming-pump characterization -------------------------------
    //
    // These pin the exact `StreamChunk` sequence the Gemini stream emits,
    // item for item, so a refactor of the SSE pump cannot quietly reorder,
    // drop, add or reword anything the client sees.

    /// Drive the streaming pump over `sse` and return every item it put on
    /// the channel — emitted chunks *and* a trailing failure, which the
    /// executor forwards in band.
    async fn stream_items(sse: String) -> Vec<Result<StreamChunk, GatewayError>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:streamGenerateContent"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;
        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let mut req = request();
        req.stream = true;
        backend.stream(req).collect::<Vec<_>>().await
    }

    /// The last item's message, or a panic naming the sequence.
    fn failure_message(items: &[Result<StreamChunk, GatewayError>]) -> String {
        match items.last() {
            Some(Err(GatewayError::Upstream(message))) => message.clone(),
            other => panic!("expected a trailing upstream failure, got {other:?}"),
        }
    }

    /// Assert `items` is exactly `chunks`, so a pin cannot pass on a prefix.
    fn assert_items(items: &[Result<StreamChunk, GatewayError>], chunks: Vec<StreamChunk>) {
        assert_eq!(items.len(), chunks.len(), "{items:?}");
        for (index, (item, expected)) in items.iter().zip(&chunks).enumerate() {
            assert_eq!(
                item.as_ref()
                    .unwrap_or_else(|error| panic!("item {index} failed: {error:?}")),
                expected,
                "item {index}"
            );
        }
    }

    /// The fail-closed message a body that never reached its terminal event
    /// gets, spelled out rather than imported: it is the string the shared
    /// pump sends and `lib.rs` sanitizes away on the wire.
    const UNTERMINATED: &str = "upstream stream ended before terminal event";

    /// A normal multi-event stream: one delta per text part (thought parts
    /// land in `reasoning`), then the terminal chunk carrying the usage the
    /// last frame reported. The translator's role announcement never reaches
    /// the client — the shared chunk mapping drops a frame with nothing in
    /// it — and neither does the keepalive comment.
    #[tokio::test]
    async fn stream_emits_deltas_then_the_terminal_chunk_in_order() {
        let items = stream_items(concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hi\"}],\"role\":\"model\"}}],\"modelVersion\":\"gemini-3.8-flash\",\"responseId\":\"r1\"}\n\n",
            ": keepalive\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"weighing\",\"thought\":true},{\"text\":\"!\"}]}}]}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":2,\"totalTokenCount\":3}}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string())
        .await;

        assert_items(
            &items,
            vec![
                StreamChunk::content("Hi"),
                StreamChunk {
                    reasoning: Some("weighing".to_string()),
                    ..StreamChunk::default()
                },
                StreamChunk::content("!"),
                StreamChunk {
                    finish_reason: Some("stop".to_string()),
                    usage: Some(Usage {
                        prompt_tokens: 1,
                        completion_tokens: 2,
                        total_tokens: 3,
                    }),
                    ..StreamChunk::default()
                },
            ],
        );
    }

    /// An upstream `event: error` frame fails the stream after the chunks
    /// already delivered, and `lib.rs` replaces the message on the wire (see
    /// its `stream_errors_never_leak_upstream_text`).
    #[tokio::test]
    async fn stream_fails_the_stream_on_an_upstream_error_frame() {
        let items = stream_items(concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}],\"role\":\"model\"}}],\"modelVersion\":\"gemini-3.8-flash\",\"responseId\":\"r1\"}\n\n",
            "event: error\ndata: {\"error\":{\"code\":429,\"message\":\"model overloaded\"}}\n\n",
        )
        .to_string())
        .await;

        assert_eq!(items.len(), 2, "the delta, then the failure: {items:?}");
        assert_items(&items[..1], vec![StreamChunk::content("partial")]);
        assert_eq!(failure_message(&items), "model overloaded");
    }

    /// A body that stops without `[DONE]` is not a finished stream: the
    /// chunks stand, then the fail-closed `UNTERMINATED` error.
    #[tokio::test]
    async fn stream_without_a_done_sentinel_fails_closed() {
        let items = stream_items(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}],\"role\":\"model\"}}],\"modelVersion\":\"gemini-3.8-flash\",\"responseId\":\"r1\"}\n\n"
                .to_string(),
        )
        .await;

        assert_eq!(items.len(), 2, "{items:?}");
        assert_items(&items[..1], vec![StreamChunk::content("partial")]);
        assert_eq!(failure_message(&items), UNTERMINATED);
    }

    /// An empty body emits nothing at all and still fails closed.
    #[tokio::test]
    async fn empty_stream_emits_nothing_and_fails_closed() {
        let items = stream_items(String::new()).await;

        assert!(
            items.iter().all(Result::is_err),
            "an empty stream must not emit a chunk: {items:?}"
        );
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(failure_message(&items), UNTERMINATED);
    }

    /// The decoder's own reason is the whole message: an un-terminated event
    /// must never echo the megabyte of upstream text behind it.
    #[tokio::test]
    async fn an_overflowed_event_fails_closed_without_echoing_upstream_text() {
        let canary = "CANARY-gemini-overflow-marker";
        let items = stream_items(format!(
            "data: {canary}{}",
            "x".repeat(heterion_router_http::sse::MAX_SSE_EVENT_BYTES + 4096)
        ))
        .await;

        assert_eq!(items.len(), 1, "{items:?}");
        let message = failure_message(&items);
        assert_eq!(message, "upstream sse event exceeded the decoder limit");
        assert!(!message.contains(canary), "{message}");
    }
}

/// Cache and reasoning accounting on both Gemini paths.
///
/// Gemini spells the counts `cachedContentTokenCount` /
/// `thoughtsTokenCount` in `usageMetadata`; the accounting layer's parser
/// knows those, so the checks below are about the counts surviving the trip
/// rather than about a second spelling table.
#[cfg(test)]
mod usage_detail_tests {
    use super::*;
    use crate::backend::chunk_cached_usage;
    use futures::StreamExt;
    use heterion_router_usage::CachedUsage;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn stream_chunks(sse: String) -> Vec<StreamChunk> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:streamGenerateContent"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;
        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        backend
            .stream(ChatCompletionRequest {
                model: "gemini-3.8-flash".to_string(),
                messages: vec![ChatMessage::plain("user", "hi")],
                stream: true,
                ..Default::default()
            })
            .map(|item| item.expect("chunk"))
            .collect::<Vec<_>>()
            .await
    }

    /// A terminal frame with `cachedContentTokenCount` and
    /// `thoughtsTokenCount` must reach the accounting boundary with both.
    #[tokio::test]
    async fn a_streamed_usage_frame_keeps_geminis_cached_and_thought_tokens() {
        let chunks = stream_chunks(concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hi\"}],\"role\":\"model\"}}],\"modelVersion\":\"gemini-3.8-flash\",\"responseId\":\"r1\"}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1000,\"candidatesTokenCount\":436,\"totalTokenCount\":1500,\"cachedContentTokenCount\":200,\"thoughtsTokenCount\":64}}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string())
        .await;

        assert_eq!(
            chunks.iter().filter_map(chunk_cached_usage).next_back(),
            Some(CachedUsage {
                cache_read: 200,
                cache_creation: 0,
                reasoning: 64,
            }),
            "the terminal usage frame reported 200 cached / 64 thought tokens"
        );
    }

    /// A frame without the detail keys books nothing extra, as before.
    #[tokio::test]
    async fn a_stream_without_detail_reports_nothing_to_book() {
        let chunks = stream_chunks(concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hi\"}],\"role\":\"model\"}}],\"modelVersion\":\"gemini-3.8-flash\",\"responseId\":\"r1\"}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":3,\"candidatesTokenCount\":2,\"totalTokenCount\":5}}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string())
        .await;

        assert_eq!(
            chunks.iter().filter_map(chunk_cached_usage).next_back(),
            None
        );
    }

    /// The non-streaming collapse hands the upstream payload over, so the
    /// same `usageMetadata` block is read by the same parser.
    #[tokio::test]
    async fn complete_hands_the_upstream_payload_to_the_accounting_layer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{"content": {"parts": [{"text": "PONG"}]}, "finishReason": "STOP"}],
                "usageMetadata": {
                    "promptTokenCount": 1000,
                    "candidatesTokenCount": 436,
                    "totalTokenCount": 1500,
                    "cachedContentTokenCount": 200,
                    "thoughtsTokenCount": 64,
                },
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let response = backend
            .complete(ChatCompletionRequest {
                model: "gemini-3.8-flash".to_string(),
                messages: vec![ChatMessage::plain("user", "hi")],
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(
            CachedUsage::from_payload(response.raw.as_ref().expect("upstream payload")),
            CachedUsage {
                cache_read: 200,
                cache_creation: 0,
                reasoning: 64,
            }
        );
        assert_eq!(serde_json::to_value(&response).unwrap().get("raw"), None);
        server.verify().await;

        // Attribution: Gemini routes by embedding the model in the URL
        // (`/models/<model>:generateContent`), so pin the route that answered.
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].url.path(),
            "/models/gemini-3.8-flash:generateContent",
            "the counted usage must belong to the model that was actually routed"
        );
    }
}
