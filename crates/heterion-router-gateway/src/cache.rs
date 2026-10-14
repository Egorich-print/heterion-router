//! Response cache in front of the completion path (roadmap M3.2).
//!
//! Two modes over the existing `semantic_cache` table (storage in
//! `heterion_router_db::cache_repos`):
//!
//! * **Exact** — SHA-256 over a canonical serialisation of the request.
//! * **Semantic** — embed the request through the gateway's own
//!   `/v1/embeddings` surface and serve the closest stored answer above a
//!   cosine threshold. No vector database: the scan is a range over the
//!   `signature` index, which is the right shape for a laptop.
//!
//! **Mechanism: one middleware around the completion routes.** It buffers the
//! request body, decides on a hit, and on a miss re-runs the handler with the
//! body it already read and books the answer it gets back. The alternative —
//! a read in the middleware and a `record_miss(...)` call in every handler arm
//! — would have to be repeated in `chat_completions`, `responses` and
//! `claude_endpoint::messages`, and each of those arms is where the streamed
//! and the failed paths diverge.
//!
//! **Position in the layer stack** (see `build_router_with_state`):
//!
//! ```text
//! cors  →  guardrails  →  budgets  →  cache  →  routes
//! ```
//!
//! * Inside `cors`, or a hit comes back without the CORS headers and a
//!   browser refuses it.
//! * Inside the guardrail middleware, so a `block` on a cached body still
//!   refuses: a cache hit must never be a way around a guardrail.
//! * Outside the budgets middleware, because a cache hit costs nothing
//!   (`x-router-cost: 0.000000`) and must not be charged as if it did.
//! * Outside `Authenticated`, unavoidably: that is an *extractor*, so it runs
//!   with the handler, after any middleware. That is why the namespace rule
//!   below carries the security weight.
//!
//! ## The exact key
//!
//! `signature = "x:<namespace>:<path tag>:<sha256 of the canonical body>"`.
//!
//! The canonical form is `canonical_json`: object keys sorted at **every**
//! depth, arrays left in order, scalars as `serde_json` spells them. Hashing
//! the parsed body rather than a fixed field list is what makes a `user`, a
//! `metadata` block, a `tool_choice` or an `extra` field impossible to
//! collide on — a fixed list is a list that will be incomplete. It is
//! deliberately conservative in the other direction too: a client that varies
//! a field it does not care about (a request id) gets no hits, which is a
//! missed saving rather than a wrong answer.
//!
//! `serde_json::Map` is already a `BTreeMap` today, so the sort is belt and
//! braces — it is what keeps the key stable if `preserve_order` is ever turned
//! on in the workspace, and `canonical_json_sorts_keys_at_every_depth` pins it.
//!
//! ## Semantic mode
//!
//! Off unless an operator names an embedding model (`with_semantic`); there
//! is no silent fallback to exact mode, and [`SemanticStatus::Off`] carries
//! the reason. On, the query embedding comes from
//! [`GatewayEmbedder`], i.e. from the configured provider's
//! `/v1/embeddings` — the same endpoint, credential resolution, URL rule and
//! accounting the public surface uses, with no second HTTP client and no new
//! dependency. When that call fails the request goes upstream as a miss, once
//! per process with a `warn!` naming the reason.
//!
//! **A semantic hit answers a *different* prompt.** The stored answer is the
//! closest neighbour's, not this request's, so the response carries
//! `x-router-cache-similarity` and semantic mode is skipped entirely for a
//! request that carries tools (another prompt's answer cannot satisfy a tool
//! contract). The default threshold is **0.95**: below that, embeddings of
//! paraphrases land around 0.85–0.92 where answers genuinely diverge, while
//! near-identical rewording and formatting-only differences sit above 0.97.
//! It is a caller-set knob because the right value depends on the embedding
//! model, and a model change silently invalidates every stored vector anyway
//! (dimensions stop matching, and [`cache_repos::cosine`] returns `0.0`).
//!
//! Semantic mode also costs one embedding call per cacheable request, which
//! is why it is not the default.
//!
//! ## Streaming
//!
//! `"stream": true` bypasses the cache **on both read and write**. A cached
//! stream cannot be replayed — the client has already been given the frames
//! once, and the row would be an empty shell of an event log — and it is
//! Future-AGI's documented behaviour too. A stream response is marked
//! `x-router-cache: bypass` so an operator can see it without reading logs.
//!
//! ## Security
//!
//! * **A cached body is sensitive.** It is whatever the model answered, which
//!   is frequently the prompt echoed back: names, keys pasted by mistake, a
//!   tenant's data. `semantic_cache.response` is therefore plaintext in the
//!   same `storage.sqlite` that holds `api_keys`. It inherits exactly the
//!   existing at-rest posture (`Db::open` sets `0600` on the file and `0700`
//!   on the directory), which is the best available without re-encrypting the
//!   table — but it does mean a backup, a sync folder or a support copy of
//!   the database carries cached answers, so the file must be treated as
//!   sensitive and the TTL is the only thing that takes a row out of it.
//!   There is no "do not cache secrets" switch: the cache cannot tell a
//!   secret from an answer.
//! * **Every entry is namespaced per API key.** The namespace is
//!   SHA-256 of the presented bearer token, truncated to 16 hex characters —
//!   the token itself is never stored, logged or keyed. Without it any
//!   authenticated client could read any other client's answers by sending the
//!   same prompt, which on a multi-tenant gateway is a cross-tenant data
//!   leak, not a performance detail. With it, a hit is only ever served to
//!   the key that caused the write.
//! * **A token that no longer authenticates gets nothing.** The middleware
//!   runs before `Authenticated`, so it re-checks the presented token against
//!   `repos::find_usable_api_key` — a revoked, banned, deactivated or expired
//!   key is refused at the cache boundary and falls through to the handler's
//!   401 instead of being served its own cached answers.
//! * **An open gateway (no usable key, no token) shares one namespace**
//!   ([`OPEN_NAMESPACE`]). There is no identity to separate, and inventing one
//!   would only hide that.
//!
//! ## What is never cached
//!
//! Non-`POST`, a path that is not a completion verb, a body whose size is not
//! known upfront (a chunked request, a streamed response — they are passed
//! through untouched rather than buffered), a body over
//! [`MAX_CACHED_BODY`], a non-UTF-8 or non-JSON answer, any status that is
//! not 2xx, an answer carrying a top-level `error`, and a stream.
//!
//! ## Indexes
//!
//! `signature` is `UNIQUE`, which is the index the exact path needs and the
//! range the semantic path needs. The one index still missing is
//! `expires_at` (the lazy sweep is a full scan); it belongs in
//! `heterion-router-db`'s `MIGRATIONS` ledger and is deliberately not created
//! from here — see `cache_repos` for the same note.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use axum::Json;
use axum::body::{Body, HttpBody};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use heterion_router_db::Db;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::AppState;

/// Set on every response the cache saw: `hit`, `miss` or `bypass`.
///
/// Same name and same precision convention as `lib.rs`'s `HEADER_COST`, so a
/// cache hit is told apart from a free-but-real call by the marker rather
/// than by guessing at the number.
const HEADER_CACHE: &str = "x-router-cache";
/// `lib.rs::HEADER_COST`, repeated here because the middleware is the only
/// writer that answers without an `AppState::prices`.
const HEADER_COST: &str = "x-router-cost";
/// Cosine of a semantic hit, `{:.4}` — the honesty marker for a hit that
/// answers a different prompt.
const HEADER_SIMILARITY: &str = "x-router-cache-similarity";

/// Namespace of a gateway with no usable API key and no presented token.
pub const OPEN_NAMESPACE: &str = "open";

/// Largest body the cache will hold, request or answer.
///
/// One megabyte of English is roughly 250k tokens, so this is far past any
/// real agent context; it exists to bound memory, not to shape behaviour.
const MAX_CACHED_BODY: u64 = 16 * 1024 * 1024;

/// Completion verbs worth caching. `/v1/messages/count_tokens` and
/// `/v1/embeddings` are not completions and are left alone.
const CACHEABLE_PATHS: [&str; 3] = ["/v1/chat/completions", "/v1/messages", "/v1/responses"];

/// Future-AGI's `default_ttl`, and the answer to "how long may a cached
/// answer live": long enough to cover a retry loop and an agent turn, short
/// enough that a stale model or a revoked permission ages out on its own.
const DEFAULT_TTL_SECONDS: i64 = 3600;

/// See the module docs: paraphrases diverge well below this.
const DEFAULT_SEMANTIC_THRESHOLD: f32 = 0.95;

/// One cache key: a namespace, a verb and a prompt hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    namespace: String,
    path_tag: String,
    prompt_hash: String,
}

impl CacheKey {
    /// The exact key for `body` on `path`, in `namespace`.
    ///
    /// `namespace` is the caller's: [`CacheLayer::namespace`] derives it from
    /// the presented token, and it is *part of the signature*, so two keys
    /// cannot reach one row even when they ask the same question.
    pub fn new(namespace: &str, path: &str, body: &Value) -> Self {
        Self {
            namespace: namespace.to_string(),
            path_tag: path_tag(path),
            prompt_hash: hex::encode(Sha256::digest(canonical_json(body).as_bytes())),
        }
    }

    /// The row an exact hit reads: `x:<namespace>:<path tag>:<prompt hash>`.
    pub fn signature(&self) -> String {
        format!(
            "x:{}:{}:{}",
            self.namespace, self.path_tag, self.prompt_hash
        )
    }

    /// The row a semantic hit reads: same key, `v:` swapped for `x:`, because
    /// the answer is stored once and both modes read it.
    pub fn answer_signature(&self) -> String {
        self.signature()
    }

    /// The row that holds this request's vector: `v:<namespace>:<path>:<hash>`.
    pub fn vector_signature(&self) -> String {
        format!(
            "v:{}:{}:{}",
            self.namespace, self.path_tag, self.prompt_hash
        )
    }

    /// Inclusive lower bound of the vector range for this namespace and verb.
    pub fn vector_prefix(&self) -> String {
        format!("v:{}:{}:", self.namespace, self.path_tag)
    }

    /// The answer signature a stored `v:` signature belongs to.
    pub fn answer_signature_of(vector_signature: &str) -> Option<String> {
        vector_signature
            .strip_prefix("v:")
            .map(|rest| format!("x:{rest}"))
    }

    /// SHA-256 of the canonical body, stored as `prompt_hash`.
    pub fn prompt_hash(&self) -> &str {
        &self.prompt_hash
    }
}

/// `/v1/chat/completions` → `v1:chat:completions`.
///
/// `':'` cannot occur in the verbs the cache is allowed to serve, so the
/// signature stays unambiguous no matter what a future route is called.
fn path_tag(path: &str) -> String {
    path.trim_start_matches('/').replace('/', ":")
}

/// Deterministic JSON: object keys sorted at every depth, arrays untouched.
///
/// The hash input. Not a general-purpose canonicalisation (no number
/// normalisation, no unicode folding) because it only has to be *stable*,
/// and a stable-but-stricter form that gives a client *fewer* hits is the
/// safe direction to be wrong in.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (position, key) in keys.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("a string key always serializes"));
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// Turns a prompt into vectors.
///
/// A trait because the production embedder costs a provider call and a test
/// must be able to place two prompts 0.01 apart in cosine without a network.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// One vector per input, in input order. `Err` degrades semantic mode to
    /// off for that request — it never falls back to serving something else.
    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, String>;
}

/// Embeds through the gateway's own `/v1/embeddings`.
///
/// Reusing the endpoint (rather than a second HTTP client) is what keeps this
/// dependency-free: `embeddings.rs` already owns provider resolution, the
/// `provider/model` rule, credential lookup, the URL shape and accounting.
pub struct GatewayEmbedder {
    state: AppState,
    model: String,
}

impl GatewayEmbedder {
    /// `model` is `provider/model`, as the embeddings endpoint requires.
    pub fn new(state: AppState, model: impl Into<String>) -> Self {
        Self {
            state,
            model: model.into(),
        }
    }
}

#[async_trait]
impl Embedder for GatewayEmbedder {
    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let request = crate::embeddings::EmbeddingRequest {
            model: self.model.clone(),
            input: crate::embeddings::EmbeddingInput::Batch(inputs.to_vec()),
            encoding_format: Some("float".to_string()),
            dimensions: None,
            user: None,
        };
        let response = crate::embeddings::embeddings(
            State(self.state.clone()),
            // The router pays for this call, not a client: the model is
            // server-configured and never client-chosen, so there is no
            // per-key model allowlist for a client to escape through. The row
            // is still booked — the tokens were really spent.
            crate::auth::Authenticated { key: None },
            crate::JsonBody(request),
        )
        .await;
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), MAX_CACHED_BODY as usize)
            .await
            .map_err(|error| error.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "embeddings endpoint for {} returned {status}",
                self.model
            ));
        }
        let payload: Value = serde_json::from_slice(&body).map_err(|error| error.to_string())?;
        let data = payload
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| "embeddings response carried no data".to_string())?;
        data.iter()
            .map(|entry| {
                let numbers = entry
                    .get("embedding")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "embeddings entry carried no embedding".to_string())?;
                numbers
                    .iter()
                    .map(|number| {
                        number
                            .as_f64()
                            .map(|number| number as f32)
                            .ok_or_else(|| "embedding value is not a number".to_string())
                    })
                    .collect()
            })
            .collect()
    }
}

/// Whether semantic mode can run, and why not when it cannot.
#[derive(Debug, Clone, PartialEq)]
pub enum SemanticStatus {
    /// Off, with the reason to show an operator.
    Off(&'static str),
    /// Configured: this is the embedding model and the cosine floor.
    Ready { model: String, threshold: f32 },
}

/// Semantic settings.
#[derive(Clone)]
struct Semantic {
    model: String,
    threshold: f32,
    embedder: Arc<dyn Embedder>,
}

/// Middleware state: the database, the TTL, and the optional embedder.
#[derive(Clone)]
pub struct CacheLayer {
    db: Option<Arc<Db>>,
    require_auth: bool,
    ttl_seconds: i64,
    semantic: Option<Semantic>,
    /// One degradation warning per process: a missing embedding provider must
    /// not turn into a log line per request.
    warned: Arc<AtomicBool>,
}

impl CacheLayer {
    /// Exact mode against whatever database the state carries. A gateway
    /// without one is a gateway without a cache — the layer is then a
    /// pass-through and says nothing on the wire.
    pub fn new(state: &AppState) -> Self {
        Self {
            db: state.db.clone(),
            require_auth: state.require_auth,
            ttl_seconds: DEFAULT_TTL_SECONDS,
            semantic: None,
            warned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Override the TTL (seconds). Negative values store already-expired rows.
    pub fn with_ttl(mut self, seconds: i64) -> Self {
        self.ttl_seconds = seconds;
        self
    }

    /// Turn semantic mode on with `model` (`provider/model`) and the default
    /// cosine floor ([`DEFAULT_SEMANTIC_THRESHOLD`]).
    pub fn with_semantic(mut self, model: impl Into<String>, embedder: Arc<dyn Embedder>) -> Self {
        self.semantic = Some(Semantic {
            model: model.into(),
            threshold: DEFAULT_SEMANTIC_THRESHOLD,
            embedder,
        });
        self
    }

    /// Override the cosine floor. Only worth touching once the embedding model
    /// is fixed: a different model invalidates every stored vector anyway.
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        if let Some(semantic) = self.semantic.as_mut() {
            semantic.threshold = threshold;
        }
        self
    }

    /// Whether semantic mode is configured. Never lies: it is `Off` with a
    /// reason rather than a quiet downgrade to exact.
    pub fn semantic_status(&self) -> SemanticStatus {
        match &self.semantic {
            None => SemanticStatus::Off("no embedding model configured"),
            Some(semantic) => SemanticStatus::Ready {
                model: semantic.model.clone(),
                threshold: semantic.threshold,
            },
        }
    }

    /// The namespace `headers` may read and write in, or `None` when this
    /// caller may not use the cache at all.
    ///
    /// Mirrors `auth.rs`'s decision without its work: a usable token gets its
    /// own namespace, an unauthenticated caller on an open gateway gets the
    /// shared one, and anything else gets nothing — the handler's 401 or 403
    /// is the only answer that caller sees.
    fn namespace(&self, db: &Db, headers: &HeaderMap) -> Option<String> {
        let guard = db.connection();
        let token = bearer_token(headers);
        match &token {
            Some(token) => {
                match heterion_router_db::repos::find_usable_api_key(&guard, token) {
                    Ok(Some(_)) => Some(namespace_of(token)),
                    // An unusable token is the handler's business, not ours:
                    // it must not inherit a cached answer.
                    _ => None,
                }
            }
            None => {
                let keys_exist = crate::auth::has_usable_api_key(&guard).unwrap_or(true);
                if keys_exist || self.require_auth {
                    None
                } else {
                    Some(OPEN_NAMESPACE.to_string())
                }
            }
        }
    }

    /// Log a semantic degradation once, with the reason.
    fn warn_degraded(&self, reason: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            tracing::warn!("semantic response cache disabled: {reason}");
        }
    }
}

/// The namespace of one API key: SHA-256 of the token, truncated.
///
/// The token itself is never stored or logged, and truncation keeps the
/// signature readable in a support query. 64 bits of a hash of a high-entropy
/// secret is not a guessable namespace, and a collision would need two keys
/// whose hashes agree — which is the same bar the stored digest in
/// `api_keys.key_hash` is held to.
fn namespace_of(token: &str) -> String {
    let digest = hex::encode(Sha256::digest(token.as_bytes()));
    digest[..16].to_string()
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Whether the client asked for a stream.
fn is_streaming(body: &Value) -> bool {
    body.get("stream").and_then(Value::as_bool) == Some(true)
}

/// Whether the request carries tools. A cached tool-calling answer is only
/// replayable for the identical request (which the exact key guarantees), and
/// a *semantic* hit for a different prompt could never satisfy the contract.
fn carries_tools(body: &Value) -> bool {
    match body.get("tools") {
        None | Some(Value::Null) => false,
        Some(Value::Array(tools)) => !tools.is_empty(),
        Some(_) => true,
    }
}

/// The text embedded for a semantic lookup: the canonical body.
///
/// Deterministic, complete, and needing no tokenizer and no per-verb message
/// extraction — which also means the three cacheable verbs share one
/// embedding space.
fn query_text(body: &Value) -> String {
    canonical_json(body)
}

/// Tokens a stored answer saved, read off the usage the upstream reported.
///
/// Both spellings: `prompt_tokens`/`completion_tokens` (OpenAI) and
/// `input_tokens`/`output_tokens`. Anything unparseable books zero rather
/// than guessing.
fn tokens_saved_by(body: &[u8]) -> i64 {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return 0;
    };
    let Some(usage) = value.get("usage") else {
        return 0;
    };
    let count = |primary: &str, fallback: &str| {
        usage
            .get(primary)
            .or_else(|| usage.get(fallback))
            .and_then(Value::as_u64)
            .unwrap_or(0) as i64
    };
    count("prompt_tokens", "input_tokens") + count("completion_tokens", "output_tokens")
}

/// The exact-mode read. `None` for a miss, an expired row or a dead database.
fn exact_hit(db: &Db, key: &CacheKey) -> Option<Response> {
    let guard = db.connection();
    let row = match heterion_router_db::cache_repos::get(&guard, &key.signature()) {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!("response cache read failed: {error}");
            return None;
        }
    };
    if let Err(error) =
        heterion_router_db::cache_repos::record_hit(&guard, &row.signature, row.tokens_saved)
    {
        tracing::warn!("response cache hit booking failed: {error}");
    }
    Some(hit_response(&row.response, None))
}

/// The semantic-mode read: embed, scan the namespace, serve the closest
/// answer at or above the threshold.
async fn semantic_hit(
    layer: &CacheLayer,
    db: &Db,
    key: &CacheKey,
    body: &Value,
) -> Option<Response> {
    let semantic = layer.semantic.as_ref()?;
    if carries_tools(body) {
        return None;
    }
    let model = body.get("model").and_then(Value::as_str)?;
    let query = match semantic.embedder.embed(&[query_text(body)]).await {
        Ok(mut vectors) => vectors.pop()?,
        Err(reason) => {
            layer.warn_degraded(&reason);
            return None;
        }
    };
    let guard = db.connection();
    let rows = match heterion_router_db::cache_repos::vectors_in(
        &guard,
        &heterion_router_db::cache_repos::VectorScope {
            prefix: &key.vector_prefix(),
            model,
        },
    ) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!("semantic cache scan failed: {error}");
            return None;
        }
    };
    let best = heterion_router_db::cache_repos::nearest(&rows, &query, semantic.threshold)?;
    let similarity = heterion_router_db::cache_repos::cosine(&best.vector, &query);
    let answer_signature = CacheKey::answer_signature_of(&best.signature)?;
    let row = match heterion_router_db::cache_repos::get(&guard, &answer_signature) {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!("semantic cache read failed: {error}");
            return None;
        }
    };
    if let Err(error) =
        heterion_router_db::cache_repos::record_hit(&guard, &row.signature, row.tokens_saved)
    {
        tracing::warn!("semantic cache hit booking failed: {error}");
    }
    Some(hit_response(&row.response, Some(similarity)))
}

/// A served-from-cache answer.
///
/// `200` and `application/json` are the only two things the row does not
/// carry: the schema has no status and no content-type column, and a row only
/// exists because a 2xx JSON body was stored. The cost is `0.000000` in
/// `lib.rs`'s precision, because nothing was spent.
fn hit_response(body: &str, similarity: Option<f32>) -> Response {
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("a cached body is valid UTF-8 text");
    mark(&mut response, HEADER_CACHE, "hit");
    response.headers_mut().insert(
        HeaderName::from_static(HEADER_COST),
        HeaderValue::from_static("0.000000"),
    );
    if let Some(similarity) = similarity
        && let Ok(value) = HeaderValue::from_str(&format!("{similarity:.4}"))
    {
        response
            .headers_mut()
            .insert(HeaderName::from_static(HEADER_SIMILARITY), value);
    }
    response
}

fn mark(response: &mut Response, name: &'static str, value: &'static str) {
    response.headers_mut().insert(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    );
}

/// The miss path: run the handler, then store what it produced.
async fn store(
    layer: &CacheLayer,
    db: &Db,
    key: &CacheKey,
    request: &Value,
    response: Response,
) -> Response {
    let (parts, stream) = response.into_parts();
    // A size the producer did not know means a stream: leave it alone rather
    // than buffer a body that cannot be replayed anyway.
    let exact = stream.size_hint().exact();
    if !parts.status.is_success() || exact.is_none_or(|size| size > MAX_CACHED_BODY) {
        // Untouched: the client gets exactly what the handler produced.
        return rebuild_response(parts, stream);
    }
    let bytes = match axum::body::to_bytes(stream, MAX_CACHED_BODY as usize).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!("response body could not be buffered for the cache: {error}");
            return bad_gateway("upstream response body could not be read");
        }
    };
    let text = String::from_utf8(bytes.to_vec()).ok();
    if let Some(text) = text.as_deref()
        && let Ok(answer) = serde_json::from_str::<Value>(text)
        && answer.get("error").is_none()
        && let Some(model) = answered_model(request, &answer)
    {
        write_entry(layer, db, key, request, &model, text, &bytes).await;
    }
    let mut response = rebuild_response(parts, Body::from(bytes));
    mark(&mut response, HEADER_CACHE, "miss");
    response
}

/// The model a row is filed under: what the client asked for, falling back to
/// what the answer names (an alias, or a combo that resolved elsewhere).
///
/// Exact mode files under the requested id, because that is what the next
/// identical request will ask for again — filing under the resolved id would
/// make a combo answer unreachable from its own name.
fn answered_model(request: &Value, answer: &Value) -> Option<String> {
    request
        .get("model")
        .and_then(Value::as_str)
        .or_else(|| answer.get("model").and_then(Value::as_str))
        .map(str::to_string)
}

/// Store one miss: the answer row, then the vector row when semantic mode is
/// on and answered. Lazy expiry sweep rides along.
async fn write_entry(
    layer: &CacheLayer,
    db: &Db,
    key: &CacheKey,
    request: &Value,
    model: &str,
    response: &str,
    bytes: &[u8],
) {
    // The embedding is taken BEFORE the database is locked: a provider round
    // trip must never be held against the single `Db` mutex.
    let vector = match layer.semantic.as_ref() {
        Some(semantic) if !carries_tools(request) => {
            match semantic.embedder.embed(&[query_text(request)]).await {
                Ok(mut vectors) => vectors.pop(),
                Err(reason) => {
                    layer.warn_degraded(&reason);
                    None
                }
            }
        }
        _ => None,
    };
    let guard = db.connection();
    let write = heterion_router_db::cache_repos::CacheWrite {
        signature: key.answer_signature(),
        model: model.to_string(),
        prompt_hash: key.prompt_hash().to_string(),
        response: response.to_string(),
        tokens_saved: tokens_saved_by(bytes),
        ttl_seconds: layer.ttl_seconds,
    };
    if let Err(error) = heterion_router_db::cache_repos::put(&guard, &write) {
        tracing::warn!("response cache write failed: {error}");
        return;
    }
    if let Some(vector) = vector
        && let Err(error) = heterion_router_db::cache_repos::put_vector(
            &guard,
            &key.vector_signature(),
            model,
            key.prompt_hash(),
            &vector,
            layer.ttl_seconds,
        )
    {
        tracing::warn!("semantic cache vector write failed: {error}");
    }
    if let Err(error) = heterion_router_db::cache_repos::sweep_expired(&guard) {
        tracing::warn!("response cache sweep failed: {error}");
    }
}

/// The same request again, with the body the key was computed from.
fn rebuild_request(parts: axum::http::request::Parts, body: Body) -> Request {
    Request::from_parts(parts, body)
}

/// The same response again, with the body the cache booked from it.
fn rebuild_response(parts: axum::http::response::Parts, body: Body) -> Response {
    Response::from_parts(parts, body)
}

/// The cache middleware.
///
/// One pass over a completion request: read the key, serve a hit without
/// touching a provider, otherwise re-run the handler with the body it already
/// read and book the answer.
pub async fn dispatch(State(layer): State<CacheLayer>, request: Request, next: Next) -> Response {
    let Some(db) = layer.db.clone() else {
        return next.run(request).await;
    };
    if request.method() != Method::POST || !CACHEABLE_PATHS.contains(&request.uri().path()) {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    // A body that is definitely larger than the cap is passed through rather
    // than buffered. An *unknown* length is not: an outer middleware that
    // re-streams the body (the guardrail scanner does) erases the exact hint
    // on every request, and refusing those would silently disable the cache
    // for the whole request path. `to_bytes` below is the actual bound.
    if body.size_hint().lower() > MAX_CACHED_BODY {
        return next.run(rebuild_request(parts, body)).await;
    }
    let bytes = match axum::body::to_bytes(body, MAX_CACHED_BODY as usize).await {
        Ok(bytes) => bytes,
        Err(_) => return unprocessable("request body could not be read"),
    };
    let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) else {
        return next.run(rebuild_request(parts, Body::from(bytes))).await;
    };
    // Streaming bypasses both ways, and says so.
    if is_streaming(&parsed) {
        let mut response = next.run(rebuild_request(parts, Body::from(bytes))).await;
        mark(&mut response, HEADER_CACHE, "bypass");
        return response;
    }
    let Some(namespace) = layer.namespace(&db, &parts.headers) else {
        return next.run(rebuild_request(parts, Body::from(bytes))).await;
    };
    let key = CacheKey::new(&namespace, parts.uri.path(), &parsed);
    if let Some(hit) = exact_hit(&db, &key) {
        return hit;
    }
    if let Some(hit) = semantic_hit(&layer, &db, &key, &parsed).await {
        return hit;
    }
    let response = next.run(rebuild_request(parts, Body::from(bytes))).await;
    store(&layer, &db, &key, &parsed, response).await
}

/// The layer to register, with the defaults (exact mode on, semantic off).
///
/// `axum::middleware::from_fn_with_state` cannot be wrapped in a helper that
/// names its return type — `tower::Layer` is not re-exported by axum and this
/// crate must not grow a dependency for a type alias — so registration is
/// spelled out at the call site:
///
/// ```ignore
/// // in build_router_with_state, directly above the cors layer:
/// .layer(axum::middleware::from_fn_with_state(
///     cache::CacheLayer::new(&state),
///     cache::dispatch,
/// ))
/// ```
pub fn defaults(state: &AppState) -> CacheLayer {
    CacheLayer::new(state)
}

fn unprocessable(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": {
                "message": message,
                "type": "invalid_request_error",
                "code": "invalid_request",
            }
        })),
    )
        .into_response()
}

/// The one place the cache can cost a client its body: a buffered answer that
/// turned out to be unreadable half way through. Say so rather than hand back
/// a truncated document.
fn bad_gateway(message: &str) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": {
                "message": message,
                "type": "upstream_error",
                "code": "upstream_error",
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{ChatBackend, ChunkStream};
    use crate::{AppState, build_router_with};
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{HeaderMap, Request, StatusCode, header::AUTHORIZATION};
    use futures::StreamExt;
    use heterion_router_core::{
        ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
        StreamChunk, Usage,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    /// Completion counter: every test asserts on how many upstream calls the
    /// cache let through, which is the whole observable contract.
    #[derive(Clone)]
    struct CountingBackend {
        calls: Arc<AtomicUsize>,
        streams: Arc<AtomicUsize>,
        fail: bool,
    }

    impl CountingBackend {
        fn new() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                streams: Arc::new(AtomicUsize::new(0)),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::new()
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn streams(&self) -> usize {
            self.streams.load(Ordering::SeqCst)
        }
    }

    fn reply(request: &ChatCompletionRequest) -> String {
        let last_user = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| message.text())
            .unwrap_or_default();
        format!("echo: {last_user}")
    }

    #[async_trait::async_trait]
    impl ChatBackend for CountingBackend {
        fn name(&self) -> &'static str {
            "counting"
        }

        async fn complete(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(GatewayError::Upstream("upstream status 503: nope".into()));
            }
            let answer = reply(&request);
            Ok(ChatCompletionResponse {
                id: "chatcmpl-test".to_string(),
                object: "chat.completion",
                created: 0,
                model: request.model,
                choices: vec![ChatChoice {
                    index: 0,
                    message: ChatMessage::assistant_text(answer),
                    finish_reason: "stop".into(),
                }],
                usage: Usage {
                    prompt_tokens: 11,
                    completion_tokens: 5,
                    total_tokens: 16,
                },
                raw: None,
            })
        }

        fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
            self.streams.fetch_add(1, Ordering::SeqCst);
            let words: Vec<Result<StreamChunk, GatewayError>> =
                vec![Ok(StreamChunk::content(reply(&request)))];
            Box::pin(
                futures::stream::iter(words).chain(futures::stream::once(async {
                    Ok(StreamChunk::done("stop"))
                })),
            )
        }
    }

    /// An embedder that answers by substring, so a test can place two prompts
    /// 0.01 apart in cosine without a network.
    struct StubEmbedder {
        rules: Vec<(String, Vec<f32>)>,
        fail: bool,
    }

    impl StubEmbedder {
        fn new(rules: Vec<(String, Vec<f32>)>) -> Arc<Self> {
            Arc::new(Self { rules, fail: false })
        }

        fn failing() -> Arc<Self> {
            Arc::new(Self {
                rules: Vec::new(),
                fail: true,
            })
        }
    }

    #[async_trait::async_trait]
    impl Embedder for StubEmbedder {
        async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
            if self.fail {
                return Err("no embeddings provider configured".to_string());
            }
            Ok(inputs
                .iter()
                .map(|input| {
                    self.rules
                        .iter()
                        .find(|(needle, _)| input.contains(needle.as_str()))
                        .map(|(_, vector)| vector.clone())
                        .unwrap_or_else(|| vec![-1.0, 0.0, 0.0])
                })
                .collect())
        }
    }

    fn db() -> Arc<heterion_router_db::Db> {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        db
    }

    /// Router + the backend behind it, with exact mode on and no embedder.
    fn cached(backend: CountingBackend, db: Arc<heterion_router_db::Db>) -> Router {
        let state = AppState::with_db(Arc::new(backend), db);
        build_router_with(state.clone(), CacheLayer::new(&state))
    }

    fn prompt(text: &str) -> Value {
        json!({"model": "gpt-test", "messages": [{"role": "user", "content": text}]})
    }

    /// POST a body to `/v1/chat/completions` and read the whole answer.
    async fn call(router: Router, body: Value, bearer: Option<&str>) -> Call {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json");
        if let Some(token) = bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = router
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let text = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        Call {
            status,
            headers,
            text,
        }
    }

    struct Call {
        status: StatusCode,
        headers: HeaderMap,
        text: String,
    }

    impl Call {
        fn cache(&self) -> &str {
            self.headers
                .get(HEADER_CACHE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("<none>")
        }

        fn cost(&self) -> Option<&str> {
            self.headers
                .get(HEADER_COST)
                .and_then(|value| value.to_str().ok())
        }
    }

    fn cached_rows(db: &heterion_router_db::Db) -> i64 {
        let guard = db.connection();
        heterion_router_db::cache_repos::count(&guard).unwrap()
    }

    #[tokio::test]
    async fn identical_non_streaming_requests_cost_one_upstream_call() {
        let backend = CountingBackend::new();
        let db = db();
        let router = cached(backend.clone(), db.clone());

        let first = call(router.clone(), prompt("hello"), None).await;
        assert_eq!(first.status, StatusCode::OK);
        assert_eq!(first.cache(), "miss");
        assert!(first.text.contains("echo: hello"));

        let second = call(router, prompt("hello"), None).await;
        assert_eq!(second.status, StatusCode::OK);
        assert_eq!(second.cache(), "hit");
        assert_eq!(second.cost(), Some("0.000000"));
        assert_eq!(second.text, first.text, "a hit replays the stored body");
        assert_eq!(backend.calls(), 1, "the second call never left the gateway");

        let guard = db.connection();
        let row = heterion_router_db::cache_repos::get(
            &guard,
            &CacheKey::new(OPEN_NAMESPACE, "/v1/chat/completions", &prompt("hello")).signature(),
        )
        .unwrap()
        .expect("stored row");
        assert_eq!(row.response, first.text, "the row holds the upstream body");
        drop(guard);
    }

    #[tokio::test]
    async fn a_hit_booked_the_tokens_it_saved() {
        let backend = CountingBackend::new();
        let db = db();
        let router = cached(backend.clone(), db.clone());
        call(router.clone(), prompt("hello"), None).await;
        call(router.clone(), prompt("hello"), None).await;
        assert_eq!(backend.calls(), 1);
        let guard = db.connection();
        let row = heterion_router_db::cache_repos::get(
            &guard,
            &CacheKey::new(OPEN_NAMESPACE, "/v1/chat/completions", &prompt("hello")).signature(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(row.hit_count, 1);
        assert_eq!(
            row.tokens_saved, 32,
            "16 stored with the answer, then 16 more for the hit it saved"
        );
    }

    #[tokio::test]
    async fn a_differing_sampling_parameter_is_a_different_key() {
        let backend = CountingBackend::new();
        let router = cached(backend.clone(), db());
        let cold = json!({"model": "gpt-test", "messages": [{"role": "user", "content": "hello"}], "temperature": 0.0});
        let hot = json!({"model": "gpt-test", "messages": [{"role": "user", "content": "hello"}], "temperature": 1.0});

        assert_eq!(
            call(router.clone(), cold.clone(), None).await.cache(),
            "miss"
        );
        assert_eq!(
            call(router.clone(), hot.clone(), None).await.cache(),
            "miss"
        );
        assert_eq!(
            call(router.clone(), cold.clone(), None).await.cache(),
            "hit"
        );
        assert_eq!(call(router.clone(), hot.clone(), None).await.cache(), "hit");
        assert_eq!(backend.calls(), 2, "two keys, two upstream calls");

        let cold_key = CacheKey::new(OPEN_NAMESPACE, "/v1/chat/completions", &cold);
        let hot_key = CacheKey::new(OPEN_NAMESPACE, "/v1/chat/completions", &hot);
        assert_ne!(cold_key.signature(), hot_key.signature());
        assert_eq!(cold_key.prompt_hash(), cold_key.prompt_hash());
    }

    #[tokio::test]
    async fn streaming_bypasses_the_cache_on_read_and_on_write() {
        let backend = CountingBackend::new();
        let db = db();
        let router = cached(backend.clone(), db.clone());
        let body = json!({"model": "gpt-test", "messages": [{"role": "user", "content": "hello"}], "stream": true});

        let first = call(router.clone(), body.clone(), None).await;
        assert_eq!(first.status, StatusCode::OK);
        assert_eq!(first.cache(), "bypass");
        assert!(first.text.contains("[DONE]"), "a real SSE stream came back");

        let second = call(router, body.clone(), None).await;
        assert_eq!(second.cache(), "bypass");
        assert_eq!(backend.calls(), 0, "no non-streaming upstream call");
        assert_eq!(backend.streams(), 2, "both streams reached the upstream");
        assert_eq!(cached_rows(&db), 0, "a stream is never written either");
    }

    #[tokio::test]
    async fn an_expired_row_is_not_served() {
        let backend = CountingBackend::new();
        let db = db();
        let body = prompt("hello");
        {
            let guard = db.connection();
            let key = CacheKey::new(OPEN_NAMESPACE, "/v1/chat/completions", &body);
            heterion_router_db::cache_repos::put(
                &guard,
                &heterion_router_db::cache_repos::CacheWrite {
                    signature: key.signature().to_string(),
                    model: "gpt-test".to_string(),
                    prompt_hash: key.prompt_hash().to_string(),
                    response: "stale answer".to_string(),
                    tokens_saved: 0,
                    ttl_seconds: -60,
                },
            )
            .unwrap();
        }
        let answer = call(cached(backend.clone(), db), body, None).await;
        assert_eq!(answer.cache(), "miss");
        assert!(answer.text.contains("echo: hello"), "the upstream answered");
        assert_eq!(backend.calls(), 1);
    }

    #[tokio::test]
    async fn an_error_response_is_never_cached() {
        let backend = CountingBackend::failing();
        let db = db();
        let router = cached(backend.clone(), db.clone());
        let first = call(router.clone(), prompt("hello"), None).await;
        assert!(first.status.is_server_error() || first.status.is_client_error());
        let second = call(router, prompt("hello"), None).await;
        assert!(second.status.is_server_error() || second.status.is_client_error());
        assert_eq!(backend.calls(), 2, "the failure was not replayed");
        assert_eq!(cached_rows(&db), 0);
    }

    #[tokio::test]
    async fn a_rejected_request_is_never_cached() {
        let backend = CountingBackend::new();
        let db = db();
        let state = AppState::with_db(Arc::new(backend.clone()), db.clone());
        {
            let guard = db.connection();
            heterion_router_db::repos::insert_api_key(
                &guard,
                &heterion_router_db::repos::ApiKeyRow {
                    id: "k1".to_string(),
                    name: "narrow".to_string(),
                    key: "sk-narrow".to_string(),
                    allowed_models: "[\"other-model\"]".to_string(),
                    no_log: false,
                },
            )
            .unwrap();
        }
        let router = build_router_with(state.clone(), CacheLayer::new(&state));
        let denied = call(router.clone(), prompt("hello"), Some("sk-narrow")).await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN);
        assert_eq!(
            call(router, prompt("hello"), Some("sk-narrow"))
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(backend.calls(), 0);
        assert_eq!(cached_rows(&db), 0, "a 403 is not a cacheable answer");
    }

    #[tokio::test]
    async fn two_api_keys_do_not_share_cached_answers() {
        let backend = CountingBackend::new();
        let db = db();
        {
            let guard = db.connection();
            for (id, key) in [("k1", "sk-one"), ("k2", "sk-two")] {
                heterion_router_db::repos::insert_api_key(
                    &guard,
                    &heterion_router_db::repos::ApiKeyRow {
                        id: id.to_string(),
                        name: id.to_string(),
                        key: key.to_string(),
                        allowed_models: "[\"*\"]".to_string(),
                        no_log: false,
                    },
                )
                .unwrap();
            }
        }
        let router = cached(backend.clone(), db);

        assert_eq!(
            call(router.clone(), prompt("secret question"), Some("sk-one"))
                .await
                .cache(),
            "miss"
        );
        assert_eq!(
            call(router.clone(), prompt("secret question"), Some("sk-one"))
                .await
                .cache(),
            "hit"
        );
        assert_eq!(
            call(router.clone(), prompt("secret question"), Some("sk-two"))
                .await
                .cache(),
            "miss",
            "key two must not read key one's answer"
        );
        assert_eq!(
            call(router, prompt("secret question"), Some("sk-two"))
                .await
                .cache(),
            "hit"
        );
        assert_eq!(backend.calls(), 2);
    }

    #[tokio::test]
    async fn a_revoked_key_cannot_read_its_own_cached_answers() {
        let backend = CountingBackend::new();
        let db = db();
        {
            let guard = db.connection();
            for (id, key) in [("k1", "sk-live"), ("k2", "sk-gone")] {
                heterion_router_db::repos::insert_api_key(
                    &guard,
                    &heterion_router_db::repos::ApiKeyRow {
                        id: id.to_string(),
                        name: id.to_string(),
                        key: key.to_string(),
                        allowed_models: "[\"*\"]".to_string(),
                        no_log: false,
                    },
                )
                .unwrap();
            }
        }
        let router = cached(backend.clone(), db.clone());
        assert_eq!(
            call(router.clone(), prompt("hello"), Some("sk-gone"))
                .await
                .cache(),
            "miss"
        );
        assert_eq!(
            call(router.clone(), prompt("hello"), Some("sk-gone"))
                .await
                .cache(),
            "hit"
        );
        // Revoked after the hit, so the row it wrote is still in the table:
        // the key must lose its own cached answers too.
        heterion_router_db::repos::set_api_key_revoked(&db.connection(), "k2", true).unwrap();
        let revoked = call(router, prompt("hello"), Some("sk-gone")).await;
        assert_eq!(
            revoked.status,
            StatusCode::UNAUTHORIZED,
            "a revoked key gets no hit, not even of its own answer"
        );
    }

    #[tokio::test]
    async fn without_a_database_nothing_is_cached() {
        let backend = CountingBackend::new();
        let state = AppState::new(Arc::new(backend.clone()));
        let router = build_router_with(state.clone(), CacheLayer::new(&state));
        assert_eq!(
            call(router.clone(), prompt("hello"), None).await.cache(),
            "<none>"
        );
        assert_eq!(call(router, prompt("hello"), None).await.cache(), "<none>");
        assert_eq!(backend.calls(), 2);
    }

    #[tokio::test]
    async fn the_exact_key_is_stable_across_insertion_orders() {
        let first: Value = serde_json::from_str(
            r#"{"messages":[{"content":"hi","role":"user"}],"model":"gpt-test","temperature":0.2}"#,
        )
        .unwrap();
        let second: Value = serde_json::from_str(
            r#"{"temperature":0.2,"model":"gpt-test","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .unwrap();
        assert_eq!(
            first.as_object().unwrap().keys().collect::<Vec<_>>(),
            second.as_object().unwrap().keys().collect::<Vec<_>>(),
            "serde_json already normalises the order; the point is the hash does too",
        );
        let a = CacheKey::new("ns", "/v1/chat/completions", &first);
        let b = CacheKey::new("ns", "/v1/chat/completions", &second);
        assert_eq!(a.signature(), b.signature());
        assert_eq!(a.prompt_hash(), b.prompt_hash());
        assert_eq!(
            a.signature(),
            "x:ns:v1:chat:completions:".to_string() + a.prompt_hash()
        );
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let value: Value =
            serde_json::from_str(r#"{"b":[3,{"z":1,"y":{"q":0,"p":1}}],"a":2}"#).unwrap();
        assert_eq!(
            canonical_json(&value),
            r#"{"a":2,"b":[3,{"y":{"p":1,"q":0},"z":1}]}"#
        );
        assert_eq!(canonical_json(&json!({"a": "x\"y"})), r#"{"a":"x\"y"}"#);
    }

    #[test]
    fn user_metadata_and_tool_differences_do_not_collide() {
        let base = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        let mut with_user = base.clone();
        with_user["user"] = json!("alice");
        let mut other_user = base.clone();
        other_user["user"] = json!("bob");
        let mut with_meta = base.clone();
        with_meta["metadata"] = json!({"trace": "1"});
        let with_tools = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}], "tools": [{"type": "function"}]});
        let streamed =
            json!({"model": "m", "messages": [{"role": "user", "content": "hi"}], "stream": true});

        let keys: Vec<String> = [
            &base,
            &with_user,
            &other_user,
            &with_meta,
            &with_tools,
            &streamed,
        ]
        .iter()
        .map(|body| {
            CacheKey::new("ns", "/v1/chat/completions", body)
                .signature()
                .to_string()
        })
        .collect();
        let unique: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(
            unique.len(),
            keys.len(),
            "every difference is a different key: {keys:?}"
        );
    }

    #[test]
    fn the_verb_is_part_of_the_key() {
        let body = prompt("hello");
        assert_ne!(
            CacheKey::new("ns", "/v1/chat/completions", &body).signature(),
            CacheKey::new("ns", "/v1/messages", &body).signature(),
        );
    }

    #[test]
    fn semantic_mode_is_off_unless_an_embedding_model_is_configured() {
        let state = AppState::new(Arc::new(CountingBackend::new()));
        let layer = CacheLayer::new(&state);
        assert!(
            matches!(layer.semantic_status(), SemanticStatus::Off(reason) if !reason.is_empty())
        );
    }

    #[tokio::test]
    async fn a_near_neighbour_is_served_from_the_cache() {
        let backend = CountingBackend::new();
        let db = db();
        // First match wins, so the longer needle comes first: "alpha with a
        // different tail" also contains "alpha".
        let embedder = StubEmbedder::new(vec![
            (
                "alpha with a different tail".to_string(),
                vec![0.999, 0.03, 0.0],
            ),
            ("alpha".to_string(), vec![1.0, 0.0, 0.0]),
            ("opposite".to_string(), vec![-1.0, 0.0, 0.0]),
        ]);
        let state = AppState::with_db(Arc::new(backend.clone()), db.clone());
        let semantic = CacheLayer::new(&state).with_semantic("test/embed", embedder);
        let router = build_router_with(state.clone(), semantic);

        let first = call(router.clone(), prompt("alpha"), None).await;
        assert_eq!(first.cache(), "miss");
        let second = call(router.clone(), prompt("alpha with a different tail"), None).await;
        assert_eq!(second.cache(), "hit");
        assert_eq!(
            second
                .headers
                .get(HEADER_SIMILARITY)
                .and_then(|v| v.to_str().ok()),
            Some("0.9995"),
            "a semantic hit reports how close it was",
        );
        assert!(
            second.text.contains("echo: alpha") && !second.text.contains("different tail"),
            "a semantic hit answers a DIFFERENT prompt",
        );
        assert_eq!(backend.calls(), 1);

        let far = call(router, prompt("opposite"), None).await;
        assert_eq!(far.cache(), "miss", "cosine -1 is below any threshold");
        assert_eq!(backend.calls(), 2);
    }

    #[tokio::test]
    async fn a_semantic_hit_below_the_threshold_is_a_miss() {
        let backend = CountingBackend::new();
        let db = db();
        let embedder = StubEmbedder::new(vec![
            ("alpha".to_string(), vec![1.0, 0.0]),
            ("beta".to_string(), vec![0.8, 0.6]),
        ]);
        let state = AppState::with_db(Arc::new(backend.clone()), db);
        let semantic = CacheLayer::new(&state).with_semantic("test/embed", embedder);
        let router = build_router_with(state.clone(), semantic);
        assert_eq!(
            call(router.clone(), prompt("alpha"), None).await.cache(),
            "miss"
        );
        assert_eq!(call(router, prompt("beta"), None).await.cache(), "miss");
        assert_eq!(backend.calls(), 2);
    }

    #[tokio::test]
    async fn a_failing_embedder_degrades_semantic_mode_to_a_miss() {
        let backend = CountingBackend::new();
        let db = db();
        let state = AppState::with_db(Arc::new(backend.clone()), db);
        let semantic = CacheLayer::new(&state).with_semantic("test/embed", StubEmbedder::failing());
        let router = build_router_with(state.clone(), semantic);
        assert_eq!(
            call(router.clone(), prompt("alpha"), None).await.cache(),
            "miss"
        );
        // Exact mode still works: the request was stored on the first miss.
        assert_eq!(call(router, prompt("alpha"), None).await.cache(), "hit");
        assert_eq!(backend.calls(), 1);
    }

    #[tokio::test]
    async fn a_tool_calling_request_is_never_semantically_cached() {
        let backend = CountingBackend::new();
        let db = db();
        let embedder = StubEmbedder::new(vec![(String::new(), vec![1.0, 0.0])]);
        let state = AppState::with_db(Arc::new(backend.clone()), db);
        let semantic = CacheLayer::new(&state).with_semantic("test/embed", embedder);
        let router = build_router_with(state.clone(), semantic);
        let tools = json!({
            "model": "gpt-test",
            "messages": [{"role": "user", "content": "hello"}],
            "tools": [{"type": "function", "function": {"name": "f"}}]
        });
        let plain = prompt("hello");
        assert_eq!(call(router.clone(), tools, None).await.cache(), "miss");
        assert_eq!(
            call(router, plain, None).await.cache(),
            "miss",
            "an answer to a plain prompt must not come from a tool-calling entry",
        );
        assert_eq!(backend.calls(), 2);
    }

    #[test]
    fn stored_tokens_are_counted_from_the_upstream_usage() {
        assert_eq!(
            tokens_saved_by(br#"{"usage":{"prompt_tokens":11,"completion_tokens":5}}"#),
            16
        );
        assert_eq!(
            tokens_saved_by(br#"{"usage":{"input_tokens":3,"output_tokens":4}}"#),
            7
        );
        assert_eq!(tokens_saved_by(br#"{}"#), 0);
        assert_eq!(tokens_saved_by(b"not json"), 0);
    }
}
