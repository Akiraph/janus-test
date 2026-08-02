//! Technical persistence primitives for Janus.
//!
//! Keep business vocabulary and orchestration above this crate. Work kinds and
//! event catalogs belong to their owning application modules.

pub mod clock;
pub mod database;
pub mod events;
pub mod id;
pub mod managed_storage;
pub mod operations;
pub mod unit_of_work;

pub fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Lease and step tokens are opaque capabilities, not domain IDs; UUID v7 keeps
/// them unique and gives recovery logs a useful time order.
pub fn random_hex_token() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}
