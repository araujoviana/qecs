//! `qecs mcp` command: Model Context Protocol server.

use crate::cli::{McpAction, McpArgs};
use crate::ctx::Ctx;

pub async fn cmd_mcp(ctx: &Ctx, args: McpArgs) -> anyhow::Result<()> {
    match args.action {
        McpAction::Serve => crate::mcp::run_stdio(ctx).await,
    }
}
