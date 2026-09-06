//! `qecs up` command: brings up an interactive ephemeral VM.
use crate::cli::UpArgs;
use crate::ctx::Ctx;
use crate::provision::{self, ProvisionOptions};
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
        Ok(Some(vm)) => {
            pb.finish_and_clear();
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
