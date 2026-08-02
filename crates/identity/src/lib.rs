//! Owner authentication, passkey ceremonies, and recovery.

pub mod interface;

pub use interface::{
    AuthContext, AuthenticationGrant, AuthenticationMode, CeremonyOptions, IdentityError,
    IdentityInterface, InitializationState, OwnerView, PasskeyView, RecoveryGrant,
};
