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

/// Diagnostic report for remote job failures, presented in Cargo/rustc diagnostic style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticReport {
    pub code: &'static str,
    pub title: String,
    pub target: String,
    pub details: Option<String>,
    pub note: Option<String>,
    pub recommendation: Option<String>,
    pub command_suggestion: Option<String>,
}

impl DiagnosticReport {
    pub fn render_cargo_style(&self) {
        use colored::Colorize;
        eprintln!();
        eprintln!(
            "{}: {}",
            format!("error[{}]", self.code).red().bold(),
            self.title.bold()
        );
        eprintln!("  {} {}", "-->".cyan().bold(), self.target);
        if let Some(ref details) = self.details {
            eprintln!("   {}", "|".cyan().bold());
            for line in details.lines() {
                eprintln!("   {}   {}", "|".cyan().bold(), line.dimmed());
            }
            eprintln!("   {}", "|".cyan().bold());
        }
        if let Some(ref note) = self.note {
            eprintln!("   {} {} {}", "=".cyan().bold(), "note:".bold(), note);
        }
        if let Some(ref rec) = self.recommendation {
            eprintln!(
                "   {} {} {}",
                "=".cyan().bold(),
                "help:".green().bold(),
                rec
            );
        }
        if let Some(ref cmd) = self.command_suggestion {
            eprintln!("           {}", cmd.cyan().bold());
        }
        eprintln!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_report_instantiation() {
        let report = DiagnosticReport {
            code: "E137",
            title: "Remote process terminated by Linux Out-Of-Memory (OOM) Killer".into(),
            target: "Remote VM: ecs-test (flavor: s7n.2xlarge.2)".into(),
            details: Some("Out of memory: Killed process 1234 (python3)".into()),
            note: Some("Workload exceeded VM memory".into()),
            recommendation: Some("Re-run with higher memory preset:".into()),
            command_suggestion: Some("qecs run --preset ram".into()),
        };
        assert_eq!(report.code, "E137");
        // Ensure rendering does not panic
        report.render_cargo_style();
    }
}
