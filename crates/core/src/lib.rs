//! Core domain contracts and runtime primitives for `termstage`.
//!
//! The browser terminal mode is split into protocol validation, local-only security
//! checks, backend session gateway contracts, and the current PTY runtime actor.
//! Application crates wire these pieces to HTTP, WebSocket, and CLI surfaces.

pub mod backend;
pub mod operation_lock;
pub mod protocol;
pub mod runtime;
pub mod security;
pub mod session_registry;
pub mod tunnel;
