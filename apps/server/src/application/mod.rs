//! Cross-module workflows live here. They have no persistent business state;
//! they record correlation/operation journals and call Module interfaces.
//!
//! The background worker leases durable `work_items` and dispatches them to the
//! owning Module's handler, so clone/delete/git operations survive HTTP
//! disconnects and process restarts (`DAT-OP-01/02`).

pub mod workers;
