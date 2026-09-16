//! `qecs logs` command: tail or stream cloud-init logs from a VM.
use crate::cli::LogsArgs;
use crate::commands::shell::resolve_target_vm;
use crate::connect;
use crate::ctx::Ctx;
use crate::keys;

pub async fn cmd_logs(ctx: &Ctx, args: LogsArgs) -> anyhow::Result<()> {
    let (paths, _pub_key) = keys::ensure_keypair(None)?;
    let (store, mut vm) = resolve_target_vm(ctx, Some(&args.target)).await?;

    let ip = vm
        .eip
        .clone()
        .or_else(|| vm.private_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("VM `{}` has no IP address assigned", vm.name))?;

    let relay = connect::Relay::from_config(ctx.config.relay.as_ref())?;
    let port =
        connect::resolve_connection_port_with_relay(&ip, vm.connect_port, Some(&relay)).await?;
    if vm.connect_port != Some(port) {
        vm.connect_port = Some(port);
        let _ = store.upsert(vm);
    }

    let remote_cmd = if args.cloud_init {
        if args.follow {
            "tail -n +1 -f /var/log/cloud-init-output.log"
        } else {
            "cat /var/log/cloud-init-output.log"
        }
    } else if args.follow {
        "if [ -f /home/ubuntu/job.log ]; then tail -n +1 -f /home/ubuntu/job.log; else tail -n +1 -f /var/log/cloud-init-output.log; fi"
    } else {
        "if [ -f /home/ubuntu/job.log ]; then cat /home/ubuntu/job.log; else cat /var/log/cloud-init-output.log; fi"
    };

    let proxy_cmd = relay.proxy_command(&ip, port);

    let status = connect::exec_remote_command(
        &ip,
        port,
        &paths.private_key,
        proxy_cmd.as_deref(),
        remote_cmd,
    )?;
    if !status.success()
        && let Some(code) = status.code()
    {
        return Err(crate::error::ExitCode(code).into());
    }
    Ok(())
}
