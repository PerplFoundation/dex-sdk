//! Exchange event streams.
//!
//! * [`EventSource`]: continuous, commit-state-tagged stream of raw events,
//!   polling and/or subscribing over websocket, with the [`once`] and
//!   [`finalized`] adapters for consumers that want one batch per block.
//! * [`raw`]: the original polling-only stream, one `Voted` batch per block.
//! * [`trade`]: normalized trades derived from either of the above.

mod raw;
pub use raw::*;

mod source;
pub use source::*;

mod trade;
pub use trade::*;
