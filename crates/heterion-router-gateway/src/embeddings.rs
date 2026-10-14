//! `POST /v1/embeddings` — the OpenAI-compatible embeddings surface.
//!
//! Roadmap M2. An embedding call is *not* a chat completion: it has no
//! messages, no choices and no stream, and the answer is vectors. So this is a
//! dedicated path rather than a defaulted method on [`crate::backend::ChatBackend`]:
//! every deployment routes chat through
//! [`crate::routing::RoutingBackend`], so a trait method it does not implement
//! would ship an endpoint that is dead in production while looking alive in
//! tests.
//!
//! Two rules keep it from quietly becoming something else:
//!
//! * **The model id must name its provider** (`provider/model`). The chat
//!   route rules, prefix fallbacks and combos all name *chat* models, and
//!   reusing them here is exactly how an embedding request would end up served
//!   as a completion.
//! * **Only providers whose registry wire format is `openai` are asked.** A
//!   `claude` / `gemini` / `openai-responses` provider has no
//!   `/v1/embeddings` endpoint here, and there is nothing to guess: an
//!   unknown model or an unsupporting provider is a clean `404`, never a
//!   fallback to a chat model.
//!
//! The registry declares no embedding models (`heterion-local` lists only its
//! chat `default`), so capability is the URL/format contract above rather than
//! a per-model flag; a model a provider does not know is refused *by that
//! provider*, and its refusal is forwarded as the status it carried.
//!
//! Accounting: an embedding call spends real provider input tokens and
//! produces none, so it writes one `usage_history` row (`tokens_input` set,
//! `tokens_output` zero) and one `call_logs` row, naming the provider and
//! connection this path resolved — never the router.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use heterion_router_core::GatewayError;
use heterion_router_crypto::{FieldCrypto, load_secret};
use heterion_router_http::{HttpClient, HttpTimeouts};
use heterion_router_providers::ProviderRegistry;
use heterion_router_usage::{CallLogRecord, UsageRecord, record_call_log, record_usage};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::Authenticated;
use crate::credentials;
use crate::ids;
use crate::openai::upstream_refusal;
use crate::{
    AppState, JsonBody, error_response, error_status, forbidden_response, with_recovered_status,
};

/// Call-log path for every row this module writes.
const CALL_LOG_PATH: &str = "/v1/embeddings";

/// `input` of an embeddings request: one string, or a batch of them.
///
/// Untagged, so a single string and an array keep the exact wire shape they
/// arrived in — which is what the upstream has to be sent back. Token-id
/// arrays are not accepted: this path forwards text, it does not tokenize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    /// One input, answered with one vector.
    Text(String),
    /// Several inputs, answered in order.
    Batch(Vec<String>),
}

impl EmbeddingInput {
    /// How many inputs there are, i.e. how many vectors the answer must carry.
    pub fn count(&self) -> usize {
        match self {
            Self::Text(_) => 1,
            Self::Batch(items) => items.len(),
        }
    }
}

/// `POST /v1/embeddings` request body.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingRequest {
    /// Embedding model, as `provider/model`.
    pub model: String,
    /// One input or a batch.
    pub input: EmbeddingInput,
    /// `"float"` (default) or `"base64"`.
    #[serde(default)]
    pub encoding_format: Option<String>,
    /// Requested vector width.
    #[serde(default)]
    pub dimensions: Option<u32>,
    /// Opaque client tag, forwarded untouched.
    #[serde(default)]
    pub user: Option<String>,
}

/// How the vectors are spelled on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncodingFormat {
    /// `[0.1, -0.2, ...]` — what OpenAI calls `float`.
    Float,
    /// base64 of the little-endian `f32` bytes — what OpenAI calls `base64`.
    Base64,
}

impl EncodingFormat {
    /// Parse the request field, defaulting to `float`.
    fn parse(value: Option<&str>) -> Result<Self, GatewayError> {
        match value.unwrap_or("float") {
            "float" => Ok(Self::Float),
            "base64" => Ok(Self::Base64),
            other => Err(GatewayError::InvalidRequest(format!(
                "encoding_format must be \"float\" or \"base64\", got {other:?}"
            ))),
        }
    }
}

/// Where one embedding call goes, and who pays for it.
struct EmbeddingTarget {
    /// Canonical provider id, as it appears in `usage_history.provider`.
    provider: String,
    /// Model as the provider knows it (the `provider/` prefix removed).
    model: String,
    /// Upstream `/v1/embeddings` URL.
    url: String,
    /// Connection the credential came from, when it came from one.
    connection_id: Option<String>,
    /// Bearer token, `None` for a keyless provider.
    bearer: Option<String>,
}

/// The upstream payload reduced to what the client asked for.
struct EmbeddingResponse {
    /// Model that produced the vectors; empty when the provider named none.
    model: String,
    /// One vector per input, in input order.
    vectors: Vec<Vec<f32>>,
    /// Input tokens the provider reported.
    prompt_tokens: u32,
}

/// `POST /v1/embeddings`.
pub async fn embeddings(
    State(state): State<AppState>,
    auth: Authenticated,
    JsonBody(request): crate::JsonBody<EmbeddingRequest>,
) -> Response {
    if !auth.allows_model(&request.model) {
        return forbidden_response("model is not allowed for this api key");
    }
    let format = match EncodingFormat::parse(request.encoding_format.as_deref()) {
        Ok(format) => format,
        Err(error) => return error_response(error),
    };
    if request.input.count() == 0 {
        return error_response(GatewayError::InvalidRequest(
            "input must hold at least one string".to_string(),
        ));
    }
    if request.dimensions == Some(0) {
        return error_response(GatewayError::InvalidRequest(
            "dimensions must be greater than zero".to_string(),
        ));
    }
    let target = match resolve_target(&state, &request.model) {
        Ok(target) => target,
        // Resolved before anything is dispatched: an unknown model never
        // reaches a provider.
        Err(error) => return error_response(error),
    };
    dispatch(&state, &auth, &request, &target, format).await
}

/// The OpenAI `/v1/embeddings` URL for a registry `base_url`.
///
/// Registry entries are versioned roots (`https://api.openai.com/v1`) *or*
/// full chat endpoints (`https://api.openai.com/v1/chat/completions`), so
/// the chat suffix is replaced rather than appended to — the same rule
/// `openai::resolve_responses_url` applies.
fn embeddings_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let root = trimmed
        .strip_suffix("/chat/completions")
        .or_else(|| trimmed.strip_suffix("/responses"))
        .unwrap_or(trimmed);
    if root.ends_with("/embeddings") {
        root.to_string()
    } else {
        format!("{root}/embeddings")
    }
}

/// Resolve `provider/model` to a provider, a URL and a credential.
fn resolve_target(state: &AppState, model: &str) -> Result<EmbeddingTarget, GatewayError> {
    let Some(registry) = state.registry.as_ref() else {
        return Err(GatewayError::UnknownModel(format!(
            "{model} (the gateway has no provider catalog)"
        )));
    };
    // No prefix rule, no combo, no default backend: those name chat models,
    // and this is the whole point of the endpoint — an embedding request is
    // never answered by a completion.
    let Some((prefix, model_id)) = model
        .split_once('/')
        .filter(|(prefix, model)| !prefix.is_empty() && !model.is_empty())
    else {
        return Err(GatewayError::UnknownModel(format!(
            "{model} (an embedding model id must name its provider: provider/model)"
        )));
    };
    let Some(provider) = registry.canonical_id(prefix) else {
        return Err(GatewayError::UnknownModel(format!(
            "{model} (unknown provider {prefix})"
        )));
    };
    let base_url = embedding_support(registry, provider)
        .map_err(|reason| GatewayError::UnknownModel(format!("{model} ({reason})")))?;
    let (bearer, connection_id) = credential(state, provider);
    // Keyless providers answer anonymously; a provider that needs a key and
    // has none is refused here rather than as a confusing upstream 401.
    if bearer.is_none() && !registry.allows_keyless(provider) {
        return Err(GatewayError::UnknownModel(format!(
            "{model} (provider {provider} has no usable credential configured)"
        )));
    }
    Ok(EmbeddingTarget {
        provider: provider.to_string(),
        model: model_id.to_string(),
        url: embeddings_url(&base_url),
        connection_id,
        bearer,
    })
}

/// Serve one resolved call: upstream round trip, accounting, client response.
async fn dispatch(
    state: &AppState,
    auth: &Authenticated,
    request: &EmbeddingRequest,
    target: &EmbeddingTarget,
    format: EncodingFormat,
) -> Response {
    let started = std::time::Instant::now();
    let outcome = match http_client() {
        Ok(http) => call_upstream(http, target, request).await,
        Err(error) => Err(error),
    };
    match outcome {
        Ok(response) => {
            let latency_ms = elapsed_ms(started);
            tracing::info!(
                "embedding ok provider={} model={} vectors={} pt={} ms={latency_ms}",
                target.provider,
                target.model,
                response.vectors.len(),
                response.prompt_tokens,
            );
            record(
                state,
                auth,
                &Booked {
                    request,
                    target,
                    tokens_in: response.prompt_tokens,
                    success: true,
                    status: 200,
                    latency_ms,
                    error_code: None,
                },
            );
            let mut served = Json(client_value(format, &response)).into_response();
            // Same header the chat surfaces carry, out of the same table: an
            // embedding price is an input price with a zero output side.
            if let Some(cost) =
                state
                    .prices
                    .cost_usd(&target.model, response.prompt_tokens, 0, 0, 0, 0)
                && let Ok(value) = axum::http::HeaderValue::from_str(&format!("{cost:.6}"))
            {
                served
                    .headers_mut()
                    .insert("x-heterion-router-cost-usd", value);
            }
            served
        }
        Err(error) => {
            // Normalized here for the same reason as in `chat_completions`: a
            // refusal delivered inside a 200 body leaves no
            // `upstream status {code}:` prefix to parse, and the client and
            // the call log must both see the status it really carried.
            let error = with_recovered_status(error);
            let latency_ms = elapsed_ms(started);
            tracing::warn!(
                "embedding failed provider={} model={}: {error}",
                target.provider,
                target.model
            );
            record(
                state,
                auth,
                &Booked {
                    request,
                    target,
                    // A refused call reported no tokens at all.
                    tokens_in: 0,
                    success: false,
                    status: i64::from(error_status(&error).as_u16()),
                    latency_ms,
                    error_code: Some("upstream_error"),
                },
            );
            error_response(error)
        }
    }
}

/// POST the request upstream and read the vectors back.
async fn call_upstream(
    http: &HttpClient,
    target: &EmbeddingTarget,
    request: &EmbeddingRequest,
) -> Result<EmbeddingResponse, GatewayError> {
    let mut body = json!({
        "model": target.model,
        "input": &request.input,
        // Always float upstream, whatever the client asked for: one decode
        // path in and one encode path out, so `encoding_format` never depends
        // on what a given provider happens to support.
        "encoding_format": "float",
    });
    if let Some(dimensions) = request.dimensions {
        body["dimensions"] = json!(dimensions);
    }
    if let Some(user) = request.user.as_deref() {
        body["user"] = json!(user);
    }
    let response = http
        .post_json(&target.url, target.bearer.as_deref(), &body)
        .await
        .map_err(|error| GatewayError::Upstream(error.to_string()))?;
    // The body is never quoted into the error: a provider error page can hold
    // anything, and this string is what the sanitizer has to keep away from
    // the client anyway.
    let payload: Value = response
        .json()
        .await
        .map_err(|_| GatewayError::Upstream("embedding response is not json".to_string()))?;
    // A refusal delivered in a 200 body: the provider answered, it just said
    // no, and the payload is the only place the status exists.
    if let Some(error) = payload.get("error").filter(|error| error.is_object()) {
        return Err(GatewayError::Upstream(upstream_refusal(error)));
    }
    let mut response = parse_response(&payload, request.input.count())?;
    // A `dimensions` the provider quietly ignored would hand the caller a
    // vector of the wrong width, which nothing downstream can detect. Refused
    // rather than truncated: only a Matryoshka model may shorten a vector, and
    // only one that says so.
    if let (Some(requested), Some(vector)) = (request.dimensions, response.vectors.first())
        && vector.len() != usize::try_from(requested).unwrap_or(usize::MAX)
    {
        return Err(GatewayError::Upstream(format!(
            "provider returned {} dimensions for a dimensions={requested} request",
            vector.len()
        )));
    }
    if response.model.is_empty() {
        response.model = target.model.clone();
    }
    Ok(response)
}

/// Reduce an upstream `/v1/embeddings` payload to ordered vectors.
fn parse_response(payload: &Value, expected: usize) -> Result<EmbeddingResponse, GatewayError> {
    let data = payload
        .get("data")
        .and_then(Value::as_array)
        .filter(|data| !data.is_empty())
        .ok_or_else(|| {
            GatewayError::Upstream("embedding response carried no vectors".to_string())
        })?;
    let mut indexed = data
        .iter()
        .enumerate()
        .map(|(position, entry)| {
            let index = entry
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .unwrap_or(position);
            let vector = entry
                .get("embedding")
                .ok_or_else(|| {
                    GatewayError::Upstream("embedding entry carries no embedding".to_string())
                })
                .and_then(parse_vector)?;
            Ok((index, vector))
        })
        .collect::<Result<Vec<(usize, Vec<f32>)>, GatewayError>>()?;
    // Ordered by the provider's own index: a caller maps input *n* to vector
    // *n*, and arrival order is not that.
    indexed.sort_by_key(|(index, _)| *index);
    let vectors: Vec<Vec<f32>> = indexed.into_iter().map(|(_, vector)| vector).collect();
    // A count that does not match the input is the one failure a caller cannot
    // detect — it poisons a vector store in silence — so it is refused.
    if vectors.len() != expected {
        return Err(GatewayError::Upstream(format!(
            "provider returned {} vectors for {expected} input(s)",
            vectors.len()
        )));
    }
    let prompt_tokens = payload
        .get("usage")
        .and_then(|usage| {
            usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
        })
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32;
    Ok(EmbeddingResponse {
        model: payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        vectors,
        prompt_tokens,
    })
}

/// One vector out of a payload entry: a float array, or base64 `f32` bytes.
fn parse_vector(value: &Value) -> Result<Vec<f32>, GatewayError> {
    if let Some(numbers) = value.as_array() {
        return numbers
            .iter()
            .map(|number| {
                number.as_f64().map(|number| number as f32).ok_or_else(|| {
                    GatewayError::Upstream("embedding value is not a number".to_string())
                })
            })
            .collect();
    }
    // Providers that answer base64 whatever they were asked for; the bytes are
    // little-endian `f32` in both directions.
    if let Some(encoded) = value.as_str() {
        let bytes = base64_decode(encoded)
            .ok_or_else(|| GatewayError::Upstream("embedding payload is not base64".to_string()))?;
        return floats_from_bytes(&bytes);
    }
    Err(GatewayError::Upstream(
        "embedding is neither a float array nor base64".to_string(),
    ))
}

/// The client-facing `list` object.
fn client_value(format: EncodingFormat, response: &EmbeddingResponse) -> Value {
    let data: Vec<Value> = response
        .vectors
        .iter()
        .enumerate()
        .map(|(index, vector)| {
            json!({
                "object": "embedding",
                "index": index,
                "embedding": match format {
                    EncodingFormat::Float => json!(vector),
                    EncodingFormat::Base64 => json!(base64_encode(vector)),
                },
            })
        })
        .collect();
    json!({
        "object": "list",
        "data": data,
        "model": response.model,
        // Embeddings have no completion side, so the totals are the input.
        "usage": {
            "prompt_tokens": response.prompt_tokens,
            "total_tokens": response.prompt_tokens,
        },
    })
}

/// What one embedding call cost, for the two rows [`record`] writes.
struct Booked<'a> {
    request: &'a EmbeddingRequest,
    target: &'a EmbeddingTarget,
    tokens_in: u32,
    success: bool,
    status: i64,
    latency_ms: u32,
    error_code: Option<&'a str>,
}

/// Book one embedding call: one `usage_history` row, one `call_logs` row.
///
/// Same two tables the chat path writes, because an embedding call spends real
/// provider input tokens (the semantic cache exists because of them) and
/// produces none, so `tokens_output` stays zero. Rows go through
/// [`record_usage`]/[`record_call_log`] directly instead of the shared chat
/// writer because that one picks the call-log path between two chat endpoints
/// only — an embeddings row filed under `/v1/chat/completions` would be a lie
/// in the audit trail. No double-counting is possible: an embeddings request
/// never enters the chat path, so exactly one row pair is written per call.
fn record(state: &AppState, auth: &Authenticated, booked: &Booked<'_>) {
    if auth.key.as_ref().is_some_and(|key| key.no_log) {
        return;
    }
    let Some(db) = state.db.as_ref() else {
        return;
    };
    let Booked {
        request,
        target,
        tokens_in,
        success,
        status,
        latency_ms,
        error_code,
    } = *booked;
    let guard = db.connection();
    let usage = UsageRecord {
        // The provider that answered, not the router that picked it: usage is
        // per-provider analytics.
        provider: Some(target.provider.clone()),
        model: Some(target.model.clone()),
        connection_id: target.connection_id.clone(),
        api_key_id: auth.key.as_ref().map(|key| key.id.clone()),
        api_key_name: auth.key.as_ref().map(|key| key.name.clone()),
        tokens_input: tokens_in,
        // An embedding produces no completion tokens, so the column the chat
        // path fills stays at zero rather than being left out.
        tokens_output: 0,
        status: Some(if success { "succeeded" } else { "failed" }.to_string()),
        success,
        latency_ms,
        error_code: error_code.map(str::to_string),
        ..Default::default()
    };
    if let Err(error) = record_usage(&guard, &usage) {
        tracing::warn!("embedding usage record failed: {error}");
    }
    let call = CallLogRecord {
        id: ids::call_id(),
        method: "POST".to_string(),
        path: CALL_LOG_PATH.to_string(),
        status,
        model: Some(target.model.clone()),
        requested_model: Some(request.model.clone()),
        provider: Some(target.provider.clone()),
        connection_id: target.connection_id.clone(),
        duration: latency_ms,
        tokens_in,
        tokens_out: 0,
        api_key_id: auth.key.as_ref().map(|key| key.id.clone()),
        api_key_name: auth.key.as_ref().map(|key| key.name.clone()),
        error_summary: error_code.map(str::to_string),
        request_type: Some("embeddings".to_string()),
        ..Default::default()
    };
    if let Err(error) = record_call_log(&guard, &call) {
        tracing::warn!("embedding call log record failed: {error}");
    }
}

/// Shared client: one connection pool for the whole process, built with the
/// three budgets split apart. Embeddings answer in one body, so `response`
/// bounds the whole exchange (a large batch on a local model is not a
/// long-poll) and `idle` only guards a body that stopped arriving.
fn http_client() -> Result<&'static HttpClient, GatewayError> {
    static CLIENT: OnceLock<Result<HttpClient, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            HttpClient::with_timeouts(HttpTimeouts {
                connect: Duration::from_secs(10),
                response: Duration::from_secs(600),
                idle: Duration::from_secs(120),
            })
            .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| GatewayError::Upstream(format!("http client unavailable: {error}")))
}

/// Field-encryption key for stored provider credentials, derived once.
///
/// [`FieldCrypto::from_secret`] runs scrypt twice at N=16384, which is not a
/// per-request price; the gateway's data dir is fixed at startup, so the first
/// resolution is the only one.
fn field_crypto(data_dir: Option<&Path>) -> Option<&'static FieldCrypto> {
    static CRYPTO: OnceLock<Option<FieldCrypto>> = OnceLock::new();
    CRYPTO
        .get_or_init(|| load_secret(data_dir).and_then(|secret| FieldCrypto::from_secret(&secret)))
        .as_ref()
}

/// One credential for `provider`: the highest-priority active one.
///
/// `(bearer, connection_id)`; both `None` for a keyless provider, which is
/// the case that must send no `Authorization` header at all.
fn credential(state: &AppState, provider: &str) -> (Option<String>, Option<String>) {
    let Some(db) = state.db.as_ref() else {
        return (None, None);
    };
    let guard = db.connection();
    match credentials::load_credentials(&guard, provider, field_crypto(state.data_dir.as_deref())) {
        Ok(loaded) => loaded.first().map_or((None, None), |first| {
            (Some(first.token.clone()), Some(first.connection_id.clone()))
        }),
        Err(error) => {
            tracing::warn!("embedding credential read failed: {error}");
            (None, None)
        }
    }
}

/// Whether `provider` can answer an OpenAI-shaped `/v1/embeddings` call.
fn embedding_support(registry: &ProviderRegistry, provider: &str) -> Result<String, String> {
    if !registry.is_openai_format(provider) {
        return Err(format!(
            "provider {provider} has no /v1/embeddings support (wire format {})",
            registry
                .get(provider)
                .and_then(|entry| entry.format.as_deref())
                .unwrap_or("unknown")
        ));
    }
    registry
        .base_url(provider)
        .map(str::to_string)
        .ok_or_else(|| format!("provider {provider} declares no base_url"))
}

/// Milliseconds since `started`, clamped to what the columns hold.
fn elapsed_ms(started: std::time::Instant) -> u32 {
    started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
}

/// Standard base64 alphabet (RFC 4648 §4), which is what OpenAI's `base64`
/// encoding format uses.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// base64 of the little-endian `f32` bytes of `vector`.
fn base64_encode(vector: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let packed = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(BASE64[(packed >> 18 & 63) as usize] as char);
        encoded.push(BASE64[(packed >> 12 & 63) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            BASE64[(packed >> 6 & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            BASE64[(packed & 63) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

/// One base64 character's value.
fn base64_value(byte: u8) -> Option<u8> {
    Some(match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

/// Decode base64, ignoring padding and whitespace.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    let mut packed = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        packed = (packed << 6) | u32::from(base64_value(byte)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((packed >> bits) as u8);
        }
    }
    Some(bytes)
}

/// Little-endian `f32` bytes back into a vector.
fn floats_from_bytes(bytes: &[u8]) -> Result<Vec<f32>, GatewayError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(GatewayError::Upstream(format!(
            "embedding payload is not a whole number of f32 values ({} bytes)",
            bytes.len()
        )));
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use heterion_router_core::{ChatCompletionRequest, ChatMessage};
    use heterion_router_db::Db;
    use heterion_router_routing::{RouteRule, Router};
    use wiremock::matchers::{body_json, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::auth::AuthKey;
    use crate::backend::{ChatBackend, EchoBackend};
    use crate::routing::RoutingBackend;

    /// Canary planted in an upstream body: it must never reach a client.
    const CANARY: &str = "CANARY-upstream-body-secret";

    fn open_auth() -> Authenticated {
        Authenticated { key: None }
    }

    fn restricted_auth(allowed_models: &str, no_log: bool) -> Authenticated {
        Authenticated {
            key: Some(AuthKey {
                id: "key-1".to_string(),
                name: "test key".to_string(),
                scopes: "[\"chat\"]".to_string(),
                allowed_models: allowed_models.to_string(),
                allowed_combos: "[]".to_string(),
                no_log,
            }),
        }
    }

    /// State wired the way `main.rs` wires it: in-memory database, the real
    /// provider catalog, and a routing backend whose breaker trips on the
    /// first *counted* failure — so any failure the chat path would have
    /// booked shows up as a cooldown on the next chat request.
    fn open() -> (AppState, Arc<Db>) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert("heterion-local".to_string(), Arc::new(EchoBackend));
        let router = Router::new(
            vec![RouteRule {
                model_prefix: "probe-".to_string(),
                backends: vec!["heterion-local".to_string()],
            }],
            vec![],
        );
        let backend = Arc::new(RoutingBackend::with_breaker_settings(
            backends, router, 1, 3600,
        ));
        let state = AppState::with_db(backend, db.clone()).with_catalog(
            Arc::new(ProviderRegistry::load()),
            vec!["heterion-local".to_string()],
        );
        (state, db)
    }

    /// A target aimed at `server`, as [`resolve_target`] would build it for a
    /// provider whose `base_url` is the mock.
    fn target(server: &MockServer, bearer: Option<&str>) -> EmbeddingTarget {
        EmbeddingTarget {
            provider: "heterion-local".to_string(),
            model: "e5-small".to_string(),
            url: embeddings_url(&format!("{}/v1", server.uri())),
            connection_id: Some("conn-1".to_string()),
            bearer: bearer.map(str::to_string),
        }
    }

    fn request(body: Value) -> EmbeddingRequest {
        serde_json::from_value(body).expect("valid embeddings request")
    }

    /// Call the handler as the router would, and read status + body.
    async fn handle(state: &AppState, auth: Authenticated, body: Value) -> (StatusCode, Value) {
        let response = embeddings(State(state.clone()), auth, JsonBody(request(body))).await;
        read(response).await
    }

    /// Call the handler for a mock-backed provider, bypassing resolution.
    async fn call(
        state: &AppState,
        server: &MockServer,
        bearer: Option<&str>,
        format: EncodingFormat,
        body: Value,
    ) -> (StatusCode, Value) {
        let request = request(body);
        let response = dispatch(
            state,
            &open_auth(),
            &request,
            &target(server, bearer),
            format,
        )
        .await;
        read(response).await
    }

    async fn read(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Upstream payload with `count` vectors, reported out of order on
    /// purpose: the answer must be ordered by `index`, not by arrival.
    fn upstream(count: usize) -> Value {
        let data: Vec<Value> = (0..count)
            .rev()
            .map(|position| {
                json!({
                    "object": "embedding",
                    "index": position,
                    "embedding": [position as f32 + 0.5, -1.25],
                })
            })
            .collect();
        json!({
            "object": "list",
            "data": data,
            "model": "e5-small",
            "usage": {"prompt_tokens": 4, "total_tokens": 4},
        })
    }

    /// Mock `/v1/embeddings` answering `payload` with HTTP 200.
    async fn serving(server: &MockServer, payload: Value) {
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(payload))
            .expect(1)
            .mount(server)
            .await;
    }

    fn floats(vector: &[f32]) -> Vec<f32> {
        vector.to_vec()
    }

    /// Whether a chat request still gets served, i.e. whether the breaker for
    /// the provider is still closed.
    async fn chat_still_served(state: &AppState) -> bool {
        state
            .backend
            .complete(ChatCompletionRequest {
                model: "probe-1".to_string(),
                messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
                ..Default::default()
            })
            .await
            .is_ok()
    }

    /// `(provider, connection_id, tokens_input, tokens_output, success)` of the
    /// single booked row.
    fn usage_row(db: &Db) -> (Option<String>, Option<String>, u32, u32, bool) {
        let guard = db.connection();
        guard
            .query_row(
                "SELECT provider, connection_id, tokens_input, tokens_output, success
                 FROM usage_history LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, u32>(2)?,
                        row.get::<_, u32>(3)?,
                        row.get::<_, i64>(4)? != 0,
                    ))
                },
            )
            .expect("a booked usage row")
    }

    /// Rows in `usage_history`.
    fn usage_count(db: &Db) -> i64 {
        let guard = db.connection();
        guard
            .query_row("SELECT COUNT(*) FROM usage_history", [], |row| row.get(0))
            .expect("count")
    }

    /// `(path, status, model, requested_model, tokens_in, tokens_out, request_type)`
    /// of the single logged call.
    fn call_log_row(
        db: &Db,
    ) -> (
        String,
        i64,
        Option<String>,
        Option<String>,
        u32,
        u32,
        String,
    ) {
        let guard = db.connection();
        guard
            .query_row(
                "SELECT path, status, model, requested_model, tokens_in, tokens_out, request_type
                 FROM call_logs LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, u32>(4)?,
                        row.get::<_, u32>(5)?,
                        row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    ))
                },
            )
            .expect("a booked call log")
    }

    /// Rows in `call_logs`.
    fn call_log_count(db: &Db) -> i64 {
        let guard = db.connection();
        guard
            .query_row("SELECT COUNT(*) FROM call_logs", [], |row| row.get(0))
            .expect("count")
    }

    #[tokio::test]
    async fn a_single_string_input_yields_one_float_embedding() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(body_json(json!({
                "model": "e5-small",
                "input": "hello",
                "encoding_format": "float",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "data": [{"object": "embedding", "index": 0, "embedding": [0.25, -0.5, 1.5]}],
                "model": "e5-small",
                "usage": {"prompt_tokens": 4, "total_tokens": 4},
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (state, _db) = open();

        let (status, body) = call(
            &state,
            &server,
            None,
            EncodingFormat::Float,
            json!({"model": "heterion-local/e5-small", "input": "hello"}),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["object"], json!("list"));
        assert_eq!(body["model"], json!("e5-small"));
        let data = body["data"].as_array().expect("data array");
        assert_eq!(data.len(), 1, "{body}");
        assert_eq!(data[0]["object"], json!("embedding"));
        assert_eq!(data[0]["index"], json!(0));
        // No `encoding_format` asked for means a float array, never base64.
        assert_eq!(data[0]["embedding"], json!([0.25, -0.5, 1.5]));
        assert_eq!(body["usage"]["prompt_tokens"], json!(4));
        assert_eq!(body["usage"]["total_tokens"], json!(4));
        server.verify().await;
    }

    #[tokio::test]
    async fn a_batch_input_yields_one_ordered_entry_per_input() {
        let server = MockServer::start().await;
        serving(&server, upstream(3)).await;
        let (state, _db) = open();

        let (status, body) = call(
            &state,
            &server,
            None,
            EncodingFormat::Float,
            json!({"model": "heterion-local/e5-small", "input": ["a", "b", "c"]}),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let data = body["data"].as_array().expect("data array").clone();
        assert_eq!(data.len(), 3, "{body}");
        for (position, entry) in data.iter().enumerate() {
            assert_eq!(entry["object"], json!("embedding"));
            assert_eq!(entry["index"], json!(position), "entry {position}: {body}");
        }
        // The provider reported them 2,1,0: the answer is ordered by index, so
        // input *n* keeps vector *n*.
        assert_eq!(data[0]["embedding"], json!([0.5, -1.25]));
        assert_eq!(data[1]["embedding"], json!([1.5, -1.25]));
        assert_eq!(data[2]["embedding"], json!([2.5, -1.25]));
    }

    #[tokio::test]
    async fn base64_encoding_is_decodable_little_endian_f32() {
        let server = MockServer::start().await;
        // The provider is always asked for floats, whatever the client wants:
        // one decode path upstream, one encode path downstream.
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(body_json(json!({
                "model": "e5-small",
                "input": "hello",
                "encoding_format": "float",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(upstream(1)))
            .expect(1)
            .mount(&server)
            .await;
        let (state, _db) = open();

        let (status, body) = call(
            &state,
            &server,
            None,
            EncodingFormat::Base64,
            json!({"model": "heterion-local/e5-small", "input": "hello", "encoding_format": "base64"}),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let encoded = body["data"][0]["embedding"]
            .as_str()
            .unwrap_or_else(|| panic!("base64 expected: {body}"))
            .to_string();
        let bytes = base64_decode(&encoded).expect("base64 decodes");
        assert_eq!(bytes.len(), 2 * 4, "two f32 values are eight bytes");
        assert_eq!(floats_from_bytes(&bytes).unwrap(), floats(&[0.5, -1.25]));
    }

    #[tokio::test]
    async fn a_provider_may_answer_in_base64_and_the_gateway_still_orders_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "data": [
                    {"object": "embedding", "index": 1, "embedding": base64_encode(&[9.5])},
                    {"object": "embedding", "index": 0, "embedding": base64_encode(&[8.5])},
                ],
                "model": "e5-small",
                "usage": {"prompt_tokens": 2, "total_tokens": 2},
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (state, _db) = open();

        let (status, body) = call(
            &state,
            &server,
            None,
            EncodingFormat::Float,
            json!({"model": "heterion-local/e5-small", "input": ["a", "b"]}),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let data = body["data"].as_array().expect("data array").clone();
        assert_eq!(data[0]["embedding"], json!([8.5]), "{body}");
        assert_eq!(data[1]["embedding"], json!([9.5]), "{body}");
    }

    /// An input↔vector mismatch is the one failure a caller cannot detect: it
    /// silently poisons a vector store. Refused instead of served.
    #[tokio::test]
    async fn a_vector_count_that_does_not_match_the_input_is_refused() {
        let server = MockServer::start().await;
        serving(&server, upstream(1)).await;
        let (state, _db) = open();

        let (status, body) = call(
            &state,
            &server,
            None,
            EncodingFormat::Float,
            json!({"model": "heterion-local/e5-small", "input": ["a", "b"]}),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert!(body.get("data").is_none(), "no vectors were served: {body}");
    }

    #[tokio::test]
    async fn dimensions_the_provider_ignored_are_refused_not_truncated() {
        let server = MockServer::start().await;
        serving(&server, upstream(1)).await;
        let (state, _db) = open();

        let (status, body) = call(
            &state,
            &server,
            None,
            EncodingFormat::Float,
            json!({"model": "heterion-local/e5-small", "input": "hello", "dimensions": 1}),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert!(body.get("data").is_none(), "no vectors were served: {body}");
    }

    #[tokio::test]
    async fn dimensions_are_forwarded_and_honoured() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(json!({
                "model": "e5-small",
                "input": "hello",
                "encoding_format": "float",
                "dimensions": 2,
                "user": "tenant-a",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "list",
                "data": [{"object": "embedding", "index": 0, "embedding": [0.5, -1.25]}],
                "model": "e5-small",
                "usage": {"prompt_tokens": 3, "total_tokens": 3},
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (state, _db) = open();

        let (status, body) = call(
            &state,
            &server,
            None,
            EncodingFormat::Float,
            json!({
                "model": "heterion-local/e5-small", "input": "hello",
                "dimensions": 2, "user": "tenant-a",
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"][0]["embedding"], json!([0.5, -1.25]));
        server.verify().await;
    }

    #[tokio::test]
    async fn an_unknown_model_is_a_clean_error_that_never_reaches_an_upstream() {
        let server = MockServer::start().await;
        let (state, db) = open();

        // No provider prefix: nothing to route it to, and chat route rules are
        // never consulted for an embedding request.
        let (status, body) = handle(
            &state,
            open_auth(),
            json!({"model": "text-embedding-3-small", "input": "hello"}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("provider/model")),
            "{body}"
        );

        // Unknown provider.
        let (status, body) = handle(
            &state,
            open_auth(),
            json!({"model": "no-such-provider/e5", "input": "hello"}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

        // Not one upstream call was made, and nothing was booked for either.
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "an unknown model reached a provider"
        );
        assert_eq!(usage_count(&db), 0, "an unserved request wrote a usage row");
        assert_eq!(
            call_log_count(&db),
            0,
            "an unserved request wrote a call log"
        );
    }

    #[tokio::test]
    async fn a_provider_without_embedding_support_is_refused_without_calling_upstream() {
        let server = MockServer::start().await;
        let (state, db) = open();

        // `agentrouter` is a `claude`-format provider: it has no
        // OpenAI-shaped /v1/embeddings here, and nothing falls back to chat.
        let (status, body) = handle(
            &state,
            open_auth(),
            json!({"model": "agentrouter/e5", "input": "hello"}),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let message = body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(message.contains("embeddings"), "{body}");
        assert!(message.contains("claude"), "{body}");
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "an unsupporting provider was called anyway"
        );
        assert_eq!(usage_count(&db), 0);
    }

    #[tokio::test]
    async fn a_model_the_api_key_does_not_allow_is_forbidden() {
        let server = MockServer::start().await;
        let (state, db) = open();

        let (status, body) = handle(
            &state,
            restricted_auth("[\"chat-only\"]", false),
            json!({"model": "heterion-local/e5-small", "input": "hello"}),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"]["code"], json!("forbidden"), "{body}");
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty()
        );
        assert_eq!(usage_count(&db), 0);
    }

    #[tokio::test]
    async fn a_refusal_in_a_200_body_keeps_its_status_and_leaves_the_breaker_alone() {
        let server = MockServer::start().await;
        // The only way a refusal arrives inside a 200: an upstream that reports
        // it in the payload. No `upstream status N:` prefix exists to parse.
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "error": {"message": "slow down", "code": "rate_limit_error", "detail": CANARY},
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (state, _db) = open();

        let (status, body) = call(
            &state,
            &server,
            None,
            EncodingFormat::Float,
            json!({"model": "heterion-local/e5-small", "input": "hello"}),
        )
        .await;

        // 429, not the 502 a blanket gateway failure would have produced.
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert_eq!(body["error"]["status"], json!(429), "{body}");
        assert!(
            !body.to_string().contains(CANARY),
            "the upstream body leaked: {body}"
        );
        // And it did not take the provider's chat traffic down with it: the
        // breaker is threshold-1, so a booked failure would have tripped it.
        assert!(
            chat_still_served(&state).await,
            "a 429 refusal tripped the breaker"
        );
    }

    #[tokio::test]
    async fn an_upstream_error_body_never_reaches_the_client() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({
                "error": {"message": CANARY, "type": "server_error"},
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (state, _db) = open();

        let (status, body) = call(
            &state,
            &server,
            None,
            EncodingFormat::Float,
            json!({"model": "heterion-local/e5-small", "input": "hello"}),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_eq!(body["error"]["message"], json!("upstream request failed"));
        assert!(
            !body.to_string().contains(CANARY),
            "the upstream body leaked: {body}"
        );
    }

    #[tokio::test]
    async fn a_keyless_call_sends_no_authorization_and_a_keyed_one_does() {
        let server = MockServer::start().await;
        // Any request carrying a bearer fails the call, as the keyless contract
        // in AGENTS.md requires.
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        serving(&server, upstream(1)).await;
        let (state, _db) = open();

        let (status, body) = call(
            &state,
            &server,
            None,
            EncodingFormat::Float,
            json!({"model": "heterion-local/e5-small", "input": "hello"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 1);
        assert!(
            sent[0].headers.get("authorization").is_none(),
            "a keyless call sent a bearer: {:?}",
            sent[0].headers
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn a_credentialed_call_sends_its_bearer() {
        let server = MockServer::start().await;
        serving(&server, upstream(1)).await;
        let (state, _db) = open();

        let (status, body) = call(
            &state,
            &server,
            Some("tok-abc"),
            EncodingFormat::Float,
            json!({"model": "heterion-local/e5-small", "input": "hello"}),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent[0].headers["authorization"], "Bearer tok-abc");
    }

    #[tokio::test]
    async fn an_embedding_call_writes_one_usage_row_naming_the_real_provider() {
        let server = MockServer::start().await;
        serving(&server, upstream(1)).await;
        let (state, db) = open();

        let (status, _body) = call(
            &state,
            &server,
            Some("tok"),
            EncodingFormat::Base64,
            json!({"model": "heterion-local/e5-small", "input": "hello"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Exactly one row pair per call: an embedding call never enters the
        // chat path, so nothing is booked twice.
        assert_eq!(usage_count(&db), 1, "usage rows");
        let (provider, connection, tokens_in, tokens_out, success) = usage_row(&db);
        assert_eq!(provider.as_deref(), Some("heterion-local"));
        assert_eq!(connection.as_deref(), Some("conn-1"));
        assert_eq!(tokens_in, 4);
        assert_eq!(tokens_out, 0, "embeddings produce no output tokens");
        assert!(success);

        assert_eq!(call_log_count(&db), 1, "call log rows");
        let (path, status, model, requested, tokens_in, tokens_out, request_type) =
            call_log_row(&db);
        assert_eq!(path, CALL_LOG_PATH);
        assert_eq!(status, 200);
        assert_eq!(model.as_deref(), Some("e5-small"));
        assert_eq!(requested.as_deref(), Some("heterion-local/e5-small"));
        assert_eq!(tokens_in, 4);
        assert_eq!(tokens_out, 0);
        assert_eq!(request_type, "embeddings");
    }

    #[tokio::test]
    async fn a_failed_embedding_call_is_booked_as_failed_with_its_own_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
            .expect(1)
            .mount(&server)
            .await;
        let (state, db) = open();

        let (status, _body) = call(
            &state,
            &server,
            None,
            EncodingFormat::Float,
            json!({"model": "heterion-local/e5-small", "input": "hello"}),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

        assert_eq!(usage_count(&db), 1);
        let (_provider, _connection, tokens_in, tokens_out, success) = usage_row(&db);
        assert_eq!(tokens_in, 0, "a refused call reported no tokens");
        assert_eq!(tokens_out, 0);
        assert!(!success);
        assert_eq!(call_log_count(&db), 1);
        let (_path, status, _model, _requested, _in, _out, _request_type) = call_log_row(&db);
        assert_eq!(status, 429, "the recovered status, not a blanket 502");
    }

    #[tokio::test]
    async fn a_no_log_key_books_nothing() {
        let server = MockServer::start().await;
        serving(&server, upstream(1)).await;
        let (state, db) = open();
        let request = request(json!({"model": "heterion-local/e5-small", "input": "hello"}));

        let response = dispatch(
            &state,
            &restricted_auth("[\"*\"]", true),
            &request,
            &target(&server, None),
            EncodingFormat::Float,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(usage_count(&db), 0, "a no_log key wrote a usage row");
        assert_eq!(call_log_count(&db), 0, "a no_log key wrote a call log");
    }

    #[tokio::test]
    async fn a_malformed_request_is_rejected_before_any_upstream_call() {
        let server = MockServer::start().await;
        let (state, db) = open();

        let (status, body) = handle(
            &state,
            open_auth(),
            json!({"model": "heterion-local/e5", "input": "hi", "encoding_format": "utf8"}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("encoding_format"))
        );

        let (status, body) = handle(
            &state,
            open_auth(),
            json!({"model": "heterion-local/e5", "input": []}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        let (status, body) = handle(
            &state,
            open_auth(),
            json!({"model": "heterion-local/e5", "input": "hi", "dimensions": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty()
        );
        assert_eq!(usage_count(&db), 0);
    }

    #[tokio::test]
    async fn a_resolved_target_names_the_provider_url_and_a_keyless_call() {
        let (state, _db) = open();

        let target = resolve_target(&state, "heterion-local/bge-m3").expect("resolved");
        assert_eq!(target.provider, "heterion-local");
        assert_eq!(target.model, "bge-m3");
        // The alias `heterion` resolves to the same provider entry.
        assert_eq!(
            resolve_target(&state, "heterion/bge-m3")
                .expect("alias resolves")
                .url,
            target.url
        );
        assert!(target.url.ends_with("/embeddings"), "{}", target.url);
        assert!(
            target.bearer.is_none(),
            "heterion-local is keyless: it must send no Authorization"
        );
        // Nothing configured, nothing named: a keyless provider with no
        // connection row has no connection to attribute the call to.
        assert_eq!(target.connection_id, None);
    }

    #[tokio::test]
    async fn a_registry_chat_endpoint_url_becomes_an_embeddings_url() {
        // `openai` declares its base_url as the full chat endpoint.
        assert_eq!(
            embeddings_url("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            embeddings_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            embeddings_url("https://api.openai.com/v1/embeddings"),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            embeddings_url("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/embeddings"
        );
    }

    #[test]
    fn base64_round_trips_arbitrary_floats() {
        for vector in [
            vec![],
            vec![0.0f32],
            vec![1.0f32, -1.0],
            vec![0.5, -1.25, 3.75, 1e-8, -1e8, 12.0, 7.0, 8.0],
        ] {
            let encoded = base64_encode(&vector);
            assert!(!encoded.contains(' '), "{encoded}");
            let decoded = base64_decode(&encoded).expect("decodes");
            assert_eq!(floats_from_bytes(&decoded).unwrap(), vector, "{encoded}");
        }
    }
}
