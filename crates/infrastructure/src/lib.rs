//! Technical persistence primitives for Janus.
//!
//! Keep business vocabulary and orchestration above this crate. Work kinds and
//! event catalogs belong to their owning application modules.

pub mod clock;
pub mod command_idempotency;
pub mod database;
pub mod events;
pub mod id;
pub mod managed_storage;
pub mod operations;
pub mod secrets;
pub mod shell;
pub mod state_broadcaster;
pub mod unit_of_work;

/// Lease and step tokens are opaque capabilities, not domain IDs; UUID v7 keeps
/// them unique and gives recovery logs a useful time order.
pub(crate) fn random_hex_token() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}
