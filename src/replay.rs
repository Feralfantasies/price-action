//! Replay of recorded bars through the engine — how a user verifies how the
//! configured strategy behaves against genuine historic data before trusting
//! it anywhere.
//!
//! Replay is read-only by design: it always uses a [`PaperBroker`] regardless
//! of the configured `mode`, reports every position change bar by bar, and
//! never performs real execution.
//!
//! The per-bar pipeline lives in [`ReplaySession`], which offline replay and
//! the live market-data session share: both feed bars into the same engine +
//! paper account, so a live run's report (and persisted CSV) reproduces under
//! an identical offline re-replay **by construction**.

use crate::{
    accounting::{BarOutcome, ClosedTrade, DayTotal, PaperAccount, SessionTotals},
    config::Config,
    csv,
    engine::Engine,
    error::Error,
    execution::PaperBroker,
    market::Bar,
    strategy::{ConsecutiveCloses, Signal},
};

/// Output of one replay (or live session): the per-bar trace, closed paper
/// trades with their realized results, per-UTC-day roll-ups, and a session
/// total.
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
    pub closed_trades: Vec<ClosedTrade>,
    /// Roll-ups by UTC calendar day over the session.
    pub per_day_totals: Vec<DayTotal>,
    /// Whole-session roll-up (equity marked at the final bar's close).
    pub session_totals: SessionTotals,
}

/// What one bar produced inside a [`ReplaySession`].
///
/// A named struct rather than a tuple: both consumers (offline replay and the
/// live session) destructure it, and positional tuples of four
/// same-ish-looking values are exactly how a caller ends up reading a `String`
/// as a trade list.
#[derive(Debug, Clone)]
pub struct BarResult {
    /// The raw strategy signal for this bar.
    pub signal: Signal,
    /// The paper account's state and decision after pricing this bar.
    pub outcome: BarOutcome,
    /// Trades closed **while pricing this bar**, in exit order.
    pub closed_trades: Vec<ClosedTrade>,
    /// The rendered trace line, already appended to the session's trace.
    pub trace_line: String,
}

/// Incremental consumer of bars through the engine + paper account.
///
/// This is the single source of truth for per-bar behaviour: offline replay
/// feeds a recorded file into one finished session, and the live session
/// feeds streamed bars into the same type — so entries/exits, fees, roll-ups
/// and trace lines come from exactly one implementation in both paths.
pub struct ReplaySession {
    engine: Engine<ConsecutiveCloses, PaperBroker>,
    account: PaperAccount,
    trace_lines: Vec<String>,
    entries: usize,
    skipped_entries: usize,
}

impl ReplaySession {
    /// Creates an empty session trading `quantity` units from
    /// `starting_balance`, charging `trade_fee_bps` per side on each notional,
    /// with the example strategy firing at `threshold` consecutive closes.
    #[must_use]
    pub fn new(quantity: u32, starting_balance: f64, trade_fee_bps: u32, threshold: u32) -> Self {
        Self {
            engine: Engine::new(ConsecutiveCloses::new(threshold), PaperBroker::new()),
            account: PaperAccount::new(quantity, starting_balance, trade_fee_bps),
            trace_lines: Vec::new(),
            entries: 0,
            skipped_entries: 0,
        }
    }

    /// Number of bars consumed so far.
    #[must_use]
    pub const fn bar_count(&self) -> usize {
        self.trace_lines.len()
    }

    /// Prices `bar` through the engine and paper account, appending its trace
    /// line (the same byte-stable format offline replay prints).
    ///
    /// Returns a [`BarResult`]: the bar's signal, the post-bar account outcome
    /// (funds/equity, any skipped entry), every trade closed **while pricing
    /// this bar** in exit order (ready for a live mock-trade log), the rendered
    /// trace line, and whether this bar was an entry.
    ///
    /// # Errors
    ///
    /// Propagates strategy errors from [`Engine::on_bar`].
    pub fn on_bar(&mut self, bar: &Bar) -> Result<BarResult, Error> {
        // `last_signal` only ever advances on successful execution (PaperBroker
        // cannot fail): this is the position held *before* this bar's decision.
        let held_before = self.engine.last_signal();

        let signal = self.engine.on_bar(bar)?;

        // An "entry" is a transition from flat into Long or Short; the raw
        // trace counts it even when the paper account cannot fund it.
        let entry = matches!(signal, Signal::Long | Signal::Short)
            && !matches!(held_before, Signal::Long | Signal::Short);
        if entry {
            self.entries = self.entries.saturating_add(1);
        }

        // Price the same decision as a funded paper account acting at this
        // bar's close (execution model documented in `accounting`).
        let trades_before = self.account.closed_trades().len();
        let outcome = self.account.on_bar(&signal, self.trace_lines.len(), bar);
        if outcome.entry_skipped {
            self.skipped_entries = self.skipped_entries.saturating_add(1);
        }

        let trace_line = {
            let cash = outcome.state.available;
            let equity = outcome.state.equity;
            format!(
                "t={:<13} o={:<9.2} h={:<9.2} l={:<9.2} c={:<9.2} v={:>10.0} -> {}{note}  cash={cash:.2} equity={equity:.2}",
                ts_secs(bar.timestamp()),
                bar.open(),
                bar.high(),
                bar.low(),
                bar.close(),
                bar.volume(),
                signal_str(signal),
                note = if outcome.entry_skipped {
                    "   (insufficient funds)"
                } else if entry {
                    "   (entry)"
                } else {
                    ""
                },
                cash = cash,
                equity = equity,
            )
        };
        self.trace_lines.push(trace_line.clone());

        let closed = self
            .account
            .closed_trades()
            .iter()
            .skip(trades_before)
            .cloned()
            .collect();
        Ok(BarResult {
            signal,
            outcome,
            closed_trades: closed,
            trace_line,
        })
    }

    /// Appends `note` to the most recently produced trace line.
    ///
    /// The live session uses this to keep the report honest about data holes:
    /// a feed interruption or a jump in window/tick timestamps is recorded on
    /// the first bar seen *after* the hole, so a reader can tell a genuinely
    /// quiet market from missing data. Offline replay never calls it — its
    /// bars come from a complete file. A no-op when no bar has been consumed
    /// yet (the hole preceded all data; the session reports that separately).
    pub fn annotate_last_trace(&mut self, note: &str) {
        if let Some(line) = self.trace_lines.last_mut() {
            line.push_str(note);
        }
    }

    /// Assembles the final report over everything consumed so far, marking
    /// any still-open position at `last_close` (the last bar's close in
    /// replay; the live session passes its own).
    #[must_use]
    pub fn finish(self, last_close: Option<f64>) -> ReplayReport {
        let session_totals = self.account.session_totals(last_close);
        let per_day_totals = self.account.per_day_totals();
        let closed_trades = self.account.closed_trades().to_vec();
        ReplayReport {
            bars: self.trace_lines.len(),
            entries: self.entries,
            skipped_entries: self.skipped_entries,
            final_signal: self.engine.last_signal(),
            trace_lines: self.trace_lines,
            closed_trades,
            per_day_totals,
            session_totals,
        }
    }
}

/// `{:?}` for a signal in the trace's historical layout (`Flat`, `Long`,
/// `Short`).
fn signal_str(signal: Signal) -> String {
    format!("{signal:?}")
}

/// Replays the bar file at `path` through one finished [`ReplaySession`]
/// configured by `config`.
///
/// # Errors
///
/// [`Error::MarketData`] when the bar file cannot be read or parsed.
pub fn run(config: &Config, path: impl AsRef<std::path::Path>) -> Result<ReplayReport, Error> {
    let bars = csv::load_bars(path)?;
    replay_bars(config, &bars)
}

/// Replays an in-memory bar slice through one finished [`ReplaySession`]
/// (used by tests and as the documented offline twin of a live session).
///
/// # Errors
///
/// Propagates strategy errors from [`Engine::on_bar`].
pub fn replay_bars(config: &Config, bars: &[Bar]) -> Result<ReplayReport, Error> {
    if bars.is_empty() {
        return Err(Error::MarketData("no bars to replay".to_string()));
    }

    let mut session = ReplaySession::new(
        config.quantity,
        config.starting_balance,
        config.trade_fee_bps,
        config.consecutive_closes_threshold,
    );
    for bar in bars {
        session.on_bar(bar)?;
    }
    let last_close = bars.last().map(Bar::close);
    Ok(session.finish(last_close))
}

/// Renders a report to `out`.
///
/// Header (mode, quantity and the sized paper account included), per-bar trace,
/// closed trade details, per-UTC-day totals, session totals. `source` labels
/// the data source in the first line — `"replay"` for recorded files, `"live"`
/// for a streamed session; it is the only difference between the two renderings
/// of the same report shape.
///
/// # Errors
///
/// Propagates write errors from `out`.
pub fn render_report(
    config: &Config,
    report: &ReplayReport,
    source: &str,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    let sep = "-".repeat(78);
    writeln!(out, "price-action {source} - {} bars", report.bars)?;
    // Header line preserved exactly as offline replay has always printed it —
    // it is pinned in README.md and docs/replay-workflow.md. Only `{source}`
    // differs between a replay and a live session.
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

/// Offline replay rendering — the historical header is preserved exactly.
///
/// # Errors
///
/// Propagates write errors from `out`.
pub fn print_report(
    config: &Config,
    report: &ReplayReport,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    render_report(config, report, "replay", out)
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

    fn config_with_threshold(n: u32) -> Config {
        Config {
            consecutive_closes_threshold: n,
            ..Config::default()
        }
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
        let config = config_with_threshold(1);
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

    #[test]
    fn incremental_session_feed_matches_batch_replay() {
        // The live path feeds bars one at a time through `ReplaySession`; the
        // offline twin replays them in a batch. Both must produce identical
        // reports — that equivalence is what makes re-replaying a live CSV an
        // exact reproduction of the session.
        let config = config_with_threshold(1);
        let closes = [10.0, 10.5, 11.0, 11.5, 12.0, 11.4, 10.8, 10.2, 9.6];
        let bars: Vec<Bar> = closes
            .iter()
            .enumerate()
            .map(|(i, &close)| bar(close, u64::try_from(i).expect("index fits") * 60))
            .collect();

        let batch = replay_bars(&config, &bars).expect("batch");
        let mut session = ReplaySession::new(
            config.quantity,
            config.starting_balance,
            config.trade_fee_bps,
            config.consecutive_closes_threshold,
        );
        for b in &bars {
            session.on_bar(b).expect("on_bar");
        }
        let streamed = session.finish(bars.last().map(Bar::close));

        assert_eq!(batch.bars, streamed.bars);
        assert_eq!(batch.entries, streamed.entries);
        assert_eq!(batch.skipped_entries, streamed.skipped_entries);
        assert_eq!(batch.final_signal, streamed.final_signal);
        assert_eq!(batch.trace_lines, streamed.trace_lines); // byte-identical trace
        assert_eq!(batch.closed_trades, streamed.closed_trades);
        assert_eq!(batch.session_totals, streamed.session_totals);
        assert_eq!(batch.per_day_totals, streamed.per_day_totals);

        // The session also produced a closed trade worth logging live.
        assert!(!streamed.closed_trades.is_empty());
    }

    #[test]
    fn on_bar_returns_trace_line_and_closed_trades() {
        let config = config_with_threshold(1);
        // The first bar has no previous close, so no run can be counted yet:
        // the strategy stays Flat and nothing is booked. The second bar closes
        // lower — one lower close meets threshold 1 — so *it* enters Short.
        let bars = [bar(10.0, 0), bar(9.0, 60)];
        let mut session = ReplaySession::new(
            config.quantity,
            config.starting_balance,
            config.trade_fee_bps,
            config.consecutive_closes_threshold,
        );

        let first = session.on_bar(&bars[0]).expect("bar 1");
        assert_eq!(first.signal, Signal::Flat);
        assert!(
            first.closed_trades.is_empty(),
            "no trade can close on bar 1"
        );
        assert!(
            !first.trace_line.contains("(entry)"),
            "{}",
            first.trace_line
        );
        assert!(first.trace_line.starts_with("t=0"), "{}", first.trace_line);

        let second = session.on_bar(&bars[1]).expect("bar 2");
        assert_eq!(second.signal, Signal::Short);
        assert_eq!(second.outcome.effective_signal, Signal::Short);
        assert!(
            !second.outcome.entry_skipped,
            "the default balance funds one unit"
        );
        assert!(second.closed_trades.is_empty(), "an entry closes nothing");
        assert!(
            second.trace_line.contains("(entry)"),
            "{}",
            second.trace_line
        );
        // The funded-account columns the live log and report both rely on.
        assert!(
            second.trace_line.contains("cash=") && second.trace_line.contains("equity="),
            "{}",
            second.trace_line
        );
    }

    #[test]
    fn rendered_report_offline_header_is_unchanged() {
        let config = Config::default();
        let bars = vec![bar(10.0, 0)];
        let report = replay_bars(&config, &bars).expect("replay");
        let mut buf = Vec::new();
        render_report(&config, &report, "replay", &mut buf).expect("print");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.starts_with("price-action replay - 1 bars"), "{text}");
        // Source label is substitutable for live sessions.
        let mut buf = Vec::new();
        render_report(&config, &report, "live", &mut buf).expect("print");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.starts_with("price-action live - 1 bars"), "{text}");
    }
}
