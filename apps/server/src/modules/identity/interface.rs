//! Public identity Module boundary. Owner authentication arrives in M1.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitializationState {
    Uninitialized,
    Initialized,
}
