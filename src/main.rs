use clap::Parser;
use qecs::cli::{Cli, Commands};
use qecs::{config, error, presets, ui};

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    ui::init_logging(cli.global.verbose, cli.global.quiet);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    match rt.block_on(run(cli)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            error::log_error_chain(&e);
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Presets => {
            let mut cfg = config::load_config(None)?;
            if let Some(region) = &cli.global.region {
                cfg.region = region.clone();
            }
            presets::cmd_presets(&cfg, cli.global.json).await
        }
        Commands::Setup => {
            let ctx = qecs::ctx::Ctx::load(&cli)?;
            qecs::commands::setup::cmd_setup(&ctx).await
        }
        Commands::Up(ref args) => {
            let ctx = qecs::ctx::Ctx::load(&cli)?;
            qecs::commands::up::cmd_up(&ctx, args.clone()).await
        }
        Commands::Ls => qecs::commands::ls::cmd_ls(cli.global.json).await,
        Commands::Info(ref args) => {
            let ctx = qecs::ctx::Ctx::load(&cli)?;
            qecs::commands::info::cmd_info(&ctx, args.clone()).await
        }
        Commands::Kill(ref args) => {
            let ctx = qecs::ctx::Ctx::load(&cli)?;
            qecs::commands::kill::cmd_kill(&ctx, args.clone()).await
        }
        Commands::Gc => {
            let ctx = qecs::ctx::Ctx::load(&cli)?;
            qecs::commands::gc::cmd_gc(&ctx, cli.global.json).await
        }
        Commands::Run(_) => todo_stub("run"),
        Commands::Shell(ref args) => {
            let ctx = qecs::ctx::Ctx::load(&cli)?;
            qecs::commands::shell::cmd_shell(&ctx, args.clone()).await
        }
        Commands::Logs(ref args) => {
            let ctx = qecs::ctx::Ctx::load(&cli)?;
            qecs::commands::logs::cmd_logs(&ctx, args.clone()).await
        }
        Commands::Wait(_) => todo_stub("wait"),
    }
}

fn todo_stub(name: &str) -> anyhow::Result<()> {
    anyhow::bail!("`qecs {name}` is not implemented yet")
}
