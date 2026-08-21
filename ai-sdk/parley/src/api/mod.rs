//! One module per wire protocol.
//!
//! A provider is data pointing at one of these; adding a company that speaks a
//! shape already here costs a base URL and a catalogue entry rather than an
//! integration.

pub mod ollama_chat;
pub mod openai_completions;
pub mod openai_responses;

pub use ollama_chat::OllamaChat;
pub use openai_completions::OpenaiCompletions;
pub use openai_responses::OpenaiResponses;

use crate::types::Api;
use crate::wire::Wire;

/// The wire for an api. Boxed because the whole point is that the caller does
/// not know which one it got.
pub fn wire(api: Api) -> Box<dyn Wire> {
    match api {
        Api::OpenaiCompletions => Box::new(OpenaiCompletions),
        Api::OpenaiResponses => Box::new(OpenaiResponses),
        Api::OllamaChat => Box::new(OllamaChat),
    }
}
