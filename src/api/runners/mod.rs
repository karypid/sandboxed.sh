//! Per-backend turn-runner support modules.
//!
//! This is the landing zone for the mission_runner.rs decomposition: shared,
//! backend-agnostic pieces (error classification, and eventually the per
//! backend turn runners themselves) move here so `mission_runner.rs` can
//! shrink down to orchestration (dispatch, retry/fallback, TerminalReason).

pub(crate) mod codex;
pub(crate) mod errors;
pub(crate) mod gemini;
pub(crate) mod grok;
