//! Connectivity subsystem: probe 22 -> 443 -> console, and wrap system SSH.

pub mod probe;
pub mod ssh;

pub use probe::{probe_ssh_port, resolve_connection_port};
pub use ssh::{exec_interactive_shell, exec_remote_command};
