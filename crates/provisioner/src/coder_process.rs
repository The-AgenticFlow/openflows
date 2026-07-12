//! Coder workspace process management (stub — Phase 4/5 will reimplement).
//!
//! The old module spawned CLI agents inside Coder workspaces via the exec API.
//! In the Coder-only redesign, the Coder control-plane agent loop replaces this.
//! This stub exists so the crate compiles; Phase 4/5 will add workspace lifecycle
//! helpers as needed.

// No exports needed yet — consumers use coder-client directly for workspace CRUD.
