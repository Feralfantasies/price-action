//! Replay of recorded bars through the engine — how a user verifies how the
//! configured strategy behaves against genuine historic data before trusting
//! it anywhere.
//!
//! Replay is read-only by design: it always uses a [`PaperBroker`] regardless
//! of the configured `mode`, reports every position change bar by bar, and
//! never performs real execution.

use crate::{
    accounting::PaperAccount,
    config::Config,
    csv,
    engine::Engine,
    error::Error,
    execution::PaperBroker,
    market::Bar,
    strategy::{ConsecutiveCloses, Signal},
};

/// Output of one replay: the per-bar trace, closed paper trades with their
/// realized results, per-UTC-day roll-ups, and a session total.
#[derive(Debug, Clone)]
pub struct ReplayReport {
    /// Number of bars fed to the engine.
    pub bars: usize,
    /// Number of separate entries (transitions from flat into Long/Short).
    pub entries: usize,
    /// Entries the paper account skipped for lack of funds (the signal trace
    /// still counts them; the account stayed flat on those bars).
    pub skipped_entries: usize,
    /// Final signal after all bars were processed.
    pub final_signal: Signal,
    /// One human-readable line per bar (includes each bar's paper cash/equity).
    pub trace_lines: Vec<String>,
    /// Closed paper trades in exit order, each net of its fees.
    pub closed_trades: Vec<crate::accounting::ClosedTrade>,
    /// Roll-ups by UTC calendar day over the session.
    pub per_day_totals: Vec<crate::accounting::DayTotal>,
    /// Whole-session roll-up (equity marked at the final bar's close).
    pub session_totals: crate::accounting::SessionTotals,
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
    let mut account = PaperAccount::new(
        config.quantity,
        config.starting_balance,
        config.trade_fee_bps,
    );
    let mut report = ReplayReport {
        bars: 0,
        entries: 0,
        skipped_entries: 0,
        final_signal: Signal::Flat,
        trace_lines: Vec::with_capacity(bars.len()),
        closed_trades: Vec::new(),
        per_day_totals: Vec::new(),
        session_totals: crate::accounting::SessionTotals {
            starting_balance: config.starting_balance,
            final_available: 0.0,
            final_equity: 0.0,
            total_fees_paid: 0.0,
            total_net_pl: 0.0,
        },
    };

    for (index, bar) in bars.iter().enumerate() {
        let signal = engine.on_bar(bar)?;

        // An "entry" is a transition from flat into Long or Short; the raw
        // trace counts it even when the paper account cannot fund it.
        let entry = matches!(signal, Signal::Long | Signal::Short)
            && !matches!(report.final_signal, Signal::Long | Signal::Short);
        if entry {
            report.entries = report.entries.saturating_add(1);
        }

        // Price the same decision as a funded paper account acting at this
        // bar's close (execution model documented in `accounting`).
        // Capture how many trades are already closed: anything appended while
        // pricing this bar is one completed trade to report on, exit-ordered.
        let trades_before = account.closed_trades().len();
        let outcome = account.on_bar(&signal, index, bar);
        if outcome.entry_skipped {
            report.skipped_entries = report.skipped_entries.saturating_add(1);
        }
        let signal_str = format!("{signal:?}");
        report.trace_lines.push(format!(
            "t={:<13} o={:<9.2} h={:<9.2} l={:<9.2} c={:<9.2} v={:>10.0} -> {}{note}  cash={cash:.2} equity={equity:.2}",
            ts_secs(bar.timestamp()),
            bar.open(),
            bar.high(),
            bar.low(),
            bar.close(),
            bar.volume(),
            signal_str,
            note = if outcome.entry_skipped {
                "   (insufficient funds)"
            } else if entry {
                "   (entry)"
            } else {
                ""
            },
            cash = outcome.state.available,
            equity = outcome.state.equity,
        ));
        for trade in account.closed_trades().iter().skip(trades_before) {
            report.closed_trades.push(trade.clone());
        }

        report.bars = report.bars.saturating_add(1);
        report.final_signal = signal;
    }

    // Final roll-ups; the last bar is the final mark for any open position.
    let last_close = bars.last().map(Bar::close);
    report.session_totals = account.session_totals(last_close);
    report.per_day_totals = account.per_day_totals();

    Ok(report)
}

/// Renders a report to `out` with a header describing the configuration used
/// (including paper-account size and fees), the per-bar trace, closed trade
/// details, per-UTC-day totals, and session totals.
///
/// # Errors
///
/// Propagates write errors from `out`.
pub fn print_report(
    config: &Config,
    report: &ReplayReport,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    let sep = "-".repeat(78);
    writeln!(out, "price-action replay - {} bars", report.bars)?;
    writeln!(
        out,
        "symbol={} mode={:?} quantity={} strategy=consecutive-closes threshold={}",
        config.symbol, config.mode, config.quantity, config.consecutive_closes_threshold,
    )?;
    writeln!(
        out,
        "paper account: starting_balance={:.2} trade_fee_bps={} (per side, on the notional)",
        config.starting_balance, config.trade_fee_bps,
    )?;
    writeln!(out, "{sep}")?;
    for line in &report.trace_lines {
        writeln!(out, "{line}")?;
    }
    writeln!(out, "{sep}")?;

    print_closed_trades(out, report)?;
    write_blank_line(out)?;
    print_per_day_totals(out, report)?;
    write_blank_line(out)?;
    print_session_totals(out, report)?;
    write_blank_line(out)?;

    writeln!(
        out,
        "result: signal={:?} entries={} of {} bars - paper execution only, no orders placed",
        report.final_signal, report.entries, report.bars,
    )?;
    Ok(())
}

/// Closed-paper-trade table (money fields net of fees where labelled). No-
/// trade runs print a single `none` line so the section is always present.
fn print_closed_trades(out: &mut dyn std::io::Write, report: &ReplayReport) -> std::io::Result<()> {
    writeln!(out, "closed paper trades (net of fees):")?;
    if report.closed_trades.is_empty() {
        return writeln!(out, "  none");
    }

    let mut rows: Vec<Vec<String>> = vec![vec![
        "#".to_string(),
        "side".to_string(),
        "entry day".to_string(),
        "entry @".to_string(),
        "exit day".to_string(),
        "exit @".to_string(),
        "invested".to_string(),
        "gross P/L".to_string(),
        "fees".to_string(),
        "net P/L".to_string(),
    ]];
    for t in &report.closed_trades {
        rows.push(vec![
            format!("{}.", t.index),
            if t.side_is_long { "long" } else { "short" }.to_string(),
            t.entry_day.clone(),
            money2(t.entry_price),
            t.exit_day.clone(),
            money2(t.exit_price),
            money2(t.investment),
            signed2(t.gross_pl),
            money2(t.fees_paid),
            signed2(t.net_pl),
        ]);
    }
    write_table(out, &rows)
}

/// Per-UTC-day roll-ups in chronological order.
fn print_per_day_totals(
    out: &mut dyn std::io::Write,
    report: &ReplayReport,
) -> std::io::Result<()> {
    writeln!(out, "totals per UTC day (24h):")?;
    if report.per_day_totals.is_empty() {
        return writeln!(out, "  none");
    }

    let mut rows: Vec<Vec<String>> = vec![vec![
        "day".to_string(),
        "entries".to_string(),
        "exits".to_string(),
        "realized P/L (net)".to_string(),
    ]];
    for d in &report.per_day_totals {
        rows.push(vec![
            d.day.clone(),
            d.entries.to_string(),
            d.exits.to_string(),
            signed2(d.net_realized_pl),
        ]);
    }
    write_table(out, &rows)
}

/// Whole session: funds at the end (a still-open open position keeps its
/// capital/collateral out of `available`), realized P/L net of fees.
fn print_session_totals(
    out: &mut dyn std::io::Write,
    report: &ReplayReport,
) -> std::io::Result<()> {
    writeln!(out, "session totals:")?;
    let s = &report.session_totals;
    let rows = vec![
        vec!["starting balance".to_string(), money2(s.starting_balance)],
        vec![
            "final available funds".to_string(),
            money2(s.final_available),
        ],
        vec![
            "final equity (marked at last close)".to_string(),
            money2(s.final_equity),
        ],
        vec![
            format!("realized P/L, net of fees ({})", report.closed_trades.len()),
            signed2(s.total_net_pl),
        ],
        vec![
            "fees paid (all legs)".to_string(),
            money2(s.total_fees_paid),
        ],
    ];
    write_table(out, &rows)
}

fn write_blank_line(out: &mut dyn std::io::Write) -> std::io::Result<()> {
    writeln!(out)
}

/// `f64` → two-decimal money (non-negative display value; P/L signs are
/// carried by [`signed2`]).
fn money2(value: f64) -> String {
    format!("{value:.2}")
}

/// Signed two-decimal money (`+1.00`, `-2.50`, `+0.00`).
fn signed2(value: f64) -> String {
    format!("{value:+.2}")
}

/// Renders rows of cells with per-column left alignment and a single space
/// between columns; column widths follow the widest cell in that column. Rows
/// are all the same width by construction (the caller builds them from fixed
/// tables), so `zip`/`first` drive the layout rather than indices.
fn write_table(out: &mut dyn std::io::Write, rows: &[Vec<String>]) -> std::io::Result<()> {
    let Some(header) = rows.first() else {
        return Ok(());
    };

    let mut widths: Vec<usize> = header.iter().map(std::string::String::len).collect();
    for row in rows.iter().skip(1) {
        for (cell, width) in row.iter().zip(widths.iter_mut()) {
            *width = (*width).max(cell.len());
        }
    }

    for row in rows {
        let mut cells = row.clone();
        for (cell, width) in cells.iter_mut().zip(&widths) {
            while cell.len() < *width {
                cell.push(' ');
            }
        }
        writeln!(out, "  {}", cells.join(" "))?;
    }
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
        assert_eq!(report.skipped_entries, 0);
        assert!(!matches!(report.final_signal, Signal::Long));

        // Paper account: one long entered at bar 4's close of 13.00, exited
        // at bar 5's close of 9.00 → gross -4, fees (13+9)*5e-4 = 0.011.
        assert_eq!(report.closed_trades.len(), 1);
        let t = &report.closed_trades[0];
        assert!(t.side_is_long);
        assert!((t.entry_price - 13.0).abs() < 1e-9);
        assert!((t.net_pl - (-4.011)).abs() < 1e-9);
        let s = &report.session_totals;
        assert!((s.total_net_pl - (-4.011)).abs() < 1e-9);
        assert!((s.final_available - s.final_equity).abs() < 1e-9); // flat at end

        let mut buf = Vec::new();
        print_report(&config, &report, &mut buf).expect("print");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("(entry)"));
        assert!(text.contains("closed paper trades (net of fees):"));
        assert!(text.contains("totals per UTC day (24h):"));
        assert!(text.contains("session totals:"));
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
        // Every line carries the post-bar paper account state (2dp amounts).
        for line in &report.trace_lines {
            assert!(line.contains("cash="));
            assert!(line.contains("equity="));
        }
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
    fn replay_marks_skipped_entries_when_the_account_cannot_fund_them() {
        // One unit at 1.00 cannot afford the 9.00 entry; each bar of the
        // Short run retries an unfunded entry and stays flat.
        let config = Config {
            consecutive_closes_threshold: 1,
            starting_balance: 1.0,
            ..Config::default()
        };
        let bars = vec![bar(10.0, 0), bar(9.0, 60)];
        let report = replay_bars(&config, &bars).expect("replay");
        assert_eq!(report.entries, 1); // raw-signal entry still counted
        assert_eq!(report.skipped_entries, 1);
        assert!(report.closed_trades.is_empty());
        assert!(report.per_day_totals.is_empty());
        assert!((report.session_totals.final_equity - 1.0).abs() < 1e-9); // never touched

        let mut buf = Vec::new();
        print_report(&config, &report, &mut buf).expect("print");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("(insufficient funds)"));
        assert!(text.contains("cash=1.00 equity=1.00"));
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
