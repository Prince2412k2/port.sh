//! An agent: a loop that talks to a model, runs what it asks for, and tells you
//! everything it did.
//!
//! `parley` answers one question at a time. This turns that into a
//! conversation: tools, a turn loop, ceilings, and an event stream detailed
//! enough that a client can reconstruct the whole run rather than being handed
//! a finished answer.
//!
//! The loop is [`agent::run`], and it is a plain `Stream`. Nothing is spawned
//! and nothing is hidden behind a handle, so it can be driven from a test with
//! a recorded conversation and no network at all.

pub mod acp;
pub mod agent;
pub mod client_tool;
pub mod compact;
pub mod config;
pub mod budget;
pub mod event;
pub mod mcp;
pub mod store;
pub mod tool;

pub use agent::{run, Config};
pub use compact::Compaction;
pub use store::Store;
pub use budget::Budget;
pub use event::{End, Event};
pub use tool::{Failed, Kind, Mode, Output, Set, Spec, Tool};
