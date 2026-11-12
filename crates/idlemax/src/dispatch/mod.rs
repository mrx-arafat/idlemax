//! Adapter: dispatcher use cases. The agent side reads Herdr and git on its own host (`herdr`,
//! `repo`, used by `agent::dispatch_feed`); the coordinator side turns the stored inputs into
//! tasks and the plan through the pure `idlemax_core::dispatch` rules.

pub mod herdr;
pub mod repo;
