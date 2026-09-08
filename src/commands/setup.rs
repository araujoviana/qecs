//! `qecs setup` command: writes default config and validates credentials.
use crate::config::{self, Config};
use crate::ctx::Ctx;
use crate::hwc::iam;
use crate::keys;
use crate::telemetry::TelemetryExt;
use colored::Colorize;

pub async fn cmd_setup(ctx: &Ctx) -> anyhow::Result<()> {
    // 1. Ensure dedicated SSH keypair
    let (paths, _pub_key) = keys::ensure_keypair(None)?;

    // 2. Ensure config file exists
    let cfg_path = config::config_path();
    if !cfg_path.exists() {
        if let Some(parent) = cfg_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let default_toml = toml::to_string_pretty(&Config::default())?;
        std::fs::write(&cfg_path, default_toml)?;
        println!(
            "{}",
            format!("✓ Created config file: {}", cfg_path.display()).green()
        );
    } else {
        println!("✓ Config file found: {}", cfg_path.display());
    }

    // 3. Validate credentials by testing IAM connectivity
    let region = ctx.region();
    let client = ctx.signed();
    let pb = crate::ui::spinner(format!("Validating credentials in region `{region}`..."));
    let project = ctx
        .telemetry
        .phase_try("iam-project", iam::discover_project(&client, &region))
        .await?;
    pb.finish_and_clear();

    println!(
        "{}",
        format!("✓ Successfully authenticated with Huawei Cloud ({region})!")
            .green()
            .bold()
    );
    println!("  Project ID: {}", project.id);
    println!("  Domain ID:  {}", project.domain_id);
    println!("  SSH Key:    {}", paths.private_key.display());
    println!();
    println!(
        "qecs is ready to use. Try running: {}",
        "qecs up --dry-run".cyan()
    );

    Ok(())
}
