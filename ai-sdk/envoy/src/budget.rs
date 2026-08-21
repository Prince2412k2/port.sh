//! Ceilings on a session.
//!
//! In the loop rather than in a hook, because `gates.rs` caps a visitor at
//! twelve turns and twenty-four tool calls and the thing being prevented is
//! somebody using a public SSH server as a free web crawler. A hook can be left
//! unregistered. A field cannot.
//!
//! Exhausting either ends the turn with ACP's own `max_turn_requests` rather
//! than an error: the conversation is intact, it just stops here.

#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// Model calls in one run. A run that needs more than this is looping.
    pub turns: usize,
    /// Tool calls in one run.
    pub tool_calls: usize,
}

impl Default for Budget {
    /// Generous enough not to interrupt honest work, low enough to stop a loop.
    /// A public deployment should set its own and not rely on this.
    fn default() -> Budget {
        Budget {
            turns: 24,
            tool_calls: 64,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Spent {
    pub turns: usize,
    pub tool_calls: usize,
}
