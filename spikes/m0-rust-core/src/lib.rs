pub mod core;
pub mod framing;
pub mod model;
pub mod protocol;
pub mod store;
pub mod worker;

pub use core::{PromptRequest, SpikeCore};
pub use model::{ClientState, Event, RunRecord, SpikeError, SpikeResult};
pub use protocol::serve_jsonl;
pub use store::Store;
