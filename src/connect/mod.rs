//! Connectivity subsystem: probe 22 -> 443 -> console, and wrap system SSH.

pub mod probe;
pub mod relay;
pub mod ssh;

pub use probe::{probe_ssh_port, resolve_connection_port, resolve_connection_port_with_relay};
pub use relay::Relay;
pub use ssh::{build_ssh_args, build_ssh_command, exec_interactive_shell, exec_remote_command};
