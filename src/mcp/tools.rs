//! MCP Tool definitions and execution dispatchers.

use serde_json::json;
use std::path::PathBuf;

use crate::ctx::Ctx;
use crate::mcp::protocol::{CallToolResult, ToolDefinition};

/// Return the complete catalog of MCP tools exposed by `qecs`.
pub fn list_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "qecs_run".to_string(),
            description: "Execute a project or command on a fresh ephemeral Huawei Cloud VM. Automatically detects runtimes, installs missing system libraries, restores OBS dependency caches, and retrieves output artifacts.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Local workspace directory path (defaults to current directory '.')"
                    },
                    "preset": {
                        "type": "string",
                        "enum": ["normal", "ram", "compute", "gpu", "beefy"],
                        "description": "Hardware preset to use"
                    },
                    "flavor": {
                        "type": "string",
                        "description": "Explicit Huawei Cloud flavor ID (overrides preset)"
                    },
                    "command": {
                        "type": "string",
                        "description": "Ad-hoc command to run instead of the auto-detected entrypoint"
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Trailing arguments forwarded to the command"
                    },
                    "ttl": {
                        "type": "string",
                        "description": "Self-destruction timeout (e.g. '30m', '1h', '2h'). Default is 2 hours."
                    },
                    "detach": {
                        "type": "boolean",
                        "description": "Launch asynchronously in the background without waiting"
                    },
                    "no_cache": {
                        "type": "boolean",
                        "description": "Bypass regional OBS dependency caching"
                    },
                    "artifacts": {
                        "type": "string",
                        "description": "Comma-separated output files or directories to download (e.g. 'models/*.pt,results.json')"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "qecs_up".to_string(),
            description: "Provision an interactive ephemeral VM and return its IP address and SSH connection credentials.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "preset": {
                        "type": "string",
                        "enum": ["normal", "ram", "compute", "gpu", "beefy"]
                    },
                    "flavor": { "type": "string" },
                    "ttl": { "type": "string", "description": "Self-destruction timeout (e.g. '1h')" },
                    "name": { "type": "string", "description": "Optional custom name for the VM" }
                }
            }),
        },
        ToolDefinition {
            name: "qecs_ls".to_string(),
            description: "List all currently tracked ephemeral VMs, IP addresses, running jobs, uptime, and estimated spend.".to_string(),
            input_schema: json!({ "type": "object" }),
        },
        ToolDefinition {
            name: "qecs_info".to_string(),
            description: "Retrieve comprehensive hardware, networking, flavor, and security details for a specific VM.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Target VM name or unique prefix"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "qecs_logs".to_string(),
            description: "Fetch execution logs (job.log or cloud-init output) from a running or finished VM.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Target VM name"
                    },
                    "cloud_init": {
                        "type": "boolean",
                        "description": "If true, fetch cloud-init bootstrap logs instead of user job logs"
                    }
                },
                "required": ["target"]
            }),
        },
        ToolDefinition {
            name: "qecs_wait".to_string(),
            description: "Block until a detached background job completes on a VM and download output artifacts.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "job": {
                        "type": "string",
                        "description": "VM or job name to wait for"
                    },
                    "output": {
                        "type": "string",
                        "description": "Local destination directory for output artifacts"
                    }
                },
                "required": ["job"]
            }),
        },
        ToolDefinition {
            name: "qecs_kill".to_string(),
            description: "Immediately terminate and destroy a specific VM or all running VMs.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Target VM name to terminate"
                    },
                    "all": {
                        "type": "boolean",
                        "description": "If true, terminate all running VMs"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "qecs_presets".to_string(),
            description: "Return the catalog of compute presets (normal, ram, compute, gpu, beefy), specs (vCPU, RAM, GPU), and pricing.".to_string(),
            input_schema: json!({ "type": "object" }),
        },
        ToolDefinition {
            name: "qecs_cache_clean".to_string(),
            description: "Delete cached dependency archives from the regional OBS bucket to reclaim storage.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "force": { "type": "boolean", "default": true }
                }
            }),
        },
    ]
}

/// Execute an MCP tool by name and return its formatted result.
pub async fn execute_tool(ctx: &Ctx, name: &str, args: &serde_json::Value) -> CallToolResult {
    match name {
        "qecs_presets" => {
            let specs: Vec<_> = crate::presets::Preset::ALL
                .iter()
                .map(|p| crate::presets::resolve(*p, &ctx.config, None))
                .collect();
            match serde_json::to_string_pretty(&specs) {
                Ok(s) => CallToolResult::ok(s),
                Err(e) => CallToolResult::err(format!("failed to serialize presets: {e}")),
            }
        }
        "qecs_ls" => match crate::state::StateStore::open().and_then(|s| s.list()) {
            Ok(vms) => match serde_json::to_string_pretty(&vms) {
                Ok(s) => CallToolResult::ok(s),
                Err(e) => CallToolResult::err(format!("failed to format VMs: {e}")),
            },
            Err(e) => CallToolResult::err(format!("failed to list VMs: {e}")),
        },
        "qecs_info" => {
            let target_name = match args.get("name").and_then(|v| v.as_str()) {
                Some(n) => n.to_string(),
                None => return CallToolResult::err("missing required parameter 'name'"),
            };
            let info_args = crate::cli::InfoArgs { name: target_name };
            match crate::commands::info::cmd_info(ctx, info_args).await {
                Ok(()) => CallToolResult::ok("Retrieved VM information successfully."),
                Err(e) => CallToolResult::err(format!("Error: {e:#}")),
            }
        }
        "qecs_kill" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .map(String::from);
            let all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
            let kill_args = crate::cli::KillArgs { name: target, all };
            match crate::commands::kill::cmd_kill(ctx, kill_args).await {
                Ok(()) => CallToolResult::ok("VM termination command completed."),
                Err(e) => CallToolResult::err(format!("Error terminating VM: {e:#}")),
            }
        }
        "qecs_wait" => {
            let job = match args.get("job").and_then(|v| v.as_str()) {
                Some(j) => j.to_string(),
                None => return CallToolResult::err("missing required parameter 'job'"),
            };
            let wait_args = crate::cli::WaitArgs { job };
            match crate::commands::wait::cmd_wait(ctx, wait_args).await {
                Ok(()) => {
                    CallToolResult::ok("Job completed successfully and artifacts downloaded.")
                }
                Err(e) => CallToolResult::err(format!("Error waiting for job: {e:#}")),
            }
        }
        "qecs_logs" => {
            let target = match args.get("target").and_then(|v| v.as_str()) {
                Some(t) => t.to_string(),
                None => return CallToolResult::err("missing required parameter 'target'"),
            };
            let logs_args = crate::cli::LogsArgs {
                target,
                follow: false,
                cloud_init: false,
            };
            match crate::commands::logs::cmd_logs(ctx, logs_args).await {
                Ok(()) => CallToolResult::ok("Logs retrieved successfully."),
                Err(e) => CallToolResult::err(format!("Error fetching logs: {e:#}")),
            }
        }
        "qecs_cache_clean" => {
            let cache_args = crate::cli::CacheArgs {
                action: crate::cli::CacheAction::Clean { force: true },
            };
            match crate::commands::cache::cmd_cache(ctx, cache_args).await {
                Ok(()) => CallToolResult::ok("Cache cleaned successfully."),
                Err(e) => CallToolResult::err(format!("Error cleaning cache: {e:#}")),
            }
        }
        "qecs_up" => {
            let preset = args
                .get("preset")
                .and_then(|v| v.as_str())
                .and_then(|p| p.parse().ok());
            let ttl = args.get("ttl").and_then(|v| v.as_str()).map(String::from);
            let name = args.get("name").and_then(|v| v.as_str()).map(String::from);
            let up_args = crate::cli::UpArgs {
                preset,
                name,
                ttl,
                dry_run: false,
                no_baked_image: false,
            };
            match crate::commands::up::cmd_up(ctx, up_args).await {
                Ok(()) => CallToolResult::ok("VM provisioned successfully."),
                Err(e) => CallToolResult::err(format!("Error provisioning VM: {e:#}")),
            }
        }
        "qecs_run" => {
            let path = args.get("path").and_then(|v| v.as_str()).map(PathBuf::from);
            let preset = args
                .get("preset")
                .and_then(|v| v.as_str())
                .and_then(|p| p.parse().ok());
            let flavor = args
                .get("flavor")
                .and_then(|v| v.as_str())
                .map(String::from);
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .map(String::from);
            let trailing_args: Vec<String> = args
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let ttl = args.get("ttl").and_then(|v| v.as_str()).map(String::from);
            let detach = args
                .get("detach")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let no_cache = args
                .get("no_cache")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let artifacts = args
                .get("artifacts")
                .and_then(|v| v.as_str())
                .map(String::from);

            let run_args = crate::cli::RunArgs {
                path,
                preset,
                flavor,
                ttl,
                detach,
                keep: false,
                output: None,
                artifacts,
                no_cache,
                command,
                args: trailing_args,
                env: vec![],
                env_file: None,
                forward_env: true,
                keep_on_failure: false,
                pty: false,
                no_pty: true, // headless execution for MCP
                dry_run: false,
                no_baked_image: false,
            };

            match crate::commands::run::cmd_run(ctx, run_args).await {
                Ok(()) => CallToolResult::ok("Job executed successfully."),
                Err(e) => CallToolResult::err(format!("Job execution error: {e:#}")),
            }
        }
        unknown => CallToolResult::err(format!("unknown tool '{unknown}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_tools_contains_all_core_capabilities() {
        let tools = list_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"qecs_run"));
        assert!(names.contains(&"qecs_up"));
        assert!(names.contains(&"qecs_ls"));
        assert!(names.contains(&"qecs_info"));
        assert!(names.contains(&"qecs_logs"));
        assert!(names.contains(&"qecs_wait"));
        assert!(names.contains(&"qecs_kill"));
        assert!(names.contains(&"qecs_presets"));
        assert!(names.contains(&"qecs_cache_clean"));
    }
}
