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

    let code = rt.block_on(run(cli));
    std::process::ExitCode::from(code as u8)
}

async fn run(cli: Cli) -> i32 {
    let config_res = config::load_config(None);
    let config = config_res.as_ref().ok();
    let config_telemetry = config.map(|c| c.telemetry.enabled).unwrap_or(false);
    let enabled = qecs::telemetry::resolve_enabled(
        cli.global.telemetry,
        std::env::var("QECS_TELEMETRY").ok().as_deref(),
        config_telemetry,
    );
    let tel =
        qecs::telemetry::Telemetry::init(enabled, qecs::telemetry::subcommand_label(&cli.command));
    let trace_path = tel.as_ref().map(|t| t.trace_path().to_path_buf());

    let result = match config_res {
        Ok(ref cfg) => run_command(&cli, cfg, tel.clone()).await,
        Err(e) => Err(e),
    };

    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            if let Some(exit_code) = e.downcast_ref::<error::ExitCode>() {
                exit_code.0
            } else {
                error::log_error_chain(&e);
                1
            }
        }
    };

    if let Some(t) = tel {
        let count = t.finish(code);
        if cli.global.verbose
            && let Some(ref path) = trace_path
        {
            eprintln!("telemetry: {count} events -> {}", path.display());
        }
    }

    code
}

async fn run_command(
    cli: &Cli,
    cfg: &config::Config,
    tel: Option<qecs::telemetry::Telemetry>,
) -> anyhow::Result<()> {
    match cli.command {
        Commands::Presets => {
            let mut cfg = cfg.clone();
            if let Some(region) = &cli.global.region {
                cfg.region = region.clone();
            }
            presets::cmd_presets(&cfg, cli.global.json).await
        }
        Commands::Setup => {
            let ctx = qecs::ctx::Ctx::load(cli, cfg.clone(), tel)?;
            qecs::commands::setup::cmd_setup(&ctx).await
        }
        Commands::Up(ref args) => {
            let ctx = qecs::ctx::Ctx::load(cli, cfg.clone(), tel)?;
            qecs::commands::up::cmd_up(&ctx, args.clone()).await
        }
        Commands::Ls => qecs::commands::ls::cmd_ls(cli.global.json).await,
        Commands::Info(ref args) => {
            let ctx = qecs::ctx::Ctx::load(cli, cfg.clone(), tel)?;
            qecs::commands::info::cmd_info(&ctx, args.clone()).await
        }
        Commands::Kill(ref args) => {
            let ctx = qecs::ctx::Ctx::load(cli, cfg.clone(), tel)?;
            qecs::commands::kill::cmd_kill(&ctx, args.clone()).await
        }
        Commands::Gc => {
            let ctx = qecs::ctx::Ctx::load(cli, cfg.clone(), tel)?;
            qecs::commands::gc::cmd_gc(&ctx, cli.global.json).await
        }
        Commands::Run(ref args) => {
            let ctx = qecs::ctx::Ctx::load(cli, cfg.clone(), tel)?;
            qecs::commands::run::cmd_run(&ctx, args.clone()).await
        }
        Commands::Shell(ref args) => {
            let ctx = qecs::ctx::Ctx::load(cli, cfg.clone(), tel)?;
            qecs::commands::shell::cmd_shell(&ctx, args.clone()).await
        }
        Commands::Logs(ref args) => {
            let ctx = qecs::ctx::Ctx::load(cli, cfg.clone(), tel)?;
            qecs::commands::logs::cmd_logs(&ctx, args.clone()).await
        }
        Commands::Wait(ref args) => {
            let ctx = qecs::ctx::Ctx::load(cli, cfg.clone(), tel)?;
            qecs::commands::wait::cmd_wait(&ctx, args.clone()).await
        }
        Commands::Image(ref args) => {
            let ctx = qecs::ctx::Ctx::load(cli, cfg.clone(), tel)?;
            qecs::commands::image::cmd_image(&ctx, args.clone(), cli.global.json).await
        }
        Commands::Completion(ref args) => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(args.shell, &mut cmd, "qecs", &mut std::io::stdout());
            Ok(())
        }
    }
}
