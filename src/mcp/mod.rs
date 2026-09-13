//! Model Context Protocol (MCP) server implementation for AI assistant orchestration.

pub mod protocol;
pub mod resources;
pub mod server;
pub mod tools;

pub use server::run_stdio;
