//! `qecs run` subsystem: detector ladder, workdir synchronization, and remote execution.

pub mod detect;
pub mod diagnostics;
pub mod execute;
pub mod session;
pub mod sync;
pub mod tunnel;

pub use detect::{RunRecipe, detect_recipe};
