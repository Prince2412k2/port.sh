//! Credentials, read rather than obtained.
//!
//! Logging in is an operator's job, not a session's. A browser callback on
//! localhost means nothing to somebody reached over SSH, and a visitor to a
//! portfolio is not going to be handed an OAuth prompt -- so nothing here opens
//! a browser or waits on a device code. It reads what is already on disk and
//! keeps it fresh.
//!
//! Refreshing has to happen in process, because an access token expires in the
//! middle of a conversation rather than between them. That is the one write this
//! module does, and it writes to its own file: the Codex CLI's credentials are
//! read as a seed and never overwritten, because a refresh rotates the token and
//! clobbering somebody's working CLI login to save a copy would be a poor
//! trade.

use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

/// The client id the Codex CLI uses. Public by construction -- it identifies the
/// application, not the user.
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Who we say we are. The Codex CLI sends `codex_cli_rs`; being honest about
/// being something else is better than pretending.
pub const ORIGINATOR: &str = "envoy";

/// How a provider is authenticated.
#[derive(Clone, Debug)]
pub enum Auth {
    /// Nothing required. A local Ollama, for instance.
    None,
    /// The first of these environment variables that is set.
    Env(Vec<String>),
    /// A key from configuration.
    Key(String),
    /// A ChatGPT OAuth token pair, in the Codex CLI's file format.
    Codex(PathBuf),
}

/// What to actually put on the request.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub api_key: Option<String>,
    pub headers: Vec<(String, String)>,
    /// Where it came from, for diagnostics. Never the credential itself.
    pub source: String,
}

/// The Codex CLI's `auth.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Codex {
    #[serde(default)]
    pub auth_mode: Option<String>,
    pub tokens: Tokens,
    #[serde(default)]
    pub last_refresh: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
}

impl Codex {
    pub fn read(path: impl AsRef<Path>) -> Result<Codex> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            Error::NoCredential(format!("{}: {e}", path.display()))
        })?;
        serde_json::from_str(&text)
            .map_err(|e| Error::NoCredential(format!("{} is not a Codex credential: {e}", path.display())))
    }

    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| Error::NoCredential(format!("cannot encode credentials: {e}")))?;
        std::fs::write(path.as_ref(), text)
            .map_err(|e| Error::NoCredential(format!("{}: {e}", path.as_ref().display())))
    }

    /// The account the token belongs to.
    ///
    /// Taken from the file when it is there and decoded from the id token when
    /// it is not, because the header is required and a request without it is
    /// rejected in a way that does not mention it.
    pub fn account(&self) -> Option<String> {
        if let Some(id) = &self.tokens.account_id {
            if !id.is_empty() {
                return Some(id.clone());
            }
        }
        let claims = claims(self.tokens.id_token.as_deref()?)?;
        claims
            .get("https://api.openai.com/auth")
            .and_then(|a| a.get("chatgpt_account_id"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// Seconds until the access token expires; negative once it has.
    ///
    /// Read from the token itself rather than from `last_refresh`, because the
    /// token is the thing the server checks and the file is only a note about
    /// when we last asked.
    pub fn expires_in(&self, now: u64) -> Option<i64> {
        let exp = claims(&self.tokens.access_token)?
            .get("exp")
            .and_then(Value::as_u64)?;
        Some(exp as i64 - now as i64)
    }

    /// Whether it is worth refreshing before the next call. A minute of slack,
    /// so a token does not expire between the check and the request.
    pub fn stale(&self, now: u64) -> bool {
        match self.expires_in(now) {
            Some(left) => left < 60,
            // A token we cannot read the expiry of is refreshed rather than
            // gambled on.
            None => true,
        }
    }

    pub fn resolved(&self) -> Resolved {
        let mut headers = vec![
            ("originator".to_string(), ORIGINATOR.to_string()),
            (
                "OpenAI-Beta".to_string(),
                "responses=experimental".to_string(),
            ),
        ];
        if let Some(account) = self.account() {
            headers.push(("chatgpt-account-id".to_string(), account));
        }
        Resolved {
            api_key: Some(self.tokens.access_token.clone()),
            headers,
            source: "codex oauth".into(),
        }
    }

    /// Exchange the refresh token for a fresh pair.
    ///
    /// The refresh token rotates, so the answer must be kept or the next
    /// refresh fails. Nothing here writes it: the caller decides where its
    /// credentials live.
    pub async fn refresh(&self) -> Result<Codex> {
        let response = crate::http::client()
            .post(CODEX_TOKEN_URL)
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": self.tokens.refresh_token,
                "client_id": CODEX_CLIENT_ID,
            }))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::from_status(status.as_u16(), &body, None));
        }
        let json: Value = serde_json::from_str(&body)
            .map_err(|e| Error::Malformed(format!("token response: {e}")))?;
        let access = json
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Malformed("token response had no access_token".into()))?;
        Ok(Codex {
            auth_mode: self.auth_mode.clone(),
            tokens: Tokens {
                access_token: access.to_string(),
                // A response without a new refresh token means keep the old one.
                refresh_token: json
                    .get("refresh_token")
                    .and_then(Value::as_str)
                    .unwrap_or(&self.tokens.refresh_token)
                    .to_string(),
                id_token: json
                    .get("id_token")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| self.tokens.id_token.clone()),
                account_id: self.tokens.account_id.clone(),
            },
            last_refresh: None,
        })
    }
}

/// The payload of a JWT, without verifying it.
///
/// Not a security decision: the server verifies the token, and we only want the
/// account id and the expiry out of it. Verifying here would need the issuer's
/// keys and would still tell us nothing the server will not.
fn claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn resolve(auth: &Auth) -> Result<Resolved> {
    match auth {
        Auth::None => Ok(Resolved {
            source: "none required".into(),
            ..Resolved::default()
        }),
        Auth::Key(key) => Ok(Resolved {
            api_key: Some(key.clone()),
            headers: Vec::new(),
            source: "configuration".into(),
        }),
        Auth::Env(names) => {
            for name in names {
                if let Ok(value) = std::env::var(name) {
                    if !value.trim().is_empty() {
                        return Ok(Resolved {
                            api_key: Some(value.trim().to_string()),
                            headers: Vec::new(),
                            source: format!("${name}"),
                        });
                    }
                }
            }
            Err(Error::NoCredential(format!(
                "none of {} is set",
                names.join(", ")
            )))
        }
        Auth::Codex(path) => Ok(Codex::read(path)?.resolved()),
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jwt(claims: Value) -> String {
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.{}",
            b.encode(br#"{"alg":"none"}"#),
            b.encode(claims.to_string()),
            b.encode(b"signature")
        )
    }

    fn codex(exp: u64, account_in_file: bool) -> Codex {
        Codex {
            auth_mode: Some("chatgpt".into()),
            tokens: Tokens {
                access_token: jwt(json!({ "exp": exp })),
                refresh_token: "r".into(),
                id_token: Some(jwt(json!({
                    "https://api.openai.com/auth": { "chatgpt_account_id": "acct-from-jwt" }
                }))),
                account_id: account_in_file.then(|| "acct-from-file".to_string()),
            },
            last_refresh: None,
        }
    }

    #[test]
    fn the_account_comes_from_the_file_when_it_is_there() {
        assert_eq!(codex(0, true).account().as_deref(), Some("acct-from-file"));
    }

    #[test]
    fn the_account_is_decoded_from_the_id_token_otherwise() {
        // A request without this header is refused in a way that does not
        // mention the header, so falling back matters.
        assert_eq!(codex(0, false).account().as_deref(), Some("acct-from-jwt"));
    }

    #[test]
    fn expiry_is_read_from_the_token_not_from_the_file() {
        let c = codex(1_000_060, false);
        assert_eq!(c.expires_in(1_000_000), Some(60));
        assert!(c.stale(1_000_001), "a minute of slack means this is stale");
        assert!(!c.stale(999_000));
        assert!(c.stale(2_000_000), "long expired");
    }

    #[test]
    fn a_token_whose_expiry_cannot_be_read_is_treated_as_stale() {
        let mut c = codex(0, false);
        c.tokens.access_token = "not-a-jwt".into();
        assert_eq!(c.expires_in(0), None);
        assert!(c.stale(0));
    }

    #[test]
    fn the_codex_headers_are_the_ones_the_backend_wants() {
        let resolved = codex(0, true).resolved();
        let names: Vec<&str> = resolved.headers.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"chatgpt-account-id"));
        assert!(names.contains(&"originator"));
        assert!(names.contains(&"OpenAI-Beta"));
        assert_eq!(resolved.api_key.is_some(), true);
        // The source is for logs, and must never be the credential.
        assert!(!resolved.source.contains("eyJ"), "{}", resolved.source);
    }

    #[test]
    fn the_codex_file_format_round_trips() {
        let dir = std::env::temp_dir().join("parley-auth-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        let original = codex(123, true);
        original.write(&path).unwrap();
        let read = Codex::read(&path).unwrap();
        assert_eq!(read.tokens.refresh_token, "r");
        assert_eq!(read.account().as_deref(), Some("acct-from-file"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_missing_credential_file_names_itself() {
        let e = Codex::read("/nonexistent/auth.json").unwrap_err();
        assert!(matches!(e, Error::NoCredential(m) if m.contains("/nonexistent/auth.json")));
    }

    #[test]
    fn an_env_key_is_found_and_a_missing_one_is_named() {
        std::env::set_var("PARLEY_TEST_KEY", "sk-test");
        let resolved = resolve(&Auth::Env(vec![
            "PARLEY_TEST_ABSENT".into(),
            "PARLEY_TEST_KEY".into(),
        ]))
        .unwrap();
        assert_eq!(resolved.api_key.as_deref(), Some("sk-test"));
        assert_eq!(resolved.source, "$PARLEY_TEST_KEY");
        std::env::remove_var("PARLEY_TEST_KEY");

        let e = resolve(&Auth::Env(vec!["PARLEY_TEST_ABSENT".into()])).unwrap_err();
        assert!(matches!(e, Error::NoCredential(m) if m.contains("PARLEY_TEST_ABSENT")));
    }

    #[test]
    fn an_empty_environment_variable_is_not_a_credential() {
        // An `.env` with `OLLAMA_API_KEY=` and nothing after it is the case
        // this exists for: present, useless, and worth saying so.
        std::env::set_var("PARLEY_TEST_EMPTY", "   ");
        let e = resolve(&Auth::Env(vec!["PARLEY_TEST_EMPTY".into()])).unwrap_err();
        assert!(matches!(e, Error::NoCredential(_)));
        std::env::remove_var("PARLEY_TEST_EMPTY");
    }
}
