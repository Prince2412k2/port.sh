//! What went wrong, and whether trying again could help.
//!
//! The distinction is the point. `health.rs` walks a list of models and falls
//! through to the next one when a tier stops answering; falling through on a
//! *fatal* error means every bug in our own request builder is reported as
//! three models refusing in a row, and the real message -- the 400 that said
//! which field was wrong -- is the one we threw away.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// The socket, DNS, TLS, a timeout. Nobody's fault and worth retrying.
    #[error("transport: {0}")]
    Transport(String),

    /// 429, or a provider saying it is overloaded.
    #[error("rate limited{}: {message}", match retry_after { Some(s) => format!(" (retry after {s}s)"), None => String::new() })]
    RateLimited {
        message: String,
        retry_after: Option<u64>,
    },

    /// 500-class. The provider broke, not us.
    #[error("provider error {status}: {message}")]
    Upstream { status: u16, message: String },

    /// 401/403. Retrying with the same credential cannot work.
    #[error("auth: {0}")]
    Auth(String),

    /// 400-class other than auth. Our request was wrong.
    #[error("rejected {status}: {message}")]
    Rejected { status: u16, message: String },

    /// The conversation no longer fits. Distinct from `Rejected` because the
    /// answer is to compact rather than to give up or switch models.
    #[error("context window exceeded: {0}")]
    TooLong(String),

    /// Bytes arrived that were not what the protocol promised.
    #[error("malformed response: {0}")]
    Malformed(String),

    /// No usable credential for this provider.
    #[error("no credential for {0}")]
    NoCredential(String),

    /// We stopped it. Not a failure.
    #[error("aborted")]
    Aborted,
}

impl Error {
    /// Whether a different attempt -- a retry, or the next model in the tier --
    /// could plausibly succeed.
    pub fn retryable(&self) -> bool {
        match self {
            Error::Transport(_) | Error::RateLimited { .. } | Error::Upstream { .. } => true,
            // Malformed bytes usually mean a truncated stream, which a retry
            // fixes. A genuinely wrong parser will fail again and get reported
            // properly on the last tier.
            Error::Malformed(_) => true,
            Error::Auth(_)
            | Error::Rejected { .. }
            | Error::TooLong(_)
            | Error::NoCredential(_)
            | Error::Aborted => false,
        }
    }

    /// Build from an HTTP status and whatever body came with it.
    ///
    /// The body is searched rather than parsed: every provider nests its error
    /// message somewhere different, and a status with no message at all is the
    /// common case when a gateway rather than the provider answered.
    pub fn from_status(status: u16, body: &str, retry_after: Option<u64>) -> Error {
        let message = extract_message(body).unwrap_or_else(|| {
            if body.trim().is_empty() {
                format!("HTTP {status}, no body")
            } else {
                body.chars().take(400).collect()
            }
        });
        match status {
            429 => Error::RateLimited {
                message,
                retry_after,
            },
            401 | 403 => Error::Auth(message),
            500..=599 => Error::Upstream { status, message },
            400 | 413 if looks_like_overflow(&message) => Error::TooLong(message),
            _ => Error::Rejected { status, message },
        }
    }
}

/// Pull a human-readable message out of a provider's error envelope.
fn extract_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    // Each path is tried independently: a missing key means "not this shape",
    // not "give up". Walking with `?` directly here returned from the whole
    // function on the first miss, so only `error.message` was ever consulted.
    let paths: [&[&str]; 4] = [&["error", "message"], &["error"], &["message"], &["detail"]];
    paths.iter().find_map(|path| {
        let mut cur = &v;
        for key in *path {
            cur = cur.get(*key)?;
        }
        cur.as_str().map(str::to_string)
    })
}

fn looks_like_overflow(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("context length")
        || m.contains("context window")
        || m.contains("too many tokens")
        || m.contains("maximum context")
        || m.contains("reduce the length")
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Error {
        if e.is_timeout() {
            Error::Transport(format!("timeout: {e}"))
        } else {
            Error::Transport(e.to_string())
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_limit_is_retryable_and_a_rejection_is_not() {
        assert!(Error::from_status(429, "{}", Some(3)).retryable());
        assert!(Error::from_status(503, "", None).retryable());
        assert!(!Error::from_status(400, "{}", None).retryable());
        assert!(!Error::from_status(401, "{}", None).retryable());
    }

    #[test]
    fn an_error_message_is_found_wherever_the_provider_put_it() {
        let openai = r#"{"error":{"message":"bad tool schema","type":"invalid_request_error"}}"#;
        assert!(matches!(
            Error::from_status(400, openai, None),
            Error::Rejected { message, .. } if message == "bad tool schema"
        ));

        let flat = r#"{"message":"nope"}"#;
        assert!(matches!(
            Error::from_status(400, flat, None),
            Error::Rejected { message, .. } if message == "nope"
        ));

        // Ollama answers some failures with a bare string field.
        let ollama = r#"{"error":"model not found"}"#;
        assert!(matches!(
            Error::from_status(404, ollama, None),
            Error::Rejected { message, .. } if message == "model not found"
        ));
    }

    #[test]
    fn an_overflow_is_told_apart_from_an_ordinary_rejection() {
        let body = r#"{"error":{"message":"This model's maximum context length is 8192 tokens"}}"#;
        assert!(matches!(
            Error::from_status(400, body, None),
            Error::TooLong(_)
        ));
        // ...and stays fatal, because the fix is to compact rather than retry.
        assert!(!Error::from_status(400, body, None).retryable());
    }

    #[test]
    fn a_status_with_no_body_still_says_something() {
        let e = Error::from_status(502, "", None);
        assert!(e.to_string().contains("502"), "{e}");
    }
}
