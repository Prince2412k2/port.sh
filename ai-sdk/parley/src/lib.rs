//! Talking to a language model, without caring which one.
//!
//! The layering that matters is between an **api** and a **provider**. An api
//! is a wire protocol: how a request is framed, and what shape the events come
//! back in. A provider is a base URL, a credential, and a list of models, and
//! it points at one api. Most providers are therefore data rather than code --
//! which is how a handful of api implementations covers most of the field.
//!
//! Nothing here runs an agent. This crate answers one question at a time and
//! has no opinion about tools beyond passing their schemas along; the loop that
//! calls tools and comes back for another turn lives in `envoy`.

pub mod api;
pub mod auth;
pub mod canned;
pub mod error;
pub mod fallback;
pub mod http;
pub mod ndjson;
pub mod sse;
pub mod stream;
pub mod types;
pub mod wire;

pub use error::{Error, Result};
pub use auth::Auth;
pub use canned::{Canned, Cassette};
pub use fallback::{Fallback, Tier};
pub use wire::{EventStream, Frames, Wire};
pub use stream::{Accumulator, Event, Kind};
pub use types::{
    Api, Assistant, Block, Context, Cost, Effort, Endpoint, Message, Model, Options, Request, Stop,
    Tool, Tuning, Usage,
};
