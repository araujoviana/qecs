//! Command-line surface. Deliberately small: `run` and `up` are the 90% path.
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "qecs", version, about = "Ephemeral Huawei Cloud compute.")]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Args, Debug, Clone)]
pub struct GlobalArgs {
    /// Credential/config profile name (selects .env.<profile>).
    #[arg(long, global = true)]
    pub profile: Option<String>,
    #[arg(long, global = true)]
    pub verbose: bool,
    #[arg(long, global = true)]
    pub quiet: bool,
    /// Machine-readable output where supported.
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, global = true, hide = true)]
    pub ak: Option<String>,
    #[arg(long, global = true, hide = true)]
    pub sk: Option<String>,
    /// Override the configured region.
    #[arg(long, global = true)]
    pub region: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Ship the working directory to a fresh VM, run it, bring artifacts back.
    Run(RunArgs),
    /// Bring up an interactive ephemeral VM.
    Up(UpArgs),
    /// Open a shell on a VM.
    Shell(ShellArgs),
    /// List tracked VMs.
    Ls,
    /// Show everything about one VM.
    Info(InfoArgs),
    /// Tail cloud-init or job logs.
    Logs(LogsArgs),
    /// Block until a detached job finishes.
    Wait(WaitArgs),
    /// Delete a VM (or all of them).
    Kill(KillArgs),
    /// Reconcile local state with the cloud; remove orphans.
    Gc,
    /// Write config and validate credentials.
    Setup,
    /// Show presets and their resolved flavors.
    Presets,
}

#[derive(Args, Debug)]
pub struct RunArgs {
    pub path: Option<PathBuf>,
    #[arg(long)]
    pub preset: Option<String>,
    #[arg(long)]
    pub flavor: Option<String>,
    #[arg(long)]
    pub ttl: Option<String>,
    #[arg(long)]
    pub detach: bool,
    #[arg(long)]
    pub keep: bool,
    #[arg(long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct UpArgs {
    #[arg(long)]
    pub preset: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub ttl: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct ShellArgs {
    pub name: Option<String>,
}

#[derive(Args, Debug)]
pub struct InfoArgs {
    pub name: String,
}

#[derive(Args, Debug)]
pub struct LogsArgs {
    pub target: String,
    #[arg(long)]
    pub follow: bool,
}

#[derive(Args, Debug)]
pub struct WaitArgs {
    pub job: String,
}

#[derive(Args, Debug)]
pub struct KillArgs {
    #[arg(required_unless_present = "all")]
    pub name: Option<String>,
    #[arg(long, conflicts_with = "name")]
    pub all: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_run_with_path_and_preset() {
        let cli = Cli::try_parse_from(["qecs", "run", "job.py", "--preset", "gpu"]).unwrap();
        match cli.command {
            Commands::Run(a) => {
                assert_eq!(a.path.unwrap().to_str().unwrap(), "job.py");
                assert_eq!(a.preset.as_deref(), Some("gpu"));
            }
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn kill_requires_name_or_all() {
        let cli = Cli::try_parse_from(["qecs", "kill", "--all"]).unwrap();
        match cli.command {
            Commands::Kill(a) => assert!(a.all && a.name.is_none()),
            _ => panic!("wrong command"),
        }
        assert!(Cli::try_parse_from(["qecs", "kill"]).is_err());
    }

    #[test]
    fn global_json_flag_is_parsed_after_subcommand() {
        let cli = Cli::try_parse_from(["qecs", "ls", "--json"]).unwrap();
        assert!(cli.global.json);
        assert!(matches!(cli.command, Commands::Ls));
    }

    #[test]
    fn verify_cli() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
