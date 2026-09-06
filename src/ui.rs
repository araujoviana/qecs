//! Terminal output: logging init, spinners, tables.
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub fn level_for(verbose: bool, quiet: bool) -> log::LevelFilter {
    match (verbose, quiet) {
        (true, _) => log::LevelFilter::Debug,
        (_, true) => log::LevelFilter::Error,
        _ => log::LevelFilter::Info,
    }
}

pub fn init_logging(verbose: bool, quiet: bool) {
    let mut b = colog::default_builder();
    b.filter_level(level_for(verbose, quiet));
    b.init();
}

pub fn spinner(msg: impl Into<String>) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    pb.enable_steady_tick(Duration::from_millis(90));
    pb.set_message(msg.into());
    pb
}

pub fn print_table<T: tabled::Tabled>(rows: Vec<T>) {
    use tabled::settings::Style;
    let mut t = tabled::Table::new(rows);
    t.with(Style::rounded());
    println!("{t}");
}

#[cfg(test)]
mod tests {
    #[test]
    fn level_mapping() {
        assert_eq!(super::level_for(true, false), log::LevelFilter::Debug);
        assert_eq!(super::level_for(false, true), log::LevelFilter::Error);
        assert_eq!(super::level_for(false, false), log::LevelFilter::Info);
    }
}
