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
//! module does, and it writes to its own file: another program's credentials are
//! read as a seed and never overwritten, because that file is not ours and its
//! owner may be writing it at the same moment.
//!
//! **A refresh token can only have one owner, and that is not a rule we chose.**
//! The issuer rotates it: the moment we exchange one, the copy in the seed file
//! is dead. So refreshing somebody else's credential *always* costs them their
//! login eventually, whether or not we write anything back. Two things make that
//! trade an honest one:
//!
//! - We only refresh when the token is nearly expired, which is exactly when the
//!   seed's copy was about to stop working anyway. A fresh seed is used as-is and
//!   nothing rotates -- the common case leaves the other program alone.
//! - Whichever side refreshed *last* wins. `fresh` reads both the seed and our
//!   own copy and uses whichever was issued more recently, so if opencode
//!   renews its own login we pick that up, and if we renew ours it keeps
//!   working. It heals in both directions instead of one side going quietly
//!   stale.
//!
//! What it cannot do is make one refresh token serve two programs indefinitely.
//! If both this and opencode are expected to reach the same provider, both need
//! their own login.

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
    /// The same token pair as opencode's `auth.json` keeps it.
    ///
    /// A second shape rather than a second flow: the two files carry the same
    /// facts under different key names, and the `client_id` inside both access
    /// tokens is the same application, so a refresh works for either. This is
    /// worth having because it is usually the login that exists -- `opencode
    /// auth login` is the one an operator has already run, and the Codex CLI's
    /// file is the one this module happened to be written against first.
    Opencode(PathBuf),
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

/// opencode's `auth.json`: one entry per provider, keyed by provider id.
///
/// Read as a map rather than as a struct because the file holds every provider
/// the operator has ever logged into, and a new one appearing should not stop
/// this reading the one it came for.
#[derive(Clone, Debug, Deserialize)]
pub struct Opencode {
    #[serde(flatten)]
    providers: std::collections::BTreeMap<String, OpencodeEntry>,
}

/// `accountId` is camel case in the file and the header it becomes is
/// `chatgpt-account-id`; getting that rename wrong costs a 401 that mentions
/// neither. The file's own `expires` is deliberately not read -- expiry comes
/// from the access token, which is what the server checks.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpencodeEntry {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    access: Option<String>,
    #[serde(default)]
    refresh: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

impl Opencode {
    pub fn read(path: impl AsRef<Path>) -> Result<Opencode> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::NoCredential(format!("{}: {e}", path.display())))?;
        Self::parse(&text)
            .map_err(|e| Error::NoCredential(format!("{}: {}", path.display(), e)))
    }

    pub fn parse(text: &str) -> std::result::Result<Opencode, String> {
        serde_json::from_str(text).map_err(|e| format!("not an opencode auth file: {e}"))
    }

    /// One provider's OAuth pair, as the Codex shape the rest of this module
    /// already knows how to refresh and resolve.
    ///
    /// Only an `oauth` entry converts. An `api` entry in the same file is a key
    /// rather than a token pair: it belongs to `Auth::Env` or `Auth::Key`, and
    /// silently treating one as the other would send a key where a bearer token
    /// is expected and report the failure as an expiry.
    pub fn oauth(&self, provider: &str) -> std::result::Result<Codex, String> {
        let entry = self
            .providers
            .get(provider)
            .ok_or_else(|| format!("no `{provider}` entry -- run `opencode auth login`"))?;
        if entry.r#type.as_deref() != Some("oauth") {
            return Err(format!(
                "the `{provider}` entry is {}, not an oauth login",
                entry.r#type.as_deref().unwrap_or("untyped")
            ));
        }
        let access = entry
            .access
            .clone()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| format!("the `{provider}` entry has no access token"))?;
        Ok(Codex {
            auth_mode: Some("chatgpt".into()),
            tokens: Tokens {
                access_token: access,
                refresh_token: entry.refresh.clone().unwrap_or_default(),
                id_token: None,
                account_id: entry.account_id.clone(),
            },
            last_refresh: None,
        })
    }
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

    /// Write our own copy, atomically and privately.
    ///
    /// Temp file and a rename, because several of these processes can run at
    /// once -- one per visitor, in the deployment this is for -- and a reader
    /// must never see half a credential. Mode 0600 for the obvious reason: this
    /// is a bearer token for somebody's account.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| Error::NoCredential(format!("cannot encode credentials: {e}")))?;
        let fail = |e: std::io::Error| Error::NoCredential(format!("{}: {e}", path.display()));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(fail)?;
        }
        // Named after this process, so two of them cannot collide on the temp
        // file itself.
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        {
            use std::io::Write;
            let mut f = open_private(&temp).map_err(fail)?;
            f.write_all(text.as_bytes()).map_err(fail)?;
            f.sync_all().map_err(fail)?;
        }
        std::fs::rename(&temp, path).map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            fail(e)
        })
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

    /// When this pair was issued, from the access token's own `iat`.
    ///
    /// Used to tell our copy from the seed's when both exist: the one issued
    /// more recently is the live one, and the other's refresh token is either
    /// already dead or about to be. Without this the two would fight, and the
    /// loser would be whichever program happened to run second.
    pub fn issued_at(&self) -> Option<u64> {
        claims(&self.tokens.access_token)?
            .get("iat")
            .and_then(Value::as_u64)
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

/// A file only this user can read. The token in it is a bearer credential.
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// Where our own copy of a renewed pair lives, given the seed it came from.
///
/// Named after the seed rather than fixed, so two tiers seeded from two
/// different logins do not overwrite each other's tokens -- which would present
/// as one account's requests being signed as the other's.
///
/// `ENVOY_AUTH_STORE` overrides the directory. That exists because the
/// deployment this is for has exactly one writable path, and it is not the one
/// the specification would pick.
pub fn keep_for(seed: &Path) -> Option<PathBuf> {
    let dir = match std::env::var_os("ENVOY_AUTH_STORE") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => match std::env::var_os("XDG_DATA_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("envoy"),
            _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/share/envoy"),
        },
    };
    // `<parent>-<stem>.json`: `/app/agent/opencode/auth.json` becomes
    // `opencode-auth.json`, `~/.codex/auth.json` becomes `codex-auth.json`.
    let stem = seed.file_stem()?.to_string_lossy().to_string();
    let parent = seed
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().trim_start_matches('.').to_string())
        .unwrap_or_default();
    let name = match parent.is_empty() {
        true => format!("{stem}.json"),
        false => format!("{parent}-{stem}.json"),
    };
    Some(dir.join(name))
}

/// The credential to use right now, renewing it first if it is about to expire.
///
/// The async half of `resolve`. Everything that cannot expire goes straight
/// through; an OAuth pair is checked, and renewed at the last moment before a
/// request rather than at startup -- a process that has been up for an hour
/// holding a token issued before that would otherwise send a dead one.
pub async fn fresh(auth: &Auth) -> Result<Resolved> {
    let seed_path = match auth {
        Auth::Codex(path) | Auth::Opencode(path) => path.clone(),
        // Nothing else here has an expiry.
        other => return resolve(other),
    };

    let seed = read_oauth(auth).ok();
    let keep_path = keep_for(&seed_path);
    let kept = keep_path.as_ref().and_then(|p| Codex::read(p).ok());

    let now = now();
    let (live, spare) = match pick(seed, kept, now) {
        // Good for a while yet. Nothing is written and nothing rotates, which
        // is what leaves the other program's copy working.
        Pick::Use(live) => return Ok(live.resolved()),
        Pick::Renew { live, spare } => (live, spare),
        Pick::Nothing => {
            // Say which file, and what to run. This is the whole diagnosis.
            return Err(Error::NoCredential(format!(
                "no usable login in {} -- run `opencode auth login` and pick the provider",
                seed_path.display()
            )));
        }
    };

    match live.refresh().await {
        Ok(renewed) => {
            if let Some(path) = &keep_path {
                // A write that fails is worth saying and not worth failing on:
                // the token in hand is good for an hour either way, and the next
                // process will renew again.
                if let Err(e) = renewed.write(path) {
                    eprintln!("parley: could not keep the renewed credential: {e}");
                }
            }
            Ok(renewed.resolved())
        }
        Err(e) => {
            // Two ways this is somebody else's success rather than our failure:
            // another process of ours renewed a moment ago and holds the pair we
            // just spent, or the other side re-logged in and the file we called
            // the spare is now the live one. Both look like `invalid_grant`.
            if let Some(path) = &keep_path {
                if let Ok(again) = Codex::read(path) {
                    if !again.stale(now) {
                        return Ok(again.resolved());
                    }
                }
            }
            if let Some(spare) = spare {
                if !spare.stale(now) {
                    return Ok(spare.resolved());
                }
            }
            Err(e)
        }
    }
}

/// What to do with what is on disk.
///
/// Split out and pure so the rules can be checked without a network, a clock or
/// a real token: which of two pairs is live, when to renew, and what to keep in
/// reserve if renewing fails.
#[derive(Debug)]
pub(crate) enum Pick {
    /// Good for now. Send it and touch nothing.
    Use(Codex),
    /// About to expire. Renew this one; fall back to the other if that fails.
    Renew { live: Codex, spare: Option<Codex> },
    /// Nothing usable on disk at all.
    Nothing,
}

pub(crate) fn pick(seed: Option<Codex>, kept: Option<Codex>, now: u64) -> Pick {
    // Whichever was issued last is the live one. See the module note: the
    // issuer rotates refresh tokens, so the older of the two holds one that is
    // already spent or about to be. A pair whose issue time cannot be read
    // loses to one whose can -- an unreadable token is not evidence of
    // freshness.
    let (live, spare) = match (seed, kept) {
        (Some(a), Some(b)) => match (a.issued_at(), b.issued_at()) {
            (Some(x), Some(y)) if y > x => (b, Some(a)),
            (None, Some(_)) => (b, Some(a)),
            _ => (a, Some(b)),
        },
        (Some(only), None) | (None, Some(only)) => (only, None),
        (None, None) => return Pick::Nothing,
    };
    // A spare that has also expired is not a spare.
    let spare = spare.filter(|s| !s.stale(now));
    match live.stale(now) {
        false => Pick::Use(live),
        // The live one is spent, but a fresh spare means somebody else renewed
        // while we were not looking -- use theirs rather than spending ours.
        true => match spare {
            Some(spare) if !spare.stale(now) && spare.issued_at() >= live.issued_at() => {
                Pick::Use(spare)
            }
            spare => Pick::Renew { live, spare },
        },
    }
}

/// The seed, whichever shape it is in.
fn read_oauth(auth: &Auth) -> Result<Codex> {
    match auth {
        Auth::Codex(path) => Codex::read(path),
        Auth::Opencode(path) => Opencode::read(path)?
            .oauth("openai")
            .map_err(|e| Error::NoCredential(format!("{}: {e}", path.display()))),
        _ => Err(Error::NoCredential("not an oauth credential".into())),
    }
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
        // The provider is `openai` because that is what opencode calls the
        // ChatGPT login in that file. Its other entries are other providers'
        // and none of them is this one.
        //
        // Note what this does *not* do: renew. `resolve` is the synchronous
        // answer to "what is on disk", used at startup to decide which tiers
        // have a credential at all. `fresh` is the one a request goes through.
        Auth::Codex(_) | Auth::Opencode(_) => Ok(read_oauth(auth)?.resolved()),
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

    /// A pair with a known issue time, an expiry and a refresh token, for the
    /// rules below. `iat` is what decides which of two files is live.
    fn pair(iat: u64, exp: u64, refresh: &str) -> Codex {
        Codex {
            auth_mode: Some("chatgpt".into()),
            tokens: Tokens {
                access_token: jwt(json!({ "iat": iat, "exp": exp })),
                refresh_token: refresh.into(),
                id_token: None,
                account_id: Some("acct".into()),
            },
            last_refresh: None,
        }
    }

    /// The common case, and the one that matters most: a token with time left is
    /// used as it is. Nothing is renewed, so nothing rotates, so the other
    /// program reading the same file keeps working.
    #[test]
    fn a_credential_with_time_left_is_used_untouched() {
        let now = 1_000_000;
        match pick(Some(pair(now - 60, now + 3600, "r")), None, now) {
            Pick::Use(live) => assert_eq!(live.tokens.refresh_token, "r"),
            other => panic!("renewed a perfectly good token: {other:?}"),
        }
    }

    /// Two files, and the one issued later wins. This is the rule that makes it
    /// heal in both directions: if the other program re-logged in, its file is
    /// newer and we use that; if we renewed, ours is.
    #[test]
    fn the_more_recently_issued_of_two_files_is_the_live_one() {
        let now = 2_000_000;
        let older = pair(now - 7200, now + 600, "old");
        let newer = pair(now - 60, now + 3600, "new");

        // Ours is newer.
        match pick(Some(older.clone()), Some(newer.clone()), now) {
            Pick::Use(live) => assert_eq!(live.tokens.refresh_token, "new"),
            other => panic!("{other:?}"),
        }
        // Theirs is newer -- they logged in again.
        match pick(Some(newer.clone()), Some(older.clone()), now) {
            Pick::Use(live) => assert_eq!(live.tokens.refresh_token, "new"),
            other => panic!("{other:?}"),
        }
    }

    /// An expired token is renewed, and the other file is kept as the fallback
    /// for a renewal that fails.
    #[test]
    fn an_expired_credential_is_renewed_with_the_other_held_in_reserve() {
        let now = 3_000_000;
        // Both expired, ours issued later.
        let theirs = pair(now - 7200, now - 3600, "theirs");
        let ours = pair(now - 3600, now - 10, "ours");
        match pick(Some(theirs), Some(ours), now) {
            Pick::Renew { live, spare } => {
                assert_eq!(live.tokens.refresh_token, "ours");
                // An expired spare is not a spare.
                assert!(spare.is_none(), "held an expired pair in reserve");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The race this is really guarding: another process of ours renewed a
    /// moment ago and wrote a good pair. Spending our dead refresh token would
    /// invalidate theirs; using it costs nothing.
    #[test]
    fn a_fresh_pair_from_somebody_else_is_used_rather_than_spending_ours() {
        let now = 4_000_000;
        let ours_dead = pair(now - 3600, now - 5, "spent");
        let theirs_fresh = pair(now - 30, now + 3600, "fresh");
        match pick(Some(ours_dead), Some(theirs_fresh), now) {
            Pick::Use(live) => assert_eq!(live.tokens.refresh_token, "fresh"),
            other => panic!("spent a dead token with a good one on disk: {other:?}"),
        }
    }

    /// A token whose issue time cannot be read loses to one that can. An
    /// unreadable token is not evidence of freshness.
    #[test]
    fn an_unreadable_pair_does_not_win_on_a_tie() {
        let now = 5_000_000;
        let unreadable = Codex {
            tokens: Tokens { access_token: "not-a-jwt".into(), ..pair(0, 0, "opaque").tokens },
            ..pair(0, 0, "opaque")
        };
        let good = pair(now - 60, now + 3600, "good");
        match pick(Some(unreadable), Some(good), now) {
            Pick::Use(live) => assert_eq!(live.tokens.refresh_token, "good"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nothing_on_disk_is_nothing_to_use() {
        assert!(matches!(pick(None, None, 1), Pick::Nothing));
    }

    /// Where our own copy goes: named after the seed, so two logins cannot
    /// overwrite each other and sign one account's requests as the other's.
    #[test]
    fn our_own_copy_is_named_after_the_login_it_came_from() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ENVOY_AUTH_STORE");
        std::env::set_var("XDG_DATA_HOME", "/app/agent");
        assert_eq!(
            keep_for(Path::new("/app/agent/opencode/auth.json")).unwrap(),
            PathBuf::from("/app/agent/envoy/opencode-auth.json")
        );
        // The leading dot of `~/.codex` is not part of a filename we want.
        assert_eq!(
            keep_for(Path::new("/home/p/.codex/auth.json")).unwrap(),
            PathBuf::from("/app/agent/envoy/codex-auth.json")
        );
        // Two logins, two files. The failure this prevents is one account's
        // token being sent for the other's requests.
        assert_ne!(
            keep_for(Path::new("/app/agent/opencode/auth.json")),
            keep_for(Path::new("/home/p/.codex/auth.json"))
        );

        std::env::set_var("ENVOY_AUTH_STORE", "/tmp/elsewhere");
        assert_eq!(
            keep_for(Path::new("/app/agent/opencode/auth.json")).unwrap(),
            PathBuf::from("/tmp/elsewhere/opencode-auth.json")
        );
        std::env::remove_var("ENVOY_AUTH_STORE");
        std::env::remove_var("XDG_DATA_HOME");
    }

    /// Our copy is written atomically, privately, and to a directory that may
    /// not exist yet. Several of these processes run at once in the deployment
    /// this is for, so a reader must never see half a credential.
    #[test]
    fn our_own_copy_is_written_whole_and_private() {
        let dir = std::env::temp_dir().join(format!("parley-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/opencode-auth.json");
        let want = pair(10, 20, "kept");
        want.write(&path).expect("did not write");

        let back = Codex::read(&path).expect("did not read back");
        assert_eq!(back.tokens.refresh_token, "kept");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "a bearer token was left readable");
        }
        // No temp file left behind.
        let strays: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(strays.is_empty(), "left a temp file: {strays:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A live one, end to end, without a network: a seed with time left resolves
    /// through `fresh` and writes nothing at all.
    #[tokio::test]
    async fn a_fresh_seed_resolves_without_renewing_or_writing() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("parley-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("ENVOY_AUTH_STORE", dir.join("keep"));

        let now = now();
        let seed = dir.join("auth.json");
        std::fs::write(
            &seed,
            json!({
                "openai": {
                    "type": "oauth",
                    "access": jwt(json!({ "iat": now - 60, "exp": now + 3600 })),
                    "refresh": "rt",
                    "accountId": "acct-9"
                }
            })
            .to_string(),
        )
        .unwrap();
        let before = std::fs::read(&seed).unwrap();

        let resolved = fresh(&Auth::Opencode(seed.clone())).await.expect("no credential");
        assert!(resolved.api_key.is_some());
        assert!(resolved
            .headers
            .iter()
            .any(|(k, v)| k == "chatgpt-account-id" && v == "acct-9"));
        assert_eq!(std::fs::read(&seed).unwrap(), before, "the seed was written to");
        assert!(!dir.join("keep").exists(), "kept a copy it did not need to");

        std::env::remove_var("ENVOY_AUTH_STORE");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// And the diagnosis when there is nothing: the file, and the command.
    #[tokio::test]
    async fn no_login_at_all_names_the_file_and_the_fix() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ENVOY_AUTH_STORE", std::env::temp_dir().join("parley-none"));
        let why = fresh(&Auth::Opencode(PathBuf::from("/nowhere/auth.json")))
            .await
            .unwrap_err()
            .to_string();
        assert!(why.contains("/nowhere/auth.json"), "{why}");
        assert!(why.contains("opencode auth login"), "{why}");
        std::env::remove_var("ENVOY_AUTH_STORE");
    }

    /// One lock, because these set environment variables and cargo runs tests
    /// on threads that share them.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The shape of opencode's file, as it is actually on disk. The tokens are
    /// stand-ins; every key name is real, including the camel case one that a
    /// wrong rename would have dropped in silence.
    const OPENCODE: &str = r#"{
      "opencode": { "type": "api", "key": "sk-not-a-token" },
      "openai": {
        "type": "oauth",
        "access": "header.payload.sig",
        "refresh": "rt-abc",
        "expires": 1787553669231,
        "accountId": "4507603a-1897-4a0b-b213-1669ff48f980"
      },
      "github-copilot": { "type": "oauth", "access": "gho_x", "refresh": "gho_y", "expires": 0 }
    }"#;

    #[test]
    fn an_opencode_login_becomes_the_same_credential_as_a_codex_one() {
        let file = Opencode::parse(OPENCODE).expect("did not parse");
        let codex = file.oauth("openai").expect("no openai entry");
        assert_eq!(codex.tokens.access_token, "header.payload.sig");
        assert_eq!(codex.tokens.refresh_token, "rt-abc");
        // The rename that would fail quietly: without it the account header is
        // absent and the backend answers 401 without saying why.
        assert_eq!(
            codex.account().as_deref(),
            Some("4507603a-1897-4a0b-b213-1669ff48f980")
        );
        let resolved = codex.resolved();
        assert!(resolved
            .headers
            .iter()
            .any(|(k, v)| k == "chatgpt-account-id" && v.starts_with("4507603a")));
        assert_eq!(resolved.api_key.as_deref(), Some("header.payload.sig"));
    }

    /// A key is not a token pair. Reading one as the other would send an API key
    /// as a bearer token and report the 401 as an expiry.
    #[test]
    fn a_key_entry_is_refused_rather_than_read_as_a_login() {
        let file = Opencode::parse(OPENCODE).unwrap();
        let why = file.oauth("opencode").unwrap_err();
        assert!(why.contains("not an oauth login"), "{why}");
    }

    /// A provider nobody has logged into names itself and says what to do,
    /// because that is the whole diagnosis.
    #[test]
    fn a_provider_with_no_entry_says_which_and_what_to_run() {
        let file = Opencode::parse(OPENCODE).unwrap();
        let why = file.oauth("anthropic").unwrap_err();
        assert!(why.contains("anthropic"), "{why}");
        assert!(why.contains("opencode auth login"), "{why}");
    }

    /// Other providers in the same file are not this one's business, and one
    /// that has never been seen before does not stop the file being read.
    #[test]
    fn an_unknown_provider_in_the_file_is_ignored() {
        let odd = r#"{"openai":{"type":"oauth","access":"a.b.c"},"brand-new":{"whatever":true}}"#;
        let file = Opencode::parse(odd).expect("a strange entry stopped the file parsing");
        assert!(file.oauth("openai").is_ok());
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
