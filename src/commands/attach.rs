//! `qecs attach` command: reattaches to an interactive `tmux` session on a running VM.

use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use colored::Colorize;

use crate::cli::AttachArgs;
use crate::commands::shell::resolve_target_vm;
use crate::ctx::Ctx;
use crate::keys;
use crate::run::session::{DEFAULT_SESSION_NAME, build_smart_attach_cmd};

pub async fn cmd_attach(ctx: &Ctx, args: AttachArgs) -> anyhow::Result<()> {
    let (_store, vm) = resolve_target_vm(ctx, args.target.as_deref()).await?;

    let ip = vm
        .eip
        .clone()
        .or(vm.private_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("VM `{}` has no IP address", vm.name))?;

    let (paths, _pub_key) = keys::ensure_keypair(None)?;

    let relay = crate::connect::Relay::from_config(ctx.config.relay.as_ref())?;
    let port = if let Some(p) = vm.connect_port {
        p
    } else {
        crate::connect::wait_for_ssh_ready(&ip, Duration::from_secs(30), &relay).await?
    };

    let proxy_cmd = relay.proxy_command(&ip, port);

    println!(
        "{}",
        format!(
            "Attaching to interactive session on `{}` ({ip})...",
            vm.name
        )
        .cyan()
        .bold()
    );

    let attach_cmd = build_smart_attach_cmd(DEFAULT_SESSION_NAME);

    let status = crate::connect::build_ssh_command_ext(
        &ip,
        port,
        &paths.private_key,
        proxy_cmd.as_deref(),
        Some(true),
    )
    .arg(&attach_cmd)
    .stdin(Stdio::inherit())
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit())
    .status()
    .context("attaching to remote session over SSH")?;

    let code = status.code().unwrap_or(0);
    if code != 0 {
        return Err(anyhow::anyhow!(crate::error::ExitCode(code)));
    }

    Ok(())
}
