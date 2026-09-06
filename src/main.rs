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
            let cfg = config::load_config(None)?;
            presets::cmd_presets(&cfg, cli.global.json).await
        }
        Commands::Setup => todo_stub("setup"),
        Commands::Run(_) => todo_stub("run"),
        Commands::Up(_) => todo_stub("up"),
        Commands::Shell(_) => todo_stub("shell"),
        Commands::Ls => todo_stub("ls"),
        Commands::Info(_) => todo_stub("info"),
        Commands::Logs(_) => todo_stub("logs"),
        Commands::Wait(_) => todo_stub("wait"),
        Commands::Kill(_) => todo_stub("kill"),
        Commands::Gc => todo_stub("gc"),
    }
}

fn todo_stub(name: &str) -> anyhow::Result<()> {
    anyhow::bail!("`qecs {name}` is not implemented yet")
}
