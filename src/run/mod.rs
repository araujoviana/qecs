//! `qecs run` subsystem: detector ladder, workdir synchronization, and remote execution.

pub mod detect;
pub mod execute;
pub mod sync;

pub use detect::{RunRecipe, detect_recipe};
