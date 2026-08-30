//! Owner authentication, passkey ceremonies, TOTP login, and recovery.

pub mod interface;

pub use interface::{
    AuthContext, AuthMode, AuthenticationGrant, AuthenticationMode, CeremonyOptions, IdentityError,
    IdentityInterface, InitializationState, OwnerView, PasskeyView, RecoveryGrant, TotpProvision,
};
