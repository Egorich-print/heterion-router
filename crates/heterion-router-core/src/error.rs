//! Gateway error type.

/// Errors surfaced by a backend while serving a completion.
///
/// `thiserror` rather than a hand-written `Display`: the same convention the
/// other workspace crates already follow, and it keeps the message next to the
/// variant it describes instead of in a `match` that can drift.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewayError {
    /// The requested model is not known to any configured provider.
    #[error("unknown model: {0}")]
    UnknownModel(String),
    /// The upstream provider rejected or failed the request.
    #[error("upstream error: {0}")]
    Upstream(String),
    /// The request was malformed.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}
