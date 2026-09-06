//! Error type and human-readable error-chain printing.
pub type Result<T> = anyhow::Result<T>;

pub fn log_error_chain(err: &anyhow::Error) {
    use colored::Colorize;
    eprintln!("{} {}", "error:".red().bold(), err);
    for cause in err.chain().skip(1) {
        eprintln!("  {} {}", "caused by:".dimmed(), cause);
    }
}
