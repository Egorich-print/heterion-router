//! The platform's own model catalogue, fetched live.
//!
//! The registry says what a platform looked like on the day the catalogue was
//! written. The platform says what it serves *today* — and only its own answer
//! counts (AGENTS.md: «перечень моделей в реестре берётся из площадки, а не из
//! памяти»). A stale registry promises clients models the area no longer has,
//! and hides models it just added.
//!
//! The fetch is therefore a measurement, not a claim: it goes out with the
//! connection's own credential, and what comes back is stored as it arrived.

use serde_json::Value;

/// One model a platform lists.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogModel {
    /// The model id, exactly as the platform writes it.
    pub id: String,
    /// The context window the platform publishes, when it publishes one.
    pub context_length: Option<i64>,
    /// The price of one input token, as the platform publishes it.
    ///
    /// `None` means the platform said nothing about the price — not that the
    /// model is free.
    pub cost_input: Option<f64>,
}

impl CatalogModel {
    /// Whether the platform prices this model at zero.
    ///
    /// A model whose price the platform did not publish is *not* free: the
    /// anonymous access of OpenCode Zen serves the models it prices at zero
    /// and refuses the rest with `401 Missing API key` (probed live
    /// 2026-11-20), so an unknown price must not be treated as a free one.
    pub fn is_free(&self) -> bool {
        self.cost_input == Some(0.0)
    }
}

/// The price of one input token in a price container.
///
/// Platforms write it either as a number (`"input": 0.25`) or as an object
/// with a `price` field (`"input": {"price": 0.25}`).
fn price_from(container: &serde_json::Map<String, Value>) -> Option<f64> {
    match container.get("input")? {
        Value::Number(number) => number.as_f64(),
        Value::Object(object) => object.get("price").and_then(Value::as_f64),
        _ => None,
    }
}

/// The price of one input token, wherever the platform keeps it.
fn input_price(object: &serde_json::Map<String, Value>) -> Option<f64> {
    for key in ["cost", "pricing"] {
        let Some(Value::Object(container)) = object.get(key) else {
            continue;
        };
        if let Some(price) = price_from(container) {
            return Some(price);
        }
        if let Some(Value::Object(text)) = container.get("text")
            && let Some(price) = price_from(text)
        {
            return Some(price);
        }
    }
    None
}

/// The URL a platform lists its models at.
///
/// The registry declares either an API root or the endpoint itself, so the
/// path is resolved the way the backends resolve it: the chat path is
/// swapped for the models path, and a root gets `/models` appended.
pub fn models_url(format: Option<&str>, base_url: &str) -> Option<String> {
    let base = base_url.trim();
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return None;
    }
    let trimmed = base.trim_end_matches('/');
    let root = ["/chat/completions", "/responses", "/messages"]
        .iter()
        .find_map(|suffix| trimmed.strip_suffix(suffix))
        .unwrap_or(trimmed);
    match format.unwrap_or("openai") {
        "openai" | "openai-responses" | "claude" => Some(format!("{root}/models")),
        // Gemini embeds the model in the path, and lists them under /models.
        "gemini" => Some(format!("{root}/models")),
        _ => None,
    }
}

/// The models a platform lists, as its answer says.
///
/// Platforms write the catalogue differently: OpenAI-shaped areas answer
/// `{"data": [{"id": …}]}`, Gemini answers `{"models": [{"name": "models/…"}]}`,
/// and some answer with plain strings. All three are read here — a catalogue
/// that is parsed strictly would report "no models" for a platform that
/// listed a hundred.
pub fn parse_catalog(body: &str) -> Vec<CatalogModel> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let entries = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut models: Vec<CatalogModel> = entries
        .iter()
        .filter_map(|entry| match entry {
            Value::String(id) => Some(CatalogModel {
                id: id.clone(),
                context_length: None,
                cost_input: None,
            }),
            Value::Object(object) => {
                let raw = object
                    .get("id")
                    .or_else(|| object.get("name"))
                    .and_then(Value::as_str)?;
                Some(CatalogModel {
                    id: raw.strip_prefix("models/").unwrap_or(raw).to_string(),
                    context_length: object
                        .get("context_length")
                        .or_else(|| object.get("context_window"))
                        .or_else(|| object.get("max_context_length"))
                        .and_then(Value::as_i64),
                    cost_input: input_price(object),
                })
            }
            _ => None,
        })
        .filter(|model| !model.id.is_empty())
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    models
}

/// The credential and headers a catalogue fetch needs.
///
/// The session header matters: OpenCode's Console refuses a request without
/// `x-opencode-session` outright, so a fetch that omits it would report "no
/// models" for a platform that has hundreds.
pub fn catalog_headers(provider: &str, format: Option<&str>, token: &str) -> Vec<(String, String)> {
    // Gemini takes the key in its own header, not as a bearer token.
    if format == Some("gemini") {
        return vec![("x-goog-api-key".to_string(), token.to_string())];
    }
    let mut headers = vec![("authorization".to_string(), format!("Bearer {token}"))];
    if provider.starts_with("opencode") {
        // The catalogue request is not part of a conversation, so a stable
        // per-provider id is enough — it only has to be present.
        headers.push((
            "x-opencode-session".to_string(),
            format!("ses_catalog_{}", short_hash(provider)),
        ));
    }
    headers
}

/// A short, stable hash of a provider id.
fn short_hash(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(&hasher.finalize()[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_models_url_swaps_the_chat_path_for_the_models_path() {
        assert_eq!(
            models_url(
                Some("openai"),
                "https://api.sambanova.ai/v1/chat/completions"
            )
            .as_deref(),
            Some("https://api.sambanova.ai/v1/models")
        );
        assert_eq!(
            models_url(Some("openai"), "https://api.openrouter.ai/api/v1").as_deref(),
            Some("https://api.openrouter.ai/api/v1/models")
        );
        assert_eq!(
            models_url(Some("claude"), "https://api.kimi.com/coding/v1/messages").as_deref(),
            Some("https://api.kimi.com/coding/v1/models")
        );
        assert_eq!(
            models_url(
                Some("gemini"),
                "https://generativelanguage.googleapis.com/v1beta"
            )
            .as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta/models")
        );
    }

    #[test]
    fn a_non_http_base_url_has_no_models_url() {
        assert!(models_url(Some("openai"), "not-a-url").is_none());
    }

    /// The price the platform publishes decides whether a model is reachable
    /// for an anonymous client: OpenCode Zen serves the models it prices at
    /// zero and refuses the rest (probed live 2026-11-20).
    #[test]
    fn the_published_price_decides_whether_a_model_is_free() {
        let body = r#"{"data":[
            {"id":"big-pickle","cost":{"text":{"input":{"price":0},"output":{"price":0}}}},
            {"id":"gpt-5.4","cost":{"text":{"input":{"price":2.5},"output":{"price":15}}}},
            {"id":"flat","cost":{"input":0.5}},
            {"id":"mystery"}
        ]}"#;
        let models = parse_catalog(body);
        let by_id = |id: &str| {
            models
                .iter()
                .find(|model| model.id == id)
                .unwrap_or_else(|| panic!("{id} parsed"))
        };
        assert_eq!(by_id("big-pickle").cost_input, Some(0.0));
        assert!(by_id("big-pickle").is_free());
        assert_eq!(by_id("gpt-5.4").cost_input, Some(2.5));
        assert!(!by_id("gpt-5.4").is_free());
        assert_eq!(by_id("flat").cost_input, Some(0.5));
        assert_eq!(
            by_id("mystery").cost_input,
            None,
            "an unpublished price is not a free one"
        );
        assert!(
            !by_id("mystery").is_free(),
            "a model of unknown price must not be offered as free"
        );
    }

    #[test]
    fn an_openai_catalogue_is_read() {
        let body = r#"{"data":[{"id":"glm-5.3"},{"id":"gpt-oss-120b","context_length":131072}]}"#;
        let models = parse_catalog(body);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "glm-5.3");
        assert_eq!(models[1].id, "gpt-oss-120b");
        assert_eq!(models[1].context_length, Some(131_072));
    }

    #[test]
    fn a_gemini_catalogue_is_read() {
        let body = r#"{"models":[{"name":"models/gemini-3.1-flash-preview"}]}"#;
        let models = parse_catalog(body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gemini-3.1-flash-preview");
    }

    #[test]
    fn a_plain_string_catalogue_is_read() {
        let body = r#"{"data":["model-a","model-b"]}"#;
        let models = parse_catalog(body);
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["model-a", "model-b"]
        );
    }

    #[test]
    fn a_broken_answer_is_no_catalogue() {
        assert!(parse_catalog("not json at all").is_empty());
        assert!(parse_catalog(r#"{"error":"nope"}"#).is_empty());
    }

    #[test]
    fn the_session_header_is_only_for_opencode() {
        let opencode = catalog_headers("opencode-go", Some("openai"), "token");
        assert!(
            opencode
                .iter()
                .any(|(name, value)| name == "x-opencode-session" && !value.is_empty()),
            "OpenCode refuses a request without a session id: {opencode:?}"
        );
        let other = catalog_headers("sambanova", Some("openai"), "token");
        assert!(!other.iter().any(|(name, _)| name == "x-opencode-session"));
    }

    #[test]
    fn gemini_takes_its_key_in_its_own_header() {
        let headers = catalog_headers("gemini", Some("gemini"), "key-1");
        assert_eq!(
            headers,
            vec![("x-goog-api-key".to_string(), "key-1".to_string())],
            "Gemini does not read a bearer token"
        );
    }
}
