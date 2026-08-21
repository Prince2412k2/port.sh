//! What the agent on this box is allowed to be.
//!
//! One table that every other file asks, rather than each deciding for itself.
//! Before this the answer lived in three places that nothing made agree: a
//! capability literal inside the initialize handshake, an `ALLOWED_TOOLS`
//! array, and a pair of budget constants. A gate advertised as shut and then
//! enforced as open is worse than either of those alone, because it reads as
//! safe. Here the bytes on the wire are *derived* from the same constant the
//! enforcement reads, so the two cannot drift apart.
//!
//! Compile time is deliberate. These gates decide whether a public SSH server
//! that accepts any username will run a shell for a stranger, and a file on
//! disk is one bad mount away from being absent, empty, or somebody else's --
//! the dangling-symlink incident in SESSION.md is exactly that failure. Turning
//! one of these on is a rebuild and a redeploy, which is the right amount of
//! friction for the question "may a visitor write to this machine".
//!
//! **Default deny.** `verdict` maps the methods we know about and answers
//! `Unimplemented` for everything else, so a method added to ACP next month
//! arrives shut rather than unhandled.

/// One ACP feature this client can hand to an agent.
///
/// Every field is named for the protocol method it governs rather than for the
/// capability word ACP happens to use, because the method name is what arrives
/// on the wire and what has to be matched against.
pub struct Gates {
    /// `fs/read_text_file`
    pub fs_read: bool,
    /// `fs/write_text_file`
    pub fs_write: bool,
    /// The whole `terminal/*` family. One gate rather than five: a terminal you
    /// can create but not read is not a smaller grant, it is a broken one.
    pub terminal: bool,
    /// `elicitation/create` -- the agent asking the visitor a question of its
    /// own. Shut because there is nobody at this end to answer it: the person
    /// reading is a stranger looking at a portfolio, not an operator.
    pub elicitation: bool,
    /// `$/cancel_request`. Open, and the only gate here that grants nothing --
    /// it lets us *stop* work rather than start it.
    pub cancel: bool,
    /// Questions per session.
    pub turns: usize,
    /// Tool calls per session, across every question.
    ///
    /// A total rather than a rate, because the thing being prevented is somebody
    /// using this box as a free web crawler, and that is a total.
    pub tool_calls: usize,
    /// Searches and page reads per session, across every question.
    ///
    /// Separate from `tool_calls` because these are the only tools that cost
    /// money: a map lookup reads an index on this disk, a search is seven tenths
    /// of a cent on somebody's account. Any username with any key opens a
    /// session here, so an uncapped search tool is a stranger's budget.
    ///
    /// Also a total, and for the same reason -- but a smaller one, because the
    /// failure it prevents is a bill rather than a busy afternoon. Twelve is one
    /// per question, which is what an honest conversation needs; the reply tells
    /// the agent how many are left so it can spend them on purpose.
    pub web_calls: usize,
}

/// The shipped policy: read the web, let us stop it, nothing else.
pub const GATES: Gates = Gates {
    fs_read: false,
    fs_write: false,
    terminal: false,
    elicitation: false,
    cancel: true,
    turns: 12,
    tool_calls: 24,
    web_calls: 12,
};

/// A tool the agent may reach for, and whether it may.
pub struct Tool {
    /// What it is called on screen. One name per capability, not per spelling.
    pub name: &'static str,
    /// The same capability under another agent's spelling.
    ///
    /// Tool names are not standardised and the differences are not cosmetic:
    /// opencode calls fetching a page `webfetch`, Copilot calls it `web_fetch`,
    /// and neither string contains the other. Without these the gate would
    /// refuse Copilot every web tool it has -- and, worse, refuse its shell
    /// under the name `shell` only because the table happened to say `bash`.
    pub aka: &'static [&'static str],
    pub open: bool,
    /// Shown on screen next to the tool. Present tense, lower case, short.
    pub blurb: &'static str,
    /// Served by us, over our own MCP server, rather than being one of the
    /// agent's own.
    ///
    /// It matters because an agent renames what it did not write. Copilot
    /// namespaces every MCP tool as `<server>-<tool>`, so `show_map` reaches
    /// the model as `portfolio-show_map` -- and its `--available-tools`
    /// allow-list is by exact name, so a list of bare names hid every tool we
    /// serve while the server itself connected perfectly. Nothing looked wrong
    /// anywhere: the log said the tools were offered, the agent said it had no
    /// map tools, and both were true.
    pub ours: bool,
}

impl Tool {
    /// Every spelling of this tool.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.name).chain(self.aka.iter().copied())
    }
}

/// Every tool this client has an opinion about.
///
/// Listed with a flag rather than trimmed to the allowed ones, so the closed
/// ones stay visible -- both on screen and to whoever edits this next. A tool
/// missing from a list and a tool switched off look identical in a diff; only
/// one of them is a decision somebody made.
pub const TOOLS: &[Tool] = &[
    // Aliases are generous on the shut side and stingy on the open side, because
    // the two mistakes are not the same size: an over-broad shut name costs a
    // refused fetch, an over-broad open name grants something. `fetch` alone is
    // deliberately *not* here -- it is an ordinary English word, and matching it
    // by containment would let a tool called anything at all be granted by a
    // label that merely mentions fetching.
    Tool { name: "webfetch", aka: &["web_fetch"], open: true, ours: false, blurb: "read a page" },
    Tool { name: "websearch", aka: &["web_search"], open: true, ours: false, blurb: "search the web" },
    // Ours, served from this process -- see `mcp.rs`. Open because the whole
    // point of them is that the agent decides when a map belongs on screen, and
    // neither one can do anything but draw: `locate_place` reads an index built
    // from the basemap, and `show_map` posts a point to the page that asked. No
    // filesystem, no network, no shell.
    //
    // Named exactly, with no aliases. The rule on the open side is stinginess,
    // and these are names we chose ourselves -- there is no upstream that might
    // rename them and no prose that should ever match one.
    Tool { name: "locate_place", aka: &[], open: true, ours: true, blurb: "find a place" },
    Tool { name: "show_map", aka: &[], open: true, ours: true, blurb: "draw a map" },
    Tool { name: "locate_visitor", aka: &[], open: true, ours: true, blurb: "where you are" },
    Tool { name: "hide_map", aka: &[], open: true, ours: true, blurb: "put the map away" },
    // Also ours, and the only two that leave this box. They are here because
    // "can look something up" used to arrive and leave with whichever server was
    // answering: Copilot's seat brings its own web tools, most of the free models
    // bring none, and the agent in `ai-sdk/` has none at all by design. The two
    // above them stay open as well -- a server that brings its own search should
    // use it rather than spend ours.
    //
    // Named so that no prose can match one. `search_web` rather than
    // `web_search`, which is already Copilot's spelling of its own; `fetch_page`
    // rather than `read_page`, because `read` is a shut tool and containment
    // would have refused ours under the name of that one.
    Tool { name: "search_web", aka: &[], open: true, ours: true, blurb: "find pages about a thing" },
    Tool { name: "fetch_page", aka: &[], open: true, ours: true, blurb: "read a page as text" },
    // Nothing provides this one. `/reach` is handled in ask.rs before the
    // agent ever sees the line, deliberately: a message meant for a person
    // should arrive whether or not a model is up, and word for word rather
    // than as something's summary of it. Off, so that if a tool by this name
    // ever does appear it arrives shut and somebody decides on purpose.
    Tool { name: "reach_out", aka: &[], open: false, ours: false, blurb: "leave Prince a message" },
    // Named and shut so the refusal is visible rather than implied. This is
    // the one that matters: a shell on a box that accepts any username is
    // arbitrary code execution for anyone who can type, and it answers to at
    // least two names depending on who is asking.
    Tool { name: "bash", aka: &["shell"], open: false, ours: false, blurb: "run a command" },
    Tool { name: "edit", aka: &["str_replace"], open: false, ours: false, blurb: "change a file" },
    Tool { name: "write", aka: &[], open: false, ours: false, blurb: "create a file" },
    Tool { name: "patch", aka: &[], open: false, ours: false, blurb: "apply a diff" },
    // Reading the filesystem is not the same risk as writing it, and it is
    // still not this agent's business: the context it needs is pushed into the
    // first prompt, and everything else on this disk is either source control
    // or somebody's messages.
    Tool { name: "view", aka: &["read"], open: false, ours: false, blurb: "read a file" },
    Tool { name: "glob", aka: &[], open: false, ours: false, blurb: "find files by name" },
    Tool { name: "grep", aka: &[], open: false, ours: false, blurb: "search the files" },
    Tool { name: "task", aka: &[], open: false, ours: false, blurb: "spawn a sub-agent" },
];

/// Every spelling of every tool that is open, for a server that takes its
/// allow-list on the command line.
///
/// All spellings rather than the display names, because the flag is read by an
/// agent that knows its own vocabulary and not ours. Extra names an agent does
/// not recognise are ignored by it, which is the harmless direction.
/// Every name an agent might use for a tool that is open, for an allow-list.
///
/// Ours appear twice: bare, and prefixed with the MCP server's name. An agent
/// that namespaces its MCP tools will only match the prefixed form and one that
/// does not will only match the bare one, and there is no way to tell which
/// from the handshake -- so both go in. An allow-list with a name in it that
/// nothing answers to costs nothing; a missing one costs the whole feature.
/// Whether this is one of the tools we serve ourselves.
///
/// Used to decide which of two rows describing the same call to draw. The agent
/// reports it over ACP with whatever name it renamed it to and no arguments; our
/// own tool server reports it with the name we gave it and what it was asked
/// for. Both are the same call and only one of them is worth reading.
pub fn ours(name: &str) -> bool {
    let t = flatten(name);
    TOOLS.iter().filter(|x| x.ours).any(|x| x.names().any(|n| t.contains(&flatten(n))))
}

pub fn open_tool_names() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in TOOLS.iter().filter(|t| t.open) {
        for n in t.names() {
            out.push(n.to_string());
            if t.ours {
                out.push(format!("{}-{n}", crate::mcp::SERVER_NAME));
            }
        }
    }
    out
}

/// The protocol capabilities, as label-and-state pairs for the screen.
///
/// Ordered as they would be worried about rather than alphabetically: writing to
/// the machine first, reading it second, everything else after.
pub fn capabilities() -> [(&'static str, bool); 5] {
    [
        ("fs.write", GATES.fs_write),
        ("fs.read", GATES.fs_read),
        ("terminal", GATES.terminal),
        ("elicitation", GATES.elicitation),
        ("cancel", GATES.cancel),
    ]
}

/// Whether a tool may be used. Anything unrecognised is refused.
///
/// Fed a tool call's machine `name` where the agent sends one -- ACP's
/// `toolCall.name`, "read_file" rather than "Read configuration" -- and its
/// human `title` only as a fallback, because that field is optional and this
/// box has never seen a real `session/request_permission` to know which arrives.
///
/// Matched by containment rather than equality, since a name may be namespaced
/// (`web.fetch`, `mcp__x__webfetch`). That cuts both ways, so a shut tool is
/// checked first: a string naming both an open and a closed tool is refused.
/// Under-granting is the safe direction here and the direction this errs in --
/// a refused fetch is a worse answer, an unrecognised shell is a broken box.
pub fn tool_open(name: &str) -> bool {
    let t = flatten(name);
    !tool_shut(name) && TOOLS.iter().any(|x| x.open && x.names().any(|n| t.contains(&flatten(n))))
}

/// Whether a string names a tool that is shut.
///
/// Separate from `tool_open` because a permission request carries several fields
/// and they get different treatment: one of them decides the grant, but *any* of
/// them naming a closed tool is enough to refuse. "webfetch, then bash" must not
/// be granted on the strength of its first word.
pub fn tool_shut(name: &str) -> bool {
    let t = flatten(name);
    TOOLS.iter().any(|x| !x.open && x.names().any(|n| t.contains(&flatten(n))))
}

/// Lower case, and with the separators taken out.
///
/// Because an agent spells a tool however it likes and this has now cost two
/// deploys. opencode says `webfetch` and Copilot says `web_fetch`; Copilot also
/// prefixes MCP tools with the server name, and ACP's `ToolCall` has no `name`
/// field at all -- only a `title`, which may be prose. `Locate place`,
/// `locate_place` and `portfolio-locate_place` are one tool and the gate has to
/// know it.
///
/// This widens the shut side and the open side together, which is the point:
/// the names in the table are specific identifiers rather than words, so
/// flattening them cannot make `bash` match anything that is not a shell, and
/// the shut side is checked first either way.
fn flatten(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// What we will do with an inbound method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Implemented here, and the gate is open.
    Open,
    /// Understood, and shut. The string names the gate, for the log and screen.
    Shut(&'static str),
    /// Not implemented at all. Indistinguishable from `Shut` on the wire; kept
    /// apart so the screen can tell "switched off" from "never existed".
    Unimplemented,
}

/// The gate on one agent-to-client method.
///
/// The permission request is `Open` unconditionally because answering it is how
/// tools get refused -- shutting the method itself would leave an agent waiting
/// on a reply that never comes, which is a hang rather than a policy.
pub fn verdict(method: &str) -> Verdict {
    match method {
        "session/request_permission" => Verdict::Open,
        "fs/read_text_file" => gate(GATES.fs_read, "fs.read"),
        "fs/write_text_file" => gate(GATES.fs_write, "fs.write"),
        "terminal/create" | "terminal/output" | "terminal/release" | "terminal/wait_for_exit"
        | "terminal/kill" => gate(GATES.terminal, "terminal"),
        "elicitation/create" => gate(GATES.elicitation, "elicitation"),
        _ => Verdict::Unimplemented,
    }
}

fn gate(open: bool, name: &'static str) -> Verdict {
    if open {
        Verdict::Open
    } else {
        Verdict::Shut(name)
    }
}

/// `clientCapabilities`, as ACP v1 wants it, built from the table above.
///
/// Advertised even where the answer is false. An omitted capability and one
/// declared false mean the same thing to a conforming agent, but only the
/// second says it on purpose -- and this string is the one place a reader can
/// check what the handshake actually claimed.
pub fn client_capabilities() -> String {
    format!(
        concat!(
            r#"{{"fs":{{"readTextFile":{},"writeTextFile":{}}},"#,
            r#""terminal":{}}}"#
        ),
        GATES.fs_read, GATES.fs_write, GATES.terminal
    )
}

/// The tool policy, as opencode's config document wants it.
///
/// Belt and braces, and the braces are in `acp.rs`: `tools` is what the agent
/// is told it has and `permission` is what happens if it asks anyway, but
/// neither is trusted on its own. Every request is checked again by name when
/// it arrives, because a config key renamed upstream should cost a refused
/// tool call rather than a shell.
pub fn tool_policy() -> String {
    // Every spelling gets an entry. opencode ignores keys it does not know, and
    // a policy that names only our word for a tool is a policy with a hole in it
    // the day the agent renames one.
    let list = |render: fn(&Tool, &str) -> String| {
        TOOLS
            .iter()
            .flat_map(|t| t.names().map(move |n| render(t, n)))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        r#""tools":{{{}}},"permission":{{{}}}"#,
        list(|t, n| format!("{}:{}", crate::json::quote(n), t.open)),
        list(|t, n| format!(
            "{}:{}",
            crate::json::quote(n),
            if t.open { "\"allow\"" } else { "\"deny\"" }
        )),
    )
}

#[cfg(test)]
mod tests {

    /// However the agent spells it, it is the same tool.
    ///
    /// Every spelling here is one that has actually turned up or is one step
    /// from it: opencode's `webfetch` against Copilot's `web_fetch`, Copilot's
    /// `<server>-<tool>` namespacing, and a `title` that is prose because ACP's
    /// `ToolCall` carries no name field and prose is all there is.
    #[test]
    fn a_tool_is_the_same_tool_however_it_is_spelled() {
        for open in [
            "show_map",
            "portfolio-show_map",
            "portfolio_show_map",
            "Show map",
            "mcp__portfolio__show_map",
            "web_fetch",
            "webfetch",
            "locate_visitor",
            "portfolio-locate_visitor",
        ] {
            assert!(tool_open(open), "`{open}` was refused");
        }
        // And the shut side widens with it rather than being left behind, which
        // is the half that matters: a flatten that only helped the open side
        // would be a way through the gate.
        for shut in [
            "bash",
            "Bash",
            "portfolio-bash",
            "str_replace",
            "str-replace",
            "strreplace",
            "Str Replace Editor",
            "shell",
            "read",
            "read_file",
        ] {
            assert!(tool_shut(shut), "`{shut}` got through");
            assert!(!tool_open(shut), "`{shut}` was granted");
        }
        // A category is not a name. This is the exact string that refused our
        // own tools for a whole deploy.
        assert!(!tool_open("other"), "`other` is a tool kind, not a tool");
        assert!(!tool_open(""), "an empty name granted something");
    }

    /// The allow-list has to carry the name the *agent* will use.
    ///
    /// The regression: `--available-tools` on Copilot means "only these tools
    /// will be available to the model", matched by exact name -- and Copilot
    /// renames every MCP tool to `<server>-<tool>`. So a list of bare names
    /// connected the server, negotiated the tools, and hid all of them from the
    /// model. The agent then said, correctly, that it had no map tools.
    #[test]
    fn the_allow_list_carries_the_namespaced_names_of_our_own_tools() {
        let names = open_tool_names();
        for t in TOOLS.iter().filter(|t| t.open && t.ours) {
            let bare = t.name.to_string();
            let owned = format!("{}-{}", crate::mcp::SERVER_NAME, t.name);
            assert!(names.contains(&bare), "`{bare}` missing from {names:?}");
            assert!(names.contains(&owned), "`{owned}` missing -- Copilot will not see it");
        }
        // The agent's own tools are not ours to rename.
        assert!(names.contains(&"web_fetch".to_string()));
        assert!(
            !names.iter().any(|n| n == &format!("{}-web_fetch", crate::mcp::SERVER_NAME)),
            "namespaced a tool we do not serve: {names:?}"
        );
        // And nothing shut leaked in under either spelling.
        for t in TOOLS.iter().filter(|t| !t.open) {
            for n in t.names() {
                assert!(!names.contains(&n.to_string()), "`{n}` is shut and on the allow-list");
            }
        }
    }
    use super::*;

    #[test]
    fn the_advertised_capabilities_are_the_enforced_ones() {
        let wire = client_capabilities();
        // The point of the module: what we say and what we do come from one
        // constant. If someone opens a gate and the handshake keeps claiming
        // it is shut, this is what notices.
        for (open, needle) in [
            (GATES.fs_read, "\"readTextFile\":true"),
            (GATES.fs_write, "\"writeTextFile\":true"),
            (GATES.terminal, "\"terminal\":true"),
        ] {
            assert_eq!(open, wire.contains(needle), "{needle} disagrees with the table");
        }
        assert!(crate::json::parse(&wire).is_some(), "not valid JSON: {wire}");
    }

    #[test]
    fn every_shut_gate_refuses_its_methods() {
        for m in [
            "fs/read_text_file",
            "fs/write_text_file",
            "terminal/create",
            "terminal/output",
            "terminal/release",
            "terminal/wait_for_exit",
            "terminal/kill",
            "elicitation/create",
        ] {
            let open = matches!(verdict(m), Verdict::Open);
            assert!(!open, "{m} is open; the shipped policy grants none of these");
        }
    }

    #[test]
    fn a_method_nobody_has_heard_of_is_refused_rather_than_ignored() {
        assert_eq!(verdict("fs/delete_everything"), Verdict::Unimplemented);
        assert_eq!(verdict("terminal/spawn_v2"), Verdict::Unimplemented);
    }

    #[test]
    fn permission_requests_are_always_answered() {
        // Shutting this would hang the agent rather than restrain it.
        assert_eq!(verdict("session/request_permission"), Verdict::Open);
    }

    #[test]
    fn a_shell_is_refused_however_it_is_labelled() {
        assert!(!tool_open("bash"));
        assert!(!tool_open("Bash(rm -rf /)"));
        assert!(!tool_open("Run a command with bash"));
        // A title mentioning an open and a shut tool is refused, not granted.
        assert!(!tool_open("webfetch then bash"));
    }

    #[test]
    fn the_web_tools_are_granted_by_name() {
        assert!(tool_open("webfetch"));
        assert!(tool_open("websearch"));
        // Namespaced and cased variants of the same name still land.
        assert!(tool_open("mcp__tools__webfetch"));
        assert!(tool_open("WebSearch"));
    }

    /// A human label on its own does not grant anything.
    ///
    /// "Fetch https://example.com" is a plausible `title` for a call whose
    /// `name` is `webfetch`, and it is deliberately *not* enough: matching prose
    /// loosely enough to catch it would also catch prose that merely mentions a
    /// tool. The cost is a refused fetch when an agent sends no name, which is
    /// the failure worth having. Do not "fix" this into a fuzzy match.
    #[test]
    /// Ours are open under every spelling an agent will use for them, and --
    /// the part that is easy to get wrong -- they are not caught by the shut
    /// side's containment. `read_page` would have been: `read` is `view`'s alias
    /// and a shut name is checked first, so the tool would have been refused
    /// under the name of a tool nobody offers. `fetch_page` is the name for
    /// that reason and this is the test that says so.
    #[test]
    fn our_web_tools_are_open_and_are_not_caught_by_a_shut_name() {
        for n in ["search_web", "fetch_page"] {
            assert!(tool_open(n), "`{n}` was refused");
            assert!(!tool_shut(n), "`{n}` matched something shut");
            let owned = format!("{}-{n}", crate::mcp::SERVER_NAME);
            assert!(tool_open(&owned), "`{owned}` was refused");
        }
        // The name that was not chosen, and why.
        assert!(tool_shut("read_page"), "`read` no longer guards reading files");
        // Reading a file is still refused, under every spelling it has.
        for n in ["view", "read", "Read a file"] {
            assert!(!tool_open(n), "`{n}` was granted");
        }
    }

    #[test]
    fn a_human_title_alone_grants_nothing() {
        assert!(!tool_open("Fetch https://example.com"));
        assert!(!tool_open("Search the web for rust"));
    }

    #[test]
    fn an_unknown_tool_is_not_granted_by_default() {
        assert!(!tool_open("send_email"));
        assert!(!tool_open(""));
    }

    #[test]
    fn the_tool_policy_denies_everything_it_does_not_allow() {
        let doc = format!("{{{}}}", tool_policy());
        let v = crate::json::parse(&doc).expect("valid JSON");
        for t in TOOLS {
            let told = v.get("tools").and_then(|x| x.get(t.name)).and_then(|x| x.as_bool());
            let asked = v.get("permission").and_then(|x| x.get(t.name)).and_then(|x| x.as_str());
            assert_eq!(told, Some(t.open), "tools.{} disagrees with the table", t.name);
            assert_eq!(
                asked,
                Some(if t.open { "allow" } else { "deny" }),
                "permission.{} disagrees with the table",
                t.name
            );
        }
    }

    #[test]
    fn the_shell_is_shut_in_the_policy_document_too() {
        let p = tool_policy();
        assert!(p.contains(r#""bash":false"#));
        assert!(p.contains(r#""bash":"deny""#));
    }
}
