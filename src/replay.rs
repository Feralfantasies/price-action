//! Replay of recorded bars through the engine — how a user verifies how the
//! configured strategy behaves against genuine historic data before trusting
//! it anywhere.
//!
//! Replay is read-only by design: it always uses a [`PaperBroker`] regardless
//! of the configured `mode`, reports every position change bar by bar, and
//! never performs real execution.

use crate::{
    config::Config,
    csv,
    engine::Engine,
    error::Error,
    execution::PaperBroker,
    market::Bar,
    strategy::{ConsecutiveCloses, Signal},
};

/// Output of one replay: the per-bar trace plus a short summary.
#[derive(Debug, Clone)]
pub struct ReplayReport {
    /// Number of bars fed to the engine.
    pub bars: usize,
    /// Number of separate entries (transitions from flat into Long/Short).
    pub entries: usize,
    /// Final signal after all bars were processed.
    pub final_signal: Signal,
    /// One human-readable line per bar.
    pub trace_lines: Vec<String>,
}

/// Replays the bar file at `path` through the engine configured by `config`.
///
/// # Errors
///
/// [`Error::MarketData`] when the bar file cannot be read or parsed.
pub fn run(config: &Config, path: impl AsRef<std::path::Path>) -> Result<ReplayReport, Error> {
    let bars = csv::load_bars(path)?;
    replay_bars(config, &bars)
}

/// Replays an in-memory bar slice (used by tests and future data sources).
///
/// # Errors
///
/// Propagates strategy errors from [`Engine::on_bar`].
fn replay_bars(config: &Config, bars: &[Bar]) -> Result<ReplayReport, Error> {
    if bars.is_empty() {
        return Err(Error::MarketData("no bars to replay".to_string()));
    }

    let strategy = ConsecutiveCloses::new(config.consecutive_closes_threshold);
    let mut engine = Engine::new(strategy, PaperBroker::new());
    let mut report = ReplayReport {
        bars: 0,
        entries: 0,
        final_signal: Signal::Flat,
        trace_lines: Vec::with_capacity(bars.len()),
    };

    for bar in bars {
        let signal = engine.on_bar(bar)?;

        // An "entry" is a transition from flat into Long or Short. PaperBroker
        // never fails, so `on_bar` success means the broker now holds the
        // position implied by `signal`.
        let entry = matches!(signal, Signal::Long | Signal::Short)
            && !matches!(report.final_signal, Signal::Long | Signal::Short);
        if entry {
            report.entries = report.entries.saturating_add(1);
        }

        let signal_str = format!("{signal:?}");
        report.trace_lines.push(format!(
            "t={:<13} o={:<9.2} h={:<9.2} l={:<9.2} c={:<9.2} v={:>10.0} -> {}{}",
            ts_secs(bar.timestamp()),
            bar.open(),
            bar.high(),
            bar.low(),
            bar.close(),
            bar.volume(),
            signal_str,
            if entry { "   (entry)" } else { "" },
        ));

        report.bars = report.bars.saturating_add(1);
        report.final_signal = signal;
    }
    Ok(report)
}

/// Renders a report to `out` with a header describing the configuration used.
///
/// # Errors
///
/// Propagates write errors from `out`.
pub fn print_report(
    config: &Config,
    report: &ReplayReport,
    mut out: impl std::io::Write,
) -> std::io::Result<()> {
    let sep = "-".repeat(78);
    writeln!(out, "price-action replay - {} bars", report.bars)?;
    writeln!(
        out,
        "symbol={} mode={:?} quantity={} strategy=consecutive-closes threshold={}",
        config.symbol, config.mode, config.quantity, config.consecutive_closes_threshold,
    )?;
    writeln!(out, "{sep}")?;
    for line in &report.trace_lines {
        writeln!(out, "{line}")?;
    }
    writeln!(out, "{sep}")?;
    writeln!(
        out,
        "result: signal={:?} entries={} of {} bars - paper execution only, no orders placed",
        report.final_signal, report.entries, report.bars,
    )?;
    Ok(())
}

/// Unix-seconds representation of `ts` (the column format used by bar files),
/// clamped for out-of-range timestamps rather than wrapping.
fn ts_secs(ts: std::time::SystemTime) -> i64 {
    use std::time::UNIX_EPOCH;
    match ts.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(_before_epoch) => 0,
    }
}

#[cfg(test)]
mod tests {
    // Test helpers build bars and timestamps from small bounded literals; the
    // same deny-of-wrap policy applies to all non-test code in this crate.
    #![allow(clippy::arithmetic_side_effects)]

    use super::*;
    use std::time::{Duration, SystemTime};

    fn testdir() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("pa-replay-test-{nanos}"));
        std::fs::create_dir_all(&dir).expect("testdir");
        dir
    }

    fn bar(close: f64, secs: u64) -> Bar {
        Bar::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            close - 0.5,
            close + 0.5,
            (close - 1.0).max(0.0),
            close,
            1_000.0,
        )
        .expect("valid bar")
    }

    #[test]
    fn ts_secs_reflects_unix_seconds() {
        assert_eq!(ts_secs(SystemTime::UNIX_EPOCH), 0);
        // `Duration::new` (rather than the nightly-only `from_days`) to
        // satisfy duration_suboptimal_units at the MSRV.
        let one_day = Duration::new(86_400, 0);
        assert_eq!(ts_secs(SystemTime::UNIX_EPOCH + one_day), 86_400);
        // Seconds below the epoch clamp to a documented zero, not a wrap.
        assert_eq!(ts_secs(SystemTime::UNIX_EPOCH - Duration::from_secs(5)), 0);
    }

    #[test]
    fn replay_counts_entries_and_final_signal() {
        let config = Config::default(); // threshold 3
                                        // Four rising closes → Long entry on the 4th, then a drop.
        let bars = vec![
            bar(10.0, 0),
            bar(11.0, 60),
            bar(12.0, 120),
            bar(13.0, 180),
            bar(9.0, 240),
        ];
        let report = replay_bars(&config, &bars).expect("replay");
        assert_eq!(report.bars, 5);
        assert_eq!(report.entries, 1);
        assert!(!matches!(report.final_signal, Signal::Long));

        let mut buf = Vec::new();
        print_report(&config, &report, &mut buf).expect("print");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("(entry)"));
        assert!(text.contains("paper execution only"));
    }

    #[test]
    fn replay_trace_lines_are_one_per_bar_and_labeled() {
        let config = Config::default();
        let bars = vec![bar(10.0, 0), bar(11.0, 60)];
        let report = replay_bars(&config, &bars).expect("replay");
        assert_eq!(report.trace_lines.len(), 2);
        assert!(report.trace_lines[0].starts_with("t="));
        assert!(report.trace_lines[0].contains(" o="));
    }

    #[test]
    fn replay_with_threshold_1_enters_immediately() {
        // Struct-update over Default: one-off override without triggering the
        // field-reassignment lint.
        let config = Config {
            consecutive_closes_threshold: 1,
            ..Config::default()
        };
        // Falling closes: run -1 on bar 2 → Short entry right away.
        let bars = vec![bar(10.0, 0), bar(9.0, 60), bar(8.0, 120)];
        let report = replay_bars(&config, &bars).expect("replay");
        assert_eq!(report.entries, 1);
        assert!(matches!(report.final_signal, Signal::Short));
    }

    #[test]
    fn run_reads_a_csv_file() {
        let dir = testdir();
        let path = dir.join("bars.csv");
        crate::csv::save_bars(vec![bar(1.0, 0), bar(2.0, 60)], &path).expect("save");

        let config = Config::default();
        let report = run(&config, &path).expect("run");
        assert_eq!(report.bars, 2);
    }

    #[test]
    fn run_reports_missing_file() {
        let dir = testdir();
        let config = Config::default();
        let err = run(&config, dir.join("missing.csv")).expect_err("no file");
        assert!(matches!(err, Error::MarketData(_)));
    }

    #[test]
    fn replay_rejects_empty_input() {
        let config = Config::default();
        let err = replay_bars(&config, &[]).expect_err("no bars");
        assert!(matches!(err, Error::MarketData(_)));
    }
}
