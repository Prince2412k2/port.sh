//! Speaking the Agent Client Protocol over stdio.
//!
//! The library above knows nothing about this; the binary is a shell that turns
//! loop events into `session/update` notifications and JSON-RPC calls into
//! prompts. Keeping the boundary means loop behaviour is tested as Rust calls
//! against recorded conversations, and these modules only have to cover framing
//! and dispatch.

pub mod rpc;
pub mod server;
pub mod update;
