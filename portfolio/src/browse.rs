//! The open web, as two capabilities: search it, and read one page of it.
//!
//! **Why these are ours when the agents have their own.** Copilot's seat comes
//! with web tools, most of the free models do not, and the agent in `ai-sdk/`
//! has none at all by design -- its tools come from the client that started it.
//! So "can look something up" arrived and left with whichever server happened
//! to be answering that hour, and the section behaved differently on each tier
//! for reasons no visitor could see. These two make it the same everywhere.
//! `gates.rs` still lists the agents' own by their own names: where a server
//! brings its own search there is no reason to refuse it, and no reason to pay
//! for ours instead.
//!
//! **Two services, because each does the half the other does not.** Exa ranks
//! pages against a question and hands back a paragraph of each, which is the
//! shape a model can act on; Jina turns one page into markdown, which is the
//! shape a model can read. Neither is a browser and neither runs anything.
//!
//! **This is separate from `mcp.rs` because of what it spends.** Every map tool
//! reaches nothing but this box's own data: an index, an archive, a channel to a
//! screen. These two cross the network on somebody's account -- a search is
//! seven tenths of a cent, and the box takes any username with any key. So the
//! ceiling is in `gates.rs` with the rest of the policy, `mcp.rs` counts against
//! it per session, and this file only knows how to ask a question.
//!
//! **A missing key is not a failure, it is a tool that is not offered.** An
//! empty `EXA_API_KEY` is the same as none -- compose passes `${EXA_API_KEY:-}`,
//! so the variable exists and is empty whenever the host has not set one, which
//! is the same trap `spawn_command` strips for the agents' credentials. A tool
//! in `tools/list` that fails every call is worse than an absent one: the model
//! reaches for it, gets nothing, and often says it looked when it did not.

use std::time::Duration;

use crate::json;

/// Exa's search endpoint. POST, key in a header, JSON in and out.
const SEARCH_URL: &str = "https://api.exa.ai/search";

/// Jina's reader. The target URL is appended to this, raw.
const READER_URL: &str = "https://r.jina.ai/";

/// Results per search, and how much of each page's text comes back with it.
///
/// Five and seven hundred, so a whole search is around a thousand tokens. The
/// gist exists to answer "is this the page I want" without a second call; a
/// model handed the full text of five pages spends its window on four it did
/// not need.
const HITS: usize = 5;
const GIST: usize = 700;

/// How much of one page comes back. About fifteen hundred tokens.
///
/// Cut rather than refused, and the reply says it was cut. A truncated article
/// usually still contains the answer, and a model told plainly that there is
/// more can say so; silently handing back the first fifth of a page is how a
/// confident wrong summary gets written.
const PAGE: usize = 6000;

/// Long enough for a slow service, short enough that a visitor is not left
/// watching a spinner. The reader is the slower of the two -- it fetches the
/// page itself before it answers.
const SEARCH_SECS: u64 = 20;
const READ_SECS: u64 = 30;

/// One search result.
#[derive(Debug)]
pub struct Hit {
    pub title: String,
    pub url: String,
    /// The opening of the page's own text, as the search service extracted it.
    pub gist: String,
}

/// One page, as text.
#[derive(Debug)]
pub struct Page {
    pub title: String,
    pub url: String,
    pub text: String,
    /// Whether `text` is the whole page. Passed on to the model, which is
    /// entitled to know that it is answering from part of something.
    pub clipped: bool,
}

/// A credential, or nothing. Empty counts as nothing -- see the module note.
fn key(var: &str) -> Option<String> {
    let v = std::env::var(var).ok()?;
    (!v.trim().is_empty()).then_some(v)
}

pub fn can_search() -> bool {
    key("EXA_API_KEY").is_some()
}

pub fn can_read() -> bool {
    key("JINA_API_KEY").is_some()
}

/// Ask the web a question.
///
/// The error string goes to the model, so it says what happened in words a model
/// can act on: a refused key is not the same as a service being down, and
/// neither is the same as there being no results.
pub fn search(query: &str) -> Result<Vec<Hit>, String> {
    let query = query.trim();
    if query.len() < 2 {
        return Err("a search needs a question".into());
    }
    let key = key("EXA_API_KEY").ok_or("there is no search key on this box")?;

    // Built by hand rather than by a serialiser, for the same reason `json.rs`
    // exists at all: serde is not in the offline registry here.
    let body = format!(
        r#"{{"query":{},"numResults":{HITS},"contents":{{"text":{{"maxCharacters":{GIST}}}}}}}"#,
        json::quote(query)
    );

    let mut res = ureq::post(SEARCH_URL)
        .header("x-api-key", &key)
        .header("content-type", "application/json")
        .config()
        .timeout_global(Some(Duration::from_secs(SEARCH_SECS)))
        // The status is read rather than raised, because the body that comes
        // with a 4xx is the half that says why.
        .http_status_as_error(false)
        .build()
        .send(body.as_str())
        .map_err(|e| format!("the search service could not be reached ({e})"))?;

    let status = res.status().as_u16();
    let text = res
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("the search service answered unreadably ({e})"))?;
    if status != 200 {
        return Err(why(status, &text, "search"));
    }
    parse_hits(&text)
}

/// Read one page.
pub fn read(url: &str) -> Result<Page, String> {
    let url = url.trim();
    if !is_http(url) {
        return Err("a page needs a full http:// or https:// address".into());
    }
    let key = key("JINA_API_KEY").ok_or("there is no reader key on this box")?;

    // The reader fetches the page from *its* network, not ours, so an address
    // pointing at something private reaches nothing here -- which is why the
    // check above is about the scheme rather than about the host. The scheme is
    // still checked: `file:///etc/passwd` appended to a URL is a question worth
    // refusing on this side rather than trusting somebody else to refuse.
    let mut res = ureq::get(format!("{READER_URL}{url}"))
        .header("authorization", &format!("Bearer {key}"))
        // JSON rather than the plain markdown the reader returns by default: the
        // title, the resolved address and the failure all arrive as fields
        // instead of having to be recognised inside prose.
        .header("accept", "application/json")
        .header("x-return-format", "markdown")
        .config()
        .timeout_global(Some(Duration::from_secs(READ_SECS)))
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|e| format!("that page could not be fetched ({e})"))?;

    let status = res.status().as_u16();
    let text = res
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("that page came back unreadably ({e})"))?;
    if status != 200 {
        return Err(why(status, &text, "reader"));
    }
    parse_page(&text)
}

/// What to tell the model about a status that is not 200.
///
/// The service's own message where there is one, because "exa said 401" is a
/// thing somebody can fix and "the search failed" is not. Truncated hard: an
/// error page can be a whole HTML document.
fn why(status: u16, body: &str, who: &str) -> String {
    let said = json::parse(body)
        .and_then(|v| {
            for k in ["error", "message", "detail"] {
                if let Some(s) = v.get(k).and_then(|v| v.as_str()) {
                    return Some(s.to_string());
                }
            }
            None
        })
        .unwrap_or_else(|| body.chars().take(120).collect());
    let said = said.trim();
    match (status, said.is_empty()) {
        (401 | 403, _) => format!("the {who} refused this box's key ({status})"),
        (429, _) => format!("the {who} is rate limiting this box, try again later"),
        (_, true) => format!("the {who} answered {status}"),
        (_, false) => format!("the {who} answered {status}: {said}"),
    }
}

/// Exa's answer, as hits. Split out so it can be tested against a recorded
/// response -- the sandbox this is built in cannot reach the service.
fn parse_hits(body: &str) -> Result<Vec<Hit>, String> {
    let v = json::parse(body).ok_or("the search service sent something that is not JSON")?;
    let results = v
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or("the search service sent no results field")?;
    Ok(results
        .iter()
        .filter_map(|r| {
            let s = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").trim();
            let url = s("url");
            if url.is_empty() {
                return None;
            }
            let title = s("title");
            Some(Hit {
                // Real answers come back with an empty title -- two of the
                // three in the recorded fixture do. The address is a worse
                // label than a headline and a much better one than nothing.
                title: match title.is_empty() {
                    true => url.to_string(),
                    false => title.to_string(),
                },
                url: url.to_string(),
                gist: clip(s("text"), GIST).0,
            })
        })
        .collect())
}

/// The reader's answer, as a page.
fn parse_page(body: &str) -> Result<Page, String> {
    let v = json::parse(body).ok_or("the reader sent something that is not JSON")?;
    let s = |k: &str| {
        v.get("data")
            .and_then(|d| d.get(k))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let content = s("content");
    if content.is_empty() {
        // A 200 with nothing in it. Happens for a page that is all script, and
        // for one behind a wall that answers politely.
        let said = v.get("message").and_then(|m| m.as_str()).unwrap_or("");
        return Err(match said.is_empty() {
            true => "that page came back empty -- it may need a browser to render".into(),
            false => format!("that page came back empty: {said}"),
        });
    }
    let (text, clipped) = clip(&content, PAGE);
    Ok(Page { title: s("title"), url: s("url"), text, clipped })
}

/// The first `max` characters, and whether anything was dropped.
///
/// Cut at a line break where there is one near the end, and at a space
/// otherwise, so the last thing a model reads is not half a word it might
/// quote. Counted in characters and not bytes: slicing a `String` at an
/// arbitrary byte panics, and a page of Devanagari is nothing but multi-byte
/// characters.
fn clip(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    let head: String = s.chars().take(max).collect();
    // Only look back over the last tenth. A document with no break in it at all
    // gets cut where it was cut rather than losing most of the excerpt.
    //
    // Walked back to a character boundary rather than computed as one: a tenth
    // of a byte length lands in the middle of a character on any page that is
    // not ASCII, and slicing there panics. The test below is that page.
    let mut floor = head.len().saturating_sub(head.len() / 10);
    while floor > 0 && !head.is_char_boundary(floor) {
        floor -= 1;
    }
    let at = head[floor..]
        .rfind('\n')
        .or_else(|| head[floor..].rfind(' '))
        .map(|i| floor + i)
        .unwrap_or(head.len());
    (head[..at].trim_end().to_string(), true)
}

/// Whether this is an address the reader can be asked for.
fn is_http(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && url.len() > 10
        // A space or a newline in the middle would be a second header line by
        // the time it reached the wire.
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real answer from the search service, recorded through a shell outside
    /// the sandbox. Three results, and the interesting part is that two of them
    /// have an **empty title** -- which is why `parse_hits` falls back to the
    /// address rather than trusting the field to be there.
    const EXA: &str = r##"{"requestId":"9501e6a1a593af450eb13c7c03125b50","resolvedSearchType":"","results":[{"id":"https://agentclientprotocol.com/protocol/v2/schema","title":"","url":"https://agentclientprotocol.com/protocol/v2/schema","text":"# Schema\n\n> Schema definitions for the Agent Client Protocol\n\nThis schema file is generated in this repository"},{"id":"https://agentclientprotocol.com/protocol/v2/overview","title":"","url":"https://agentclientprotocol.com/protocol/v2/overview","text":"# Overview\n\n> How the Agent Client Protocol works"},{"id":"https://github.com/agentclientprotocol/agent-client-protocol","title":"agentclientprotocol/agent-client-protocol: A ...","url":"https://github.com/agentclientprotocol/agent-client-protocol","text":"# agentclientprotocol/agent-client-protocol\n\nA protocol for connecting any editor to any agent\n\n- Stars: 4035"}],"searchTime":1072.4,"costDollars":{"total":0.007,"search":{"neural":0.007}}}"##;

    /// A real answer from the reader, recorded the same way.
    const JINA: &str = r##"{"code":200,"status":200,"data":{"title":"Example Domain","description":"","url":"https://example.com/","content":"# Example Domain\n\nThis domain is for use in documentation examples without needing permission. Avoid use in operations.\n\n[Learn more](https://iana.org/domains/example)","publishedTime":"Tue, 18 Aug 2026 20:06:42 GMT","warning":"This is a cached snapshot of the original page, consider retry with caching opt-out.","metadata":{"lang":"en"},"httpStatus":200,"usage":{"tokens":33}},"meta":{"usage":{"tokens":33}}}"##;

    #[test]
    fn a_recorded_search_is_read_correctly() {
        let hits = parse_hits(EXA).expect("did not parse");
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].url, "https://agentclientprotocol.com/protocol/v2/schema");
        // No title in the answer, so the address stands in for one.
        assert_eq!(hits[0].title, hits[0].url);
        assert!(hits[0].gist.contains("Schema definitions"), "{}", hits[0].gist);
        assert_eq!(hits[2].title, "agentclientprotocol/agent-client-protocol: A ...");
    }

    #[test]
    fn a_recorded_page_is_read_correctly() {
        let page = parse_page(JINA).expect("did not parse");
        assert_eq!(page.title, "Example Domain");
        assert_eq!(page.url, "https://example.com/");
        assert!(page.text.starts_with("# Example Domain"), "{}", page.text);
        assert!(!page.clipped, "a short page was reported as cut");
    }

    /// A 200 carrying nothing is an error rather than an empty page, because a
    /// model handed "" writes a summary of it anyway.
    #[test]
    fn a_page_with_no_text_is_an_error_not_an_empty_page() {
        let empty = r#"{"code":200,"data":{"title":"Wall","url":"https://x.test/","content":""}}"#;
        assert!(parse_page(empty).is_err());
    }

    #[test]
    fn a_long_page_is_cut_and_says_so() {
        let long = "word ".repeat(4000);
        let (text, clipped) = clip(&long, PAGE);
        assert!(clipped);
        assert!(text.chars().count() <= PAGE);
        // Cut between words, not through one.
        assert!(text.ends_with("word"), "{:?}", &text[text.len() - 20..]);
    }

    /// Characters, not bytes. Slicing a `String` at byte `max` panics on any
    /// page that is not ASCII, and this one is a page of Devanagari.
    #[test]
    fn a_page_of_multibyte_text_is_cut_without_panicking() {
        let hindi = "अहमदाबाद ".repeat(3000);
        let (text, clipped) = clip(&hindi, PAGE);
        assert!(clipped);
        assert!(text.chars().count() <= PAGE);
        assert!(text.contains("अहमदाबाद"));
    }

    #[test]
    fn a_short_string_is_returned_whole() {
        let (text, clipped) = clip("all of it", PAGE);
        assert_eq!(text, "all of it");
        assert!(!clipped);
    }

    /// Only a real web address. The reader would refuse the rest too; this is
    /// the refusal that does not depend on somebody else's service being strict.
    #[test]
    fn only_http_addresses_are_accepted() {
        assert!(is_http("https://example.com/x"));
        assert!(is_http("HTTP://example.com"));
        assert!(!is_http("file:///etc/passwd"));
        assert!(!is_http("/etc/passwd"));
        assert!(!is_http("example.com"));
        assert!(!is_http("javascript:alert(1)"));
        assert!(!is_http("https://ex ample.com/ HTTP/1.1"));
        assert!(!is_http("https://x\r\nHost: y"));
    }

    /// The words that go to the model. A refused key and a rate limit are
    /// different problems and it can only say which if we tell it.
    #[test]
    fn a_refused_key_and_a_rate_limit_read_differently() {
        assert!(why(401, "{}", "search").contains("refused"));
        assert!(why(429, "", "search").contains("rate limiting"));
        assert!(why(500, r#"{"error":"upstream on fire"}"#, "reader").contains("upstream on fire"));
        // Not JSON at all -- an HTML error page. Still says something.
        let said = why(503, "<html><body>gateway</body></html>", "reader");
        assert!(said.contains("503"), "{said}");
    }

    /// The whole path, against the real services rather than against a
    /// recording. Ignored by default because the sandbox this is normally built
    /// in cannot reach them, and because a test suite should not spend money:
    ///
    ///     EXA_API_KEY=... JINA_API_KEY=... \
    ///       cargo test --offline -p portfolio -- --ignored --nocapture
    ///
    /// Run it through `tmux`, which is where a shell with the open internet
    /// lives on this machine -- see SESSION.md. This is the check that the
    /// recorded fixtures above cannot make: that the request is shaped the way
    /// the service wants, that the key goes in the header it is read from, and
    /// that what comes back today still parses.
    #[test]
    #[ignore = "reaches the internet and spends a search"]
    fn a_live_search_comes_back_with_pages() {
        let hits = search("Agent Client Protocol session/prompt").expect("search failed");
        assert!(!hits.is_empty(), "no results at all");
        for h in &hits {
            assert!(is_http(&h.url), "not an address: {}", h.url);
            assert!(!h.title.is_empty(), "a result with no label");
        }
        println!("{} hits, first: {} {}", hits.len(), hits[0].title, hits[0].url);
    }

    #[test]
    #[ignore = "reaches the internet"]
    fn a_live_page_comes_back_as_text() {
        let page = read("https://example.com").expect("read failed");
        assert!(page.text.contains("Example Domain"), "{}", page.text);
        println!("{} -- {} chars, clipped {}", page.title, page.text.len(), page.clipped);
    }

    /// An address that goes nowhere comes back as the reader's own words, not as
    /// a dump of its JSON and not as an empty page.
    ///
    /// Written against what the service actually does, which is not what was
    /// assumed: a *path* that does not exist is answered by most sites with
    /// their own "not found" page, and the reader returns that with a 200 --
    /// correctly, it read the page it was given. It is an unresolvable *host*
    /// that fails, as a 422 whose `message` says why. That message is the one
    /// the model needs, so this checks it survives `why`.
    #[test]
    #[ignore = "reaches the internet"]
    fn a_live_dead_address_is_reported_in_the_services_own_words() {
        let said = read("https://no-such-host-42424242.example/").unwrap_err();
        println!("dead address said: {said}");
        assert!(said.contains("could not be resolved"), "{said}");
        assert!(!said.contains('{'), "the raw JSON reached the model: {said}");
    }

    /// A query too short to mean anything never reaches the network, and never
    /// spends a lookup.
    #[test]
    fn an_empty_query_is_refused_here_rather_than_paid_for() {
        assert!(search(" ").is_err());
        assert!(read("nonsense").is_err());
    }
}
