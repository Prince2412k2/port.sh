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
};

/// A tool the agent may reach for, and whether it may.
pub struct Tool {
    pub name: &'static str,
    pub open: bool,
    /// Shown on screen next to the tool. Present tense, lower case, short.
    pub blurb: &'static str,
}

/// Every tool this client has an opinion about.
///
/// Listed with a flag rather than trimmed to the allowed ones, so the closed
/// ones stay visible -- both on screen and to whoever edits this next. A tool
/// missing from a list and a tool switched off look identical in a diff; only
/// one of them is a decision somebody made.
pub const TOOLS: &[Tool] = &[
    Tool { name: "webfetch", open: true, blurb: "read a page" },
    Tool { name: "websearch", open: true, blurb: "search the web" },
    // Nothing provides this one. `/reach` is handled in ask.rs before the
    // agent ever sees the line, deliberately: a message meant for a person
    // should arrive whether or not a model is up, and word for word rather
    // than as something's summary of it. Off, so that if a tool by this name
    // ever does appear it arrives shut and somebody decides on purpose.
    Tool { name: "reach_out", open: false, blurb: "leave Prince a message" },
    // Named and shut so the refusal is visible rather than implied. This is
    // the one that matters: `bash` on a box that accepts any username is
    // arbitrary code execution for anyone who can type.
    Tool { name: "bash", open: false, blurb: "run a command" },
    Tool { name: "edit", open: false, blurb: "change a file" },
    Tool { name: "write", open: false, blurb: "create a file" },
    Tool { name: "patch", open: false, blurb: "apply a diff" },
];

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
    let t = name.to_ascii_lowercase();
    !tool_shut(name) && TOOLS.iter().any(|x| x.open && t.contains(x.name))
}

/// Whether a string names a tool that is shut.
///
/// Separate from `tool_open` because a permission request carries several fields
/// and they get different treatment: one of them decides the grant, but *any* of
/// them naming a closed tool is enough to refuse. "webfetch, then bash" must not
/// be granted on the strength of its first word.
pub fn tool_shut(name: &str) -> bool {
    let t = name.to_ascii_lowercase();
    TOOLS.iter().any(|x| !x.open && t.contains(x.name))
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
    let list = |render: fn(&Tool) -> String| {
        TOOLS.iter().map(render).collect::<Vec<_>>().join(",")
    };
    format!(
        r#""tools":{{{}}},"permission":{{{}}}"#,
        list(|t| format!("{}:{}", crate::json::quote(t.name), t.open)),
        list(|t| format!(
            "{}:{}",
            crate::json::quote(t.name),
            if t.open { "\"allow\"" } else { "\"deny\"" }
        )),
    )
}

#[cfg(test)]
mod tests {
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
