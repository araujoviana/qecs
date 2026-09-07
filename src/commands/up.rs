//! `qecs up` command: brings up an interactive ephemeral VM.
use crate::cli::UpArgs;
use crate::ctx::Ctx;
use crate::provision::{self, ProvisionOptions};
use crate::telemetry::TelemetryExt;
use colored::Colorize;

pub async fn cmd_up(ctx: &Ctx, args: UpArgs) -> anyhow::Result<()> {
    let opts = ProvisionOptions {
        preset: args.preset,
        flavor: None,
        name: args.name,
        ttl: args.ttl,
        dry_run: args.dry_run,
        no_baked_image: args.no_baked_image,
    };

    let pb = crate::ui::spinner("Provisioning VM...");
    match provision::provision_vm(ctx, &opts).await {
        Ok(Some(mut vm)) => {
            pb.finish_and_clear();
            let ip = vm.eip.clone().or_else(|| vm.private_ip.clone());
            if let Some(ip) = &ip {
                let _ = crate::keys::remove_known_host(ip);

                let pb_ssh = crate::ui::spinner(format!(
                    "Waiting for SSH readiness on `{}` ({ip})...",
                    vm.name
                ));
                let relay = crate::connect::Relay::from_config(ctx.config.relay.as_ref())?;
                let port = {
                    let _p = ctx.telemetry.phase("ssh-probe");
                    crate::connect::wait_for_ssh_ready(
                        ip,
                        std::time::Duration::from_secs(90),
                        &relay,
                    )
                    .await
                };
                pb_ssh.finish_and_clear();

                if let Ok(port) = port {
                    vm.connect_port = Some(port);
                    if let Ok(store) = crate::state::StateStore::open() {
                        let _ = store.upsert(vm.clone());
                    }
                }
            }

            if ctx.global.json {
                println!("{}", serde_json::to_string_pretty(&vm)?);
            } else {
                println!("{}", format!("✓ VM `{}` is up!", vm.name).green().bold());
                println!("  ID:        {}", vm.id);
                println!("  Flavor:    {}", vm.flavor);
                println!("  Region/AZ: {} / {}", vm.region, vm.az);
                if let Some(eip) = &vm.eip {
                    println!("  Public IP: {}", eip.cyan().bold());
                    println!(
                        "  Connect:   ssh -i ~/.config/qecs/keys/id_qecs ubuntu@{}",
                        eip
                    );
                } else if let Some(ip) = &vm.private_ip {
                    println!("  Private IP: {}", ip);
                }
            }
            Ok(())
        }
        Ok(None) => {
            pb.finish_and_clear();
            if ctx.global.json {
                println!(r#"{{"dry_run": true, "valid": true}}"#);
            } else {
                println!(
                    "{}",
                    "✓ Dry run validation succeeded (no VM created)."
                        .green()
                        .bold()
                );
            }
            Ok(())
        }
        Err(e) => {
            pb.finish_and_clear();
            Err(e)
        }
    }
}
