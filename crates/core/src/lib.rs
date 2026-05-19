//! Core domain contracts and runtime primitives for `presenterm`.
//!
//! The browser terminal mode is split into protocol validation, local-only security
//! checks, and a PTY runtime actor. Application crates wire these pieces to HTTP,
//! WebSocket, and CLI surfaces.

pub mod protocol;
pub mod runtime;
pub mod security;
