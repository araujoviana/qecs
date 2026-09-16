//! Command-line surface. Deliberately small: `run` and `up` are the 90% path.
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

const BANNER: &str = r#"
                      
  _` |  _ \  __|  __| 
 (   |  __/ (   \__ \ 
\__, |\___|\___|____/ 
    _|                
"#;

#[derive(Parser, Debug)]
#[command(
    name = "qecs",
    version,
    about = "Ephemeral Huawei Cloud compute.",
    before_help = BANNER
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Args, Debug, Clone, Default)]
pub struct GlobalArgs {
    /// Credential/config profile name (selects .env.<profile>).
    #[arg(short = 'P', long, global = true)]
    pub profile: Option<String>,
    #[arg(short, long, global = true)]
    pub verbose: bool,
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Machine-readable output where supported.
    #[arg(short, long, global = true)]
    pub json: bool,
    #[arg(long, global = true, hide = true)]
    pub ak: Option<String>,
    #[arg(long, global = true, hide = true)]
    pub sk: Option<String>,
    /// Override the configured region.
    #[arg(short, long, global = true)]
    pub region: Option<String>,
    /// Record telemetry trace to ~/.local/state/qecs/traces/.
    #[arg(short = 'T', long, global = true)]
    pub telemetry: bool,
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
    /// Delete a VM (or all of them) (alias for kill).
    Down(KillArgs),
    /// Reconcile local state with the cloud; remove orphans.
    Gc,
    /// Write config and validate credentials.
    Setup,
    /// Show presets and their resolved flavors.
    Presets,
    /// Manage and build pre-baked IMS images for accelerated cold starts.
    Image(ImageArgs),
    /// Manage the regional OBS dependency and build cache.
    Cache(CacheArgs),
    /// Attach to an interactive session on a running VM.
    Attach(AttachArgs),
    /// Model Context Protocol (MCP) server for AI assistants.
    Mcp(McpArgs),
    /// Generate shell completion script (bash, zsh, fish, powershell, elvish).
    Completion(CompletionArgs),
}

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    pub path: Option<PathBuf>,
    #[arg(short, long, value_enum)]
    pub preset: Option<crate::presets::Preset>,
    #[arg(short, long)]
    pub flavor: Option<String>,
    #[arg(short, long)]
    pub ttl: Option<String>,
    #[arg(short, long)]
    pub detach: bool,
    #[arg(short, long)]
    pub keep: bool,
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Artifact file(s), directory, or glob pattern to retrieve (e.g. "out", "results.json", "models/*.pt").
    #[arg(short = 'a', long = "artifacts")]
    pub artifacts: Option<String>,
    #[arg(short = 'D', long)]
    pub dry_run: bool,
    /// Bypass pre-baked private images and use a fresh base gold image.
    #[arg(long)]
    pub no_baked_image: bool,
    /// Inline ad-hoc command to execute instead of inferring from detector.
    #[arg(short = 'c', long)]
    pub command: Option<String>,
    /// Keep VM alive only if the job exits with non-zero status.
    #[arg(long)]
    pub keep_on_failure: bool,
    /// Environment variables to inject (e.g. -e KEY=VAL or -e KEY to forward host variable).
    #[arg(short = 'e', long = "env", value_name = "KEY[=VAL]")]
    pub env: Vec<String>,
    /// Load environment variables from a .env file.
    #[arg(long = "env-file", value_name = "PATH")]
    pub env_file: Option<PathBuf>,
    /// Auto-forward well-known AI tokens (HF_TOKEN, WANDB_API_KEY, OPENAI_API_KEY, etc.).
    #[arg(long, default_value = "true", action = clap::ArgAction::Set)]
    pub forward_env: bool,
    /// Force pseudo-terminal (PTY) allocation.
    #[arg(long, conflicts_with = "no_pty")]
    pub pty: bool,
    /// Disable pseudo-terminal (PTY) allocation.
    #[arg(long, conflicts_with = "pty")]
    pub no_pty: bool,
    /// Disable regional OBS dependency and build caching for this run.
    #[arg(long)]
    pub no_cache: bool,
    /// Trailing arguments passed directly to the run command.
    #[arg(last = true)]
    pub args: Vec<String>,
}

#[derive(Args, Debug, Clone)]
pub struct UpArgs {
    #[arg(short, long, value_enum)]
    pub preset: Option<crate::presets::Preset>,
    #[arg(short, long)]
    pub name: Option<String>,
    #[arg(short, long)]
    pub ttl: Option<String>,
    #[arg(short = 'D', long)]
    pub dry_run: bool,
    /// Bypass pre-baked private images and use a fresh base gold image.
    #[arg(long)]
    pub no_baked_image: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ShellArgs {
    pub name: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct AttachArgs {
    /// Target VM name or prefix (optional if only one VM is active).
    pub target: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct InfoArgs {
    pub name: String,
}

#[derive(Args, Debug, Clone)]
pub struct LogsArgs {
    pub target: String,
    #[arg(short, long)]
    pub follow: bool,
    #[arg(long, help = "View cloud-init bootstrap logs instead of the job log")]
    pub cloud_init: bool,
}

#[derive(Args, Debug, Clone)]
pub struct WaitArgs {
    pub job: String,
}

#[derive(Args, Debug, Clone)]
pub struct KillArgs {
    pub name: Option<String>,
    #[arg(short, long, conflicts_with = "name")]
    pub all: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ImageArgs {
    #[command(subcommand)]
    pub action: ImageAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ImageAction {
    /// List active private images.
    Ls,
    /// Build a pre-baked GPU image from an ephemeral instance.
    Build(ImageBuildArgs),
    /// Delete a private image by ID.
    Delete(ImageDeleteArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ImageBuildArgs {
    /// Name for the new baked image (defaults to qecs-gpu-YYYYMMDD-HHMM).
    #[arg(short, long)]
    pub name: Option<String>,
    /// Description for the baked image.
    #[arg(long)]
    pub description: Option<String>,
    /// Keep the builder VM alive after image creation.
    #[arg(short, long)]
    pub keep: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ImageDeleteArgs {
    /// Image ID to delete.
    pub id: String,
}

#[derive(Args, Debug, Clone)]
pub struct CompletionArgs {
    /// Target shell to generate completions for.
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

#[derive(Args, Debug, Clone)]
pub struct CacheArgs {
    #[command(subcommand)]
    pub action: CacheAction,
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum CacheAction {
    /// List all cached objects and their sizes in the regional OBS bucket.
    Ls,
    /// Delete all cached dependency and build artifacts from the regional OBS bucket.
    Clean {
        /// Force deletion without confirmation.
        #[arg(short, long)]
        force: bool,
    },
    /// Completely destroy the regional OBS cache bucket and all its contents.
    Destroy {
        /// Force destruction without confirmation.
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Args, Debug, Clone)]
pub struct McpArgs {
    #[command(subcommand)]
    pub action: McpAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpAction {
    /// Start the MCP JSON-RPC 2.0 stdio server.
    Serve,
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
                assert_eq!(a.preset, Some(crate::presets::Preset::Gpu));
                assert!(!a.no_baked_image);
            }
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn parses_run_with_no_baked_image() {
        let cli = Cli::try_parse_from(["qecs", "run", "--no-baked-image"]).unwrap();
        match cli.command {
            Commands::Run(a) => assert!(a.no_baked_image),
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn parses_image_subcommands() {
        let cli = Cli::try_parse_from(["qecs", "image", "ls"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Image(ImageArgs {
                action: ImageAction::Ls
            })
        ));

        let cli = Cli::try_parse_from(["qecs", "image", "build", "--name", "my-image", "--keep"])
            .unwrap();
        match cli.command {
            Commands::Image(ImageArgs {
                action: ImageAction::Build(b),
            }) => {
                assert_eq!(b.name.as_deref(), Some("my-image"));
                assert!(b.keep);
            }
            _ => panic!("wrong command"),
        }

        let cli = Cli::try_parse_from(["qecs", "image", "delete", "img-123"]).unwrap();
        match cli.command {
            Commands::Image(ImageArgs {
                action: ImageAction::Delete(d),
            }) => {
                assert_eq!(d.id, "img-123");
            }
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn kill_args_parse_none_name_or_all() {
        let cli = Cli::try_parse_from(["qecs", "kill", "--all"]).unwrap();
        match cli.command {
            Commands::Kill(a) => assert!(a.all && a.name.is_none()),
            _ => panic!("wrong command"),
        }
        let cli = Cli::try_parse_from(["qecs", "kill"]).unwrap();
        match cli.command {
            Commands::Kill(a) => assert!(!a.all && a.name.is_none()),
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn down_alias_resolves_to_kill() {
        let cli = Cli::try_parse_from(["qecs", "down", "--all"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Down(KillArgs {
                all: true,
                name: None
            }) | Commands::Kill(KillArgs {
                all: true,
                name: None
            })
        ));
    }

    #[test]
    fn completion_subcommand_parses_shell() {
        let cli = Cli::try_parse_from(["qecs", "completion", "fish"]).unwrap();
        if let Commands::Completion(args) = cli.command {
            assert_eq!(args.shell, clap_complete::Shell::Fish);
        } else {
            panic!("expected Completion command");
        }
    }

    #[test]
    fn global_json_flag_is_parsed_after_subcommand() {
        let cli = Cli::try_parse_from(["qecs", "ls", "--json"]).unwrap();
        assert!(cli.global.json);
        assert!(matches!(cli.command, Commands::Ls));
    }

    #[test]
    fn bundled_flags_and_short_options_work() {
        // Global bundled flags: -v (verbose) + -j (json)
        // Subcommand short flags: -d (detach) + -k (keep) + -p (preset)
        let cli = Cli::try_parse_from(["qecs", "-vj", "run", "-dk", "-p", "gpu", "myjob"]).unwrap();
        assert!(cli.global.verbose);
        assert!(cli.global.json);
        match cli.command {
            Commands::Run(a) => {
                assert!(a.detach);
                assert!(a.keep);
                assert_eq!(a.preset, Some(crate::presets::Preset::Gpu));
                assert_eq!(a.path.unwrap().to_str().unwrap(), "myjob");
            }
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn down_alias_with_short_all_flag() {
        let cli = Cli::try_parse_from(["qecs", "down", "-a"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Down(KillArgs {
                all: true,
                name: None
            }) | Commands::Kill(KillArgs {
                all: true,
                name: None
            })
        ));
    }

    #[test]
    fn parses_run_with_command_and_env() {
        let cli = Cli::try_parse_from([
            "qecs",
            "run",
            "-c",
            "pytest -v",
            "-e",
            "FOO=bar",
            "-e",
            "LOCAL_VAR",
            "--env-file",
            ".env.test",
            "--keep-on-failure",
        ])
        .unwrap();
        match cli.command {
            Commands::Run(a) => {
                assert_eq!(a.command.as_deref(), Some("pytest -v"));
                assert_eq!(a.env, vec!["FOO=bar", "LOCAL_VAR"]);
                assert_eq!(a.env_file, Some(PathBuf::from(".env.test")));
                assert!(a.keep_on_failure);
                assert!(a.forward_env);
                assert!(!a.pty);
                assert!(!a.no_pty);
            }
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn parses_run_with_trailing_args() {
        let cli = Cli::try_parse_from([
            "qecs",
            "run",
            "train.py",
            "--",
            "--epochs",
            "50",
            "--batch-size",
            "32",
        ])
        .unwrap();
        match cli.command {
            Commands::Run(a) => {
                assert_eq!(a.path.unwrap().to_str().unwrap(), "train.py");
                assert_eq!(a.args, vec!["--epochs", "50", "--batch-size", "32"]);
            }
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn parses_run_with_pty_flags() {
        let cli = Cli::try_parse_from(["qecs", "run", "--pty"]).unwrap();
        match cli.command {
            Commands::Run(a) => {
                assert!(a.pty);
                assert!(!a.no_pty);
            }
            _ => panic!("wrong command"),
        }

        let cli = Cli::try_parse_from(["qecs", "run", "--no-pty"]).unwrap();
        match cli.command {
            Commands::Run(a) => {
                assert!(!a.pty);
                assert!(a.no_pty);
            }
            _ => panic!("wrong command"),
        }

        assert!(Cli::try_parse_from(["qecs", "run", "--pty", "--no-pty"]).is_err());
    }

    #[test]
    fn parses_run_with_artifacts_flag() {
        let cli = Cli::try_parse_from(["qecs", "run", "-a", "models/*.pt,results.json"]).unwrap();
        match cli.command {
            Commands::Run(a) => {
                assert_eq!(a.artifacts.as_deref(), Some("models/*.pt,results.json"));
            }
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn parses_run_with_no_cache() {
        let cli = Cli::try_parse_from(["qecs", "run", "--no-cache"]).unwrap();
        match cli.command {
            Commands::Run(a) => {
                assert!(a.no_cache);
            }
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn parses_cache_subcommands() {
        let cli = Cli::try_parse_from(["qecs", "cache", "ls"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Cache(CacheArgs {
                action: CacheAction::Ls
            })
        ));

        let cli = Cli::try_parse_from(["qecs", "cache", "clean", "--force"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Cache(CacheArgs {
                action: CacheAction::Clean { force: true }
            })
        ));

        let cli = Cli::try_parse_from(["qecs", "cache", "destroy"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Cache(CacheArgs {
                action: CacheAction::Destroy { force: false }
            })
        ));
    }

    #[test]
    fn verify_cli() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
