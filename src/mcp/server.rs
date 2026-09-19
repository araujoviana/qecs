//! MCP stdio JSON-RPC server transport and dispatcher.

use serde_json::json;
use std::io::{BufRead, Write};

use crate::ctx::Ctx;
use crate::mcp::protocol::*;
use crate::mcp::resources::{list_resources, read_resource};
use crate::mcp::tools::{execute_tool, list_tools};

#[cfg(unix)]
use std::os::unix::io::FromRawFd;

pub struct StdioIsolation {
    #[cfg(unix)]
    orig_stdout_fd: Option<std::os::unix::io::RawFd>,
}

impl StdioIsolation {
    pub fn redirect_stdout_to_stderr() -> (Self, Box<dyn std::io::Write + Send>) {
        #[cfg(unix)]
        {
            unsafe {
                let orig_fd = libc::dup(1);
                if orig_fd >= 0 {
                    // Redirect stdout (fd 1) to stderr (fd 2) so any commands calling println! don't corrupt JSON-RPC
                    libc::dup2(2, 1);
                    let rpc_file = std::fs::File::from_raw_fd(orig_fd);
                    return (
                        StdioIsolation {
                            orig_stdout_fd: Some(orig_fd),
                        },
                        Box::new(std::io::BufWriter::new(rpc_file)),
                    );
                }
            }
        }
        (
            StdioIsolation {
                #[cfg(unix)]
                orig_stdout_fd: None,
            },
            Box::new(std::io::stdout()),
        )
    }
}

#[cfg(unix)]
impl Drop for StdioIsolation {
    fn drop(&mut self) {
        if let Some(orig_fd) = self.orig_stdout_fd {
            unsafe {
                libc::dup2(orig_fd, 1);
                libc::close(orig_fd);
            }
        }
    }
}

/// Run the MCP server over standard input and standard output until EOF.
pub async fn run_stdio(ctx: &Ctx) -> anyhow::Result<()> {
    let mut mcp_ctx = ctx.clone();
    mcp_ctx.global.quiet = true;
    mcp_ctx.global.json = true;

    let (_isolation, mut stdout) = StdioIsolation::redirect_stdout_to_stderr();
    let stdin = std::io::stdin();

    eprintln!(
        "qecs MCP server started (protocol version {MCP_PROTOCOL_VERSION}). Listening on stdio..."
    );

    for line_res in stdin.lock().lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(e) => {
                eprintln!("qecs MCP stdio read error: {e}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let err_resp = JsonRpcResponse::error(
                    serde_json::Value::Null,
                    error_codes::PARSE_ERROR,
                    format!("parse error: {e}"),
                );
                let json_bytes = serde_json::to_vec(&err_resp)?;
                stdout.write_all(&json_bytes)?;
                stdout.write_all(b"\n")?;
                stdout.flush()?;
                continue;
            }
        };

        let resp = dispatch_request(&mcp_ctx, request).await;

        // If the request was a notification (no id), we do not send a response
        if let Some(resp) = resp {
            let json_bytes = serde_json::to_vec(&resp)?;
            stdout.write_all(&json_bytes)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }

    eprintln!("qecs MCP server exiting on EOF.");
    Ok(())
}

/// Process a single incoming JSON-RPC request and return the response (or None for notifications).
pub async fn dispatch_request(ctx: &Ctx, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    let id = req.id.clone();

    match req.method.as_str() {
        "initialize" => {
            let id = id.unwrap_or(serde_json::Value::Null);
            let result = InitializeResult {
                protocol_version: MCP_PROTOCOL_VERSION.to_string(),
                capabilities: ServerCapabilities {
                    tools: json!({}),
                    resources: json!({}),
                },
                server_info: ServerInfo {
                    name: "qecs".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
            };
            Some(JsonRpcResponse::success(
                id,
                serde_json::to_value(result).unwrap(),
            ))
        }
        "notifications/initialized" => {
            // Client acknowledgment notification: no response needed
            None
        }
        "ping" => {
            let id = id.unwrap_or(serde_json::Value::Null);
            Some(JsonRpcResponse::success(id, json!({})))
        }
        "tools/list" => {
            let id = id.unwrap_or(serde_json::Value::Null);
            let tools = list_tools();
            Some(JsonRpcResponse::success(id, json!({ "tools": tools })))
        }
        "tools/call" => {
            let id = id.unwrap_or(serde_json::Value::Null);
            let params = req.params.unwrap_or(json!({}));
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            let call_res = execute_tool(ctx, tool_name, &arguments).await;
            Some(JsonRpcResponse::success(
                id,
                serde_json::to_value(call_res).unwrap(),
            ))
        }
        "resources/list" => {
            let id = id.unwrap_or(serde_json::Value::Null);
            let resources = list_resources();
            Some(JsonRpcResponse::success(
                id,
                json!({ "resources": resources }),
            ))
        }
        "resources/read" => {
            let id = id.unwrap_or(serde_json::Value::Null);
            let params = req.params.unwrap_or(json!({}));
            let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");

            match read_resource(uri) {
                Ok(content) => Some(JsonRpcResponse::success(
                    id,
                    json!({ "contents": [content] }),
                )),
                Err(e) => Some(JsonRpcResponse::error(
                    id,
                    error_codes::INVALID_PARAMS,
                    format!("failed to read resource: {e}"),
                )),
            }
        }
        unknown => id.map(|id| {
            JsonRpcResponse::error(
                id,
                error_codes::METHOD_NOT_FOUND,
                format!("unknown method: '{unknown}'"),
            )
        }),
    }
}
