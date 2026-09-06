//! `qecs info` command: displays full details of one VM.
use crate::cli::InfoArgs;
use crate::ctx::Ctx;
use crate::hwc::ecs;
use crate::hwc::iam;
use crate::state::StateStore;
use colored::Colorize;

pub async fn cmd_info(ctx: &Ctx, args: InfoArgs) -> anyhow::Result<()> {
    let store = StateStore::open()?;
    let record = store.get(&args.name)?;

    let region = ctx.region();
    let client = ctx.signed();
    let project = iam::discover_project(&client, &region).await?;

    let server_id = match &record {
        Some(r) => r.id.clone(),
        None => {
            let servers = ecs::list_servers(&client, &region, &project.id).await?;
            servers
                .into_iter()
                .find(|s| s.name == args.name)
                .map(|s| s.id)
                .ok_or_else(|| anyhow::anyhow!("VM `{}` not found in state or cloud", args.name))?
        }
    };

    let server = ecs::get_server(&client, &region, &project.id, &server_id).await?;
    let console_url = ecs::remote_console(&client, &region, &project.id, &server_id)
        .await
        .ok();

    if ctx.global.json {
        let mut v = serde_json::to_value(&server)?;
        if let Some(url) = console_url {
            v["vnc_console_url"] = serde_json::json!(url);
        }
        if let Some(r) = record {
            v["state_record"] = serde_json::to_value(r)?;
        }
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("{}", format!("VM `{}` Details", server.name).bold());
        println!("  ID:          {}", server.id);
        println!("  Status:      {}", server.status);
        println!("  Flavor:      {}", server.flavor);
        println!("  Region / AZ: {} / {}", region, server.az);
        if let Some(pub_ip) = &server.public_ip {
            println!("  Public IP:   {}", pub_ip.cyan());
            println!(
                "  SSH Command: ssh -i ~/.config/qecs/keys/id_qecs ubuntu@{}",
                pub_ip
            );
        }
        if let Some(priv_ip) = &server.private_ip {
            println!("  Private IP:  {}", priv_ip);
        }
        if let Some(url) = &console_url {
            println!("  VNC Console: {}", url);
        }
        if let Some(r) = &record {
            println!("  Created:     {}", r.created_at);
            println!("  TTL:         {}s", r.ttl_secs);
        }
    }
    Ok(())
}
