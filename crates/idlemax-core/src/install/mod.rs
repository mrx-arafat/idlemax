//! Domain: plans for `idlemax service` and `idlemax statusline`.
//! Pure: they decide which file, which content and which command; the CLI only performs them.

pub mod codex;
pub mod diff;
pub mod hooks;
pub mod json_edit;
pub mod opencode;
pub mod quote;
pub mod service;
pub mod statusline;
