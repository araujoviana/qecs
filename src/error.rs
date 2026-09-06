//! Error type and human-readable error-chain printing.
pub type Result<T> = anyhow::Result<T>;

pub fn log_error_chain(err: &anyhow::Error) {
    use colored::Colorize;
    eprintln!("{} {}", "error:".red().bold(), err);
    for cause in err.chain().skip(1) {
        eprintln!("  {} {}", "caused by:".dimmed(), cause);
    }
}

/// Typed error wrapper indicating a desired process exit code, allowing clean unwinding
/// and telemetry completion instead of calling `std::process::exit` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitCode(pub i32);

impl std::fmt::Display for ExitCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "process exit code {}", self.0)
    }
}

impl std::error::Error for ExitCode {}
