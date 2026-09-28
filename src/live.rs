//! Live market-data sessions: paper trading against **real** time data.
//!
//! [`run_session`] drives one session end to end:
//! - the [`crate::feed`] feeder streams events (minute windows or ticks for
//!   one symbol), reconnecting on its own when the socket drops;
//! - [`BarShaper`] turns those events into `Bar`s — holding each in-flight
//!   minute window until the next one starts so the bar carries its **final**
//!   numbers, aggregating ticks into per-second bars, and marking data holes
//!   (a timestamp jump or a feed interruption) with per-bar gap notes so the
//!   report stays honest about missing data;
//! - every bar flows through the same [`crate::replay::ReplaySession`] used
//!   by offline replay — entries/exits/fees/roll-ups are computed by exactly
//!   the same code, which is what makes re-replaying the persisted CSV an
//!   exact reproduction of the session;
//! - mock trades are logged to stdout with their realized numbers the moment
//!   they close; on stop the in-flight window/bucket is flushed, the session
//!   report is returned to the caller and every bar is persisted as a
//!   `replay`-compatible CSV.
//!
//! **No venue is ever contacted for execution.** The feed is data-in only and
//! the account is the in-memory paper account from offline replay: a live
//! session cannot place an order, so "how much it would have put down" is a
//! reported number, never a trade.
//!
//! The shaper is kept free of sockets and clocks so it is unit-testable by
//! feeding synthetic events directly.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;

use crate::{
    accounting::{utc_day, BarState, ClosedTrade},
    config::Config,
    csv,
    error::Error,
    feed::{FeedChannel, FeedSettings, RawEvent, WindowUpdate},
    market::Bar,
    replay::{BarResult, ReplayReport, ReplaySession},
    strategy::Signal,
};

/// Buffer between feeder and session.
///
/// Minute windows are ~1 event/min per symbol; even a bursty tick stream stays
/// far under this within the buffer's time horizon. Larger buys latency
/// headroom at negligible memory cost.
pub const EVENT_CHANNEL_SIZE: usize = 1024;

/// Buffer for finished-day summaries.
///
/// At most one summary per UTC day plus a final partial one, so a handful of
/// slots is ample; the point is that a slow or dead notifier (Telegram
/// unreachable) can never stall the session.
pub const SUMMARY_CHANNEL_SIZE: usize = 16;

/// One minute in Unix milliseconds — the aggregate feed's window length, and
/// the silence that is still "normal" before a bar is called a data hole.
const MINUTE_MS: u64 = 60_000;

/// Seconds in a UTC day; the roll-up bucket boundary.
const SECS_PER_DAY: i64 = 86_400;

/// One shaped bar plus, when data was missed before it, the rendered note to
/// attach to its trace line (e.g. `"   [feed gap: ~120s of missing windows]"`).
#[derive(Debug)]
pub struct ShapedBar {
    /// The bar itself, ready for the shared engine + paper account.
    pub bar: Bar,
    /// A rendered `[feed gap: …]` note when data was missed before this bar.
    pub gap_note: Option<String>,
}

/// A minute window received but not yet emitted: the feed re-emits the same
/// window with fresh numbers as trades land inside it, so the shaper holds it
/// and only turns it into a bar once the *next* window starts (or the session
/// flushes). Emitting on first sight would trade on a partial minute.
#[derive(Debug, Clone, Copy)]
struct PendingWindow {
    start_ms: u64,
    end_ms: u64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

/// One open second of tick trades, accumulating into a single bar.
#[derive(Debug, Clone, Copy)]
struct TickBucket {
    ts_secs: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

/// Event → bar state machine for one session. Socket-free and clock-free:
/// every timestamp comes from the event, so behaviour is reproducible in
/// tests and in a re-replay of the persisted CSV.
#[derive(Debug)]
pub struct BarShaper {
    mode: ShaperMode,
    /// Unix ms end of the last window turned into a bar (minute mode).
    last_window_end_ms: Option<u64>,
    /// The window currently being re-emitted upstream (minute mode).
    pending_window: Option<PendingWindow>,
    /// The second currently accumulating trades (tick mode).
    tick_bucket: Option<TickBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShaperMode {
    /// Native per-minute aggregate windows (`AM`); one bar per traded minute.
    Minute,
    /// Tick trades (`T`) aggregated locally into per-second bars.
    Ticks,
}

impl BarShaper {
    /// Native minute-window mode (one bar per traded minute).
    #[must_use]
    pub const fn new_minute() -> Self {
        Self {
            mode: ShaperMode::Minute,
            last_window_end_ms: None,
            pending_window: None,
            tick_bucket: None,
        }
    }

    /// Tick-aggregation mode (one bar per UTC second with ≥1 trade).
    #[must_use]
    pub const fn new_ticks() -> Self {
        Self {
            mode: ShaperMode::Ticks,
            last_window_end_ms: None,
            pending_window: None,
            tick_bucket: None,
        }
    }

    /// Consume one event; returns 0–1 closed bars (the bar held in flight
    /// closes when a newer one arrives). Events this mode does not consume —
    /// ticks on a minute feed, windows on a tick feed, feed-interruption
    /// markers — yield nothing; the session handles interruptions itself.
    /// Events with unusable values are rejected with [`Error::MarketData`]
    /// rather than silently corrupting the session's numbers.
    ///
    /// # Errors
    ///
    /// [`Error::MarketData`] when a price/volume is not finite or is negative,
    /// or when a validated event still cannot form a [`Bar`].
    pub fn on_event(&mut self, event: RawEvent) -> Result<Vec<ShapedBar>, Error> {
        match (self.mode, event) {
            (ShaperMode::Minute, RawEvent::Window(w)) => self.on_window(&w),
            (ShaperMode::Ticks, RawEvent::Tick(t)) => self.on_tick(t.ts_ms, t.price, t.size),
            // Cross-mode events are not expected upstream, and interruptions
            // are the session's bookkeeping: neither produces a bar.
            (_, RawEvent::Window(_) | RawEvent::Tick(_) | RawEvent::FeedInterrupted) => {
                Ok(Vec::new())
            }
        }
    }

    /// Flush whatever is still in flight as its final bar — called by the
    /// session on stop so no price action is lost. A minute window flushed
    /// this way may be partial (the minute had not closed upstream yet); that
    /// is unavoidable at shutdown and is visible as the session's last bar.
    ///
    /// # Errors
    ///
    /// [`Error::MarketData`] when the held values cannot form a [`Bar`].
    pub fn flush(&mut self) -> Result<Vec<ShapedBar>, Error> {
        match self.mode {
            ShaperMode::Minute => match self.pending_window.take() {
                Some(window) => Ok(vec![self.emit_window(window)?]),
                None => Ok(Vec::new()),
            },
            ShaperMode::Ticks => match self.tick_bucket.take() {
                Some(bucket) => Ok(vec![ShapedBar {
                    bar: bucket_to_bar(bucket)?,
                    gap_note: None,
                }]),
                None => Ok(Vec::new()),
            },
        }
    }

    /// Minute-window handling: dedupe re-emissions of the window in flight,
    /// drop stale (out-of-order) ones, and close the held window when a newer
    /// one starts.
    fn on_window(&mut self, w: &WindowUpdate) -> Result<Vec<ShapedBar>, Error> {
        validate_money(w)?;

        let start_ms = w.start_ms;
        // An absent or non-advancing end is treated as one interval long, so
        // the dedupe anchor below is always strictly after the start.
        let end_ms = w
            .end_ms
            .filter(|end| *end > start_ms)
            .unwrap_or_else(|| start_ms.saturating_add(MINUTE_MS));

        let incoming = PendingWindow {
            start_ms,
            end_ms,
            open: w.open,
            high: w.high,
            low: w.low,
            close: w.close,
            volume: w.volume,
        };

        let Some(held) = self.pending_window else {
            self.pending_window = Some(incoming);
            return Ok(Vec::new());
        };

        if start_ms == held.start_ms {
            // In-flight re-emission of the same window: keep the newer
            // numbers, emit nothing yet (the final update is the bar).
            self.pending_window = Some(incoming);
            return Ok(Vec::new());
        }
        if start_ms < held.start_ms {
            // Out-of-order/duplicate from before the window in flight: the
            // session already saw a later minute, so this one is stale.
            return Ok(Vec::new());
        }

        // A newer window started → the held one is complete.
        self.pending_window = Some(incoming);
        Ok(vec![self.emit_window(held)?])
    }

    /// Turns a completed window into a bar, attaching a gap note when more
    /// than one interval of windows is missing before it.
    fn emit_window(&mut self, window: PendingWindow) -> Result<ShapedBar, Error> {
        let gap_note = match self.last_window_end_ms {
            Some(anchor) if window.start_ms > anchor => {
                let miss_s = window.start_ms.saturating_sub(anchor) / 1_000;
                (miss_s >= MINUTE_MS / 1_000).then(|| {
                    format!("   [feed gap: ~{miss_s}s of missing windows before this bar]")
                })
            }
            _ => None,
        };
        self.last_window_end_ms = Some(window.end_ms.max(self.last_window_end_ms.unwrap_or(0)));

        let bar = Bar::new(
            ms_to_systemtime(window.start_ms),
            window.open,
            window.high,
            window.low,
            window.close,
            window.volume,
        )
        .map_err(|e| {
            Error::MarketData(format!("cannot build a bar from an aggregate window: {e}"))
        })?;
        Ok(ShapedBar { bar, gap_note })
    }

    /// Tick handling: accumulate within a second, close the bucket when a
    /// later second's trade arrives.
    fn on_tick(&mut self, ts_ms: u64, price: f64, size: f64) -> Result<Vec<ShapedBar>, Error> {
        if !price.is_finite() || price < 0.0 {
            return Err(Error::MarketData(format!(
                "tick has unusable price={price}; event dropped"
            )));
        }
        if !size.is_finite() || size < 0.0 {
            return Err(Error::MarketData(format!(
                "tick has unusable size={size}; event dropped"
            )));
        }

        let ts_secs = i64::try_from(ts_ms / 1_000).unwrap_or(i64::MAX);

        let Some(bucket) = self.tick_bucket else {
            self.tick_bucket = Some(TickBucket {
                ts_secs,
                open: price,
                high: price,
                low: price,
                close: price,
                volume: size,
            });
            return Ok(Vec::new());
        };

        if bucket.ts_secs == ts_secs {
            // Same second: accumulate. First price stays the open, the last
            // one becomes the close, high/low widen; delivery order within the
            // second does not matter.
            self.tick_bucket = Some(TickBucket {
                ts_secs,
                open: bucket.open,
                high: bucket.high.max(price),
                low: bucket.low.min(price),
                close: price,
                volume: bucket.volume + size,
            });
            return Ok(Vec::new());
        }

        if ts_secs < bucket.ts_secs {
            // Out-of-order delivery from before the second in flight: the
            // session already traded a later second. Emitting it would book
            // an older bar after a newer one, and closing the held bucket on
            // its arrival would truncate that second's true high/low/volume
            // — so the tick is stale and dropped (matching the minute-mode
            // rule for out-of-order windows).
            return Ok(Vec::new());
        }

        // A later second: close the held bucket. A jump of ≥2s means at
        // least one traded second was missed → honest gap note; adjacent
        // seconds carry none.
        let gap_note = gap_note_for_seconds(bucket.ts_secs, ts_secs);
        self.tick_bucket = Some(TickBucket {
            ts_secs,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: size,
        });
        Ok(vec![ShapedBar {
            bar: bucket_to_bar(bucket)?,
            gap_note,
        }])
    }
}

/// Rejects non-finite or negative money/volume values from an aggregate
/// window before they can reach a `Bar`.
fn validate_money(w: &WindowUpdate) -> Result<(), Error> {
    for (name, value) in [
        ("open", w.open),
        ("high", w.high),
        ("low", w.low),
        ("close", w.close),
        ("volume", w.volume),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(Error::MarketData(format!(
                "aggregate window has unusable {name}={value}; event dropped"
            )));
        }
    }
    Ok(())
}

/// Gap note for the seconds moved from `from` to `to`: present when at least
/// one whole traded second was missed between them (the next traded second is
/// ≥2s later); adjacent or equal seconds produce no note.
fn gap_note_for_seconds(from: i64, to: i64) -> Option<String> {
    let miss = to.saturating_sub(from);
    (miss >= 2).then(|| format!("   [feed gap: ~{miss}s of missing ticks before this bar]"))
}

/// A closed tick bucket as a bar.
///
/// Bucket values were validated on arrival, so a failure here means a
/// timestamp outside `SystemTime`'s range — reported, never panicked (the
/// panic lints are deny outside tests).
#[allow(clippy::arithmetic_side_effects)] // bounded timestamp math
fn bucket_to_bar(bucket: TickBucket) -> Result<Bar, Error> {
    let secs = u64::try_from(bucket.ts_secs).unwrap_or(0);
    Bar::new(
        UNIX_EPOCH + Duration::from_secs(secs),
        bucket.open,
        bucket.high,
        bucket.low,
        bucket.close,
        bucket.volume,
    )
    .map_err(|e| Error::MarketData(format!("cannot build a bar from tick data: {e}")))
}

/// Unix ms → `SystemTime`.
///
/// Milliseconds before the epoch clamp to it (the same documented contract the
/// CSV/trace timestamp columns use).
#[allow(clippy::arithmetic_side_effects)] // bounded ms→s/ns conversion
fn ms_to_systemtime(ms: u64) -> SystemTime {
    let nanos = u32::try_from((ms % 1_000) * 1_000_000).unwrap_or(0);
    UNIX_EPOCH + Duration::new(ms / 1_000, nanos)
}

/// Unix seconds from a `SystemTime`; pre-epoch clamps to 0.
fn unix_secs(ts: SystemTime) -> i64 {
    match ts.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(_before_epoch) => 0,
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for live-log timestamps (UTC, no timezone leakage —
/// the same civil-date arithmetic as [`crate::accounting::utc_day`]).
fn utc_stamp(ts: SystemTime) -> String {
    let secs = unix_secs(ts);
    let secs_of_day = secs.rem_euclid(SECS_PER_DAY);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        utc_day(secs),
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60
    )
}

/// Everything a finished live session produced.
#[derive(Debug)]
pub struct SessionOutput {
    /// The same report shape offline replay produces (trace, closed trades,
    /// per-UTC-day totals, session totals) — rendered by the caller.
    pub report: ReplayReport,
    /// Where the session's bars were persisted as a `replay`-compatible CSV;
    /// `None` when no bar ever arrived (nothing worth writing).
    pub csv_path: Option<PathBuf>,
    /// Number of bars written to `csv_path` (0 when it is `None`).
    pub bars_written: usize,
}

/// One UTC day's paper-trading result.
///
/// Emitted when that day closes (the first bar of the next UTC day arrives) or
/// when the session stops mid-day. Every money figure comes from the same
/// funded paper account offline replay uses, so a day's numbers reconcile with
/// the report's per-UTC-day roll-up. **Nothing here is a real order** — it is
/// what the configured strategy's decisions would have cost or earned.
#[derive(Debug, Clone, PartialEq)]
pub struct DailySummary {
    /// UTC calendar day (`YYYY-MM-DD`) this summary covers.
    pub day: String,
    /// Symbol the session traded.
    pub symbol: String,
    /// Bars consumed during the day.
    pub bars: usize,
    /// Position entries signalled during the day (funded or not).
    pub entries: usize,
    /// Paper trades that exited during the day.
    pub exits: usize,
    /// Fees paid on the day's exits (per-side bps on each leg's notional).
    pub fees_paid: f64,
    /// Realized P/L over the day's exits, net of those fees.
    pub net_realized_pl: f64,
    /// Free funds at the day's last bar; open-position capital is not free.
    pub ending_cash: f64,
    /// Equity at the day's last bar: cash plus the open position marked there.
    pub ending_equity: f64,
    /// Position still open at the end of the day ([`Signal::Flat`] when none).
    pub open_position: Signal,
    /// True when the day was cut short by session shutdown rather than
    /// reaching midnight UTC.
    pub partial: bool,
}

impl DailySummary {
    /// Human-readable rendering, used for both the console report and the
    /// Telegram message body. Ends with the paper-only framing the repo's
    /// disclaimer requires: this is never a trading recommendation.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "price-action paper trading — {symbol} — {day}{partial}\n\
             bars: {bars}   entries: {entries}   exits: {exits}\n\
             fees paid: {fees:.2}\n\
             realized P/L (net of fees): {pl:+.2}\n\
             ending balance: cash {cash:.2} / equity {equity:.2}\n\
             open position: {position:?}\n\
             paper account only — no orders were placed, not financial advice.",
            symbol = self.symbol,
            day = self.day,
            partial = if self.partial {
                " (partial: session stopped)"
            } else {
                ""
            },
            bars = self.bars,
            entries = self.entries,
            exits = self.exits,
            fees = self.fees_paid,
            pl = self.net_realized_pl,
            cash = self.ending_cash,
            equity = self.ending_equity,
            position = self.open_position,
        )
    }
}

/// Running totals for the UTC day currently in progress.
#[derive(Debug, Clone)]
struct DayAccumulator {
    /// The day being accumulated; empty until the first bar arrives.
    day: String,
    bars: usize,
    entries: usize,
    exits: usize,
    fees_paid: f64,
    net_realized_pl: f64,
    ending_cash: f64,
    ending_equity: f64,
    open_position: Signal,
}

impl DayAccumulator {
    /// An accumulator for a session that has not seen a bar yet: no day, and
    /// the starting balance as the only known money.
    const fn new(starting_balance: f64) -> Self {
        Self {
            day: String::new(),
            bars: 0,
            entries: 0,
            exits: 0,
            fees_paid: 0.0,
            net_realized_pl: 0.0,
            ending_cash: starting_balance,
            ending_equity: starting_balance,
            open_position: Signal::Flat,
        }
    }

    /// If `day` starts a new UTC day, returns the finished day's summary and
    /// resets the activity counters. The *account* is continuous across
    /// midnight, so balances and any open position carry over untouched.
    /// Returns `None` when the day is unchanged or nothing was recorded yet.
    fn roll_over(&mut self, day: &str, symbol: &str) -> Option<DailySummary> {
        if self.day.is_empty() || self.day == day {
            return None;
        }
        self.take(symbol, false)
    }

    /// Adds one bar's result to the day in progress.
    ///
    /// Entries are counted the way [`crate::accounting::PaperAccount::per_day_totals`]
    /// counts them — one per **funded position opened**, so a reversal books an
    /// exit *and* an entry on that day, and an entry the account could not fund
    /// books neither. Counting the raw strategy signal instead would disagree
    /// with the report's own per-UTC-day table.
    #[allow(clippy::arithmetic_side_effects)] // bounded money math; same policy as `accounting`
    fn record(&mut self, day: &str, result: &BarResult) {
        self.day = day.to_string();
        self.bars = self.bars.saturating_add(1);
        let now_held = result.outcome.effective_signal;
        if now_held != Signal::Flat && now_held != self.open_position {
            self.entries = self.entries.saturating_add(1);
        }
        for trade in &result.closed_trades {
            self.exits = self.exits.saturating_add(1);
            self.fees_paid += trade.fees_paid;
            self.net_realized_pl += trade.net_pl;
        }
        self.ending_cash = result.outcome.state.available;
        self.ending_equity = result.outcome.state.equity;
        self.open_position = now_held;
    }

    /// Emits the accumulated day as a summary and resets for the next one.
    /// `None` when no bar has been recorded — there is no day to report.
    fn take(&mut self, symbol: &str, partial: bool) -> Option<DailySummary> {
        if self.day.is_empty() || self.bars == 0 {
            return None;
        }
        let summary = DailySummary {
            day: self.day.clone(),
            symbol: symbol.to_string(),
            bars: self.bars,
            entries: self.entries,
            exits: self.exits,
            fees_paid: self.fees_paid,
            net_realized_pl: self.net_realized_pl,
            ending_cash: self.ending_cash,
            ending_equity: self.ending_equity,
            open_position: self.open_position,
            partial,
        };
        // Carry the continuous account state into the fresh day.
        *self = Self {
            day: String::new(),
            bars: 0,
            entries: 0,
            exits: 0,
            fees_paid: 0.0,
            net_realized_pl: 0.0,
            ending_cash: summary.ending_cash,
            ending_equity: summary.ending_equity,
            open_position: summary.open_position,
        };
        Some(summary)
    }
}

/// Mutable state one session carries across bars.
struct SessionState {
    session: ReplaySession,
    /// Every bar consumed, in order — persisted at the end so the session can
    /// be re-replayed offline.
    bars: Vec<Bar>,
    /// Final mark for a position still open at shutdown: the last bar's close,
    /// identical semantics to offline replay's final mark.
    last_close: Option<f64>,
    /// A feed interruption is only honest once there is a bar to attach it to.
    pending_gap_note: Option<String>,
    /// Running totals for the UTC day in progress.
    day: DayAccumulator,
}

impl SessionState {
    fn new(config: &Config) -> Self {
        Self {
            session: ReplaySession::new(
                config.quantity,
                config.starting_balance,
                config.trade_fee_bps,
                config.consecutive_closes_threshold,
            ),
            bars: Vec::new(),
            last_close: None,
            pending_gap_note: None,
            day: DayAccumulator::new(config.starting_balance),
        }
    }

    /// Feeds one shaped bar through the shared session: merges any queued
    /// interruption note with the shaper's own gap note, records the bar for
    /// persistence, logs mock trades closed by this bar, remembers its close
    /// as the session's final mark, and folds it into the day's totals.
    ///
    /// Returns the summary of a UTC day that this bar just closed — usually
    /// `None`, since a day closes at most once.
    fn consume(
        &mut self,
        shaped: ShapedBar,
        config: &Config,
    ) -> Result<Option<DailySummary>, Error> {
        let gap_note = merge_notes(self.pending_gap_note.take(), shaped.gap_note);
        let bar = shaped.bar;
        let day = utc_day(unix_secs(bar.timestamp()));

        // A bar from a new UTC day closes the previous one *before* anything
        // is booked against the new day.
        let closed_day = self.day.roll_over(&day, config.symbol.as_str());

        let result = self.session.on_bar(&bar)?;
        if let Some(note) = gap_note {
            self.session.annotate_last_trace(&note);
            eprintln!("live session:{}", note.trim_start());
        }

        self.bars.push(bar);
        self.last_close = Some(bar.close());
        self.day.record(&day, &result);

        for trade in &result.closed_trades {
            log_mock_trade(trade, bar.timestamp(), config, result.outcome.state);
        }
        Ok(closed_day)
    }

    /// Summary for the day still in progress, marked partial; `None` when the
    /// session never saw a bar.
    fn finish_day(&mut self, symbol: &str) -> Option<DailySummary> {
        self.day.take(symbol, true)
    }
}

/// One full live session, run to completion on the caller's tokio runtime.
///
/// Reads events from `rx` until the channel closes **or** `shutdown`
/// completes (the binary passes `ctrl_c`, so a session stops cleanly: the
/// in-flight window/bucket is flushed, the day in progress is summarized, the
/// report assembled and the bars persisted). The feeder never closes the
/// channel itself — it reconnects forever — so `shutdown` is the normal way a
/// session ends.
///
/// Every finished UTC day is pushed to `summaries` as a [`DailySummary`] — one
/// per day boundary crossed, plus a final `partial` one for the day the session
/// stopped in. Delivery is best-effort (see [`deliver_summary`]): a dead
/// consumer costs a log line, never the session.
///
/// # Errors
///
/// [`Error::MarketData`] when an event cannot be shaped into a bar or the
/// session CSV cannot be written; [`Error::Strategy`] propagated from the
/// shared engine.
pub async fn run_session(
    config: &Config,
    settings: &FeedSettings,
    mut rx: mpsc::Receiver<RawEvent>,
    shutdown: impl Future<Output = ()>,
    summaries: mpsc::Sender<DailySummary>,
) -> Result<SessionOutput, Error> {
    let started_at = SystemTime::now();
    let mut shaper = match config.live_feed_channel {
        FeedChannel::Minute => BarShaper::new_minute(),
        FeedChannel::Ticks => BarShaper::new_ticks(),
    };
    let mut state = SessionState::new(config);
    let mut interruptions_before_first_bar = 0usize;

    eprintln!(
        "live session started — symbol={} channel={} host={} bar_interval={}s threshold={}",
        settings.symbol,
        channel_label(config.live_feed_channel),
        config.live_feed_host,
        config.bar_interval_secs,
        config.consecutive_closes_threshold,
    );

    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            maybe_event = rx.recv() => {
                let Some(event) = maybe_event else { break }; // sender dropped → session over
                match event {
                    RawEvent::FeedInterrupted => {
                        if state.bars.is_empty() {
                            interruptions_before_first_bar =
                                interruptions_before_first_bar.saturating_add(1);
                        } else {
                            state.pending_gap_note =
                                Some("   [feed interrupted; reconnecting]".to_string());
                        }
                    }
                    other => {
                        // One unshapable event must not end the whole live
                        // session: log it and keep consuming (mirrors the
                        // feeder skipping unparseable frames). Errors from
                        // `consume` — the account actually booking a bar —
                        // still propagate.
                        match shaper.on_event(other) {
                            Ok(shaped_bars) => {
                                for shaped in shaped_bars {
                                    if let Some(summary) = state.consume(shaped, config)? {
                                        deliver_summary(&summaries, summary).await;
                                    }
                                }
                            }
                            Err(e) => eprintln!("live session: dropping event: {e}"),
                        }
                    }
                }
            }
            () = &mut shutdown => {
                eprintln!("live session: shutdown requested; flushing and reporting");
                break;
            }
        }
    }

    // Graceful teardown: whatever is still in flight becomes the final bar.
    for shaped in shaper.flush()? {
        if let Some(summary) = state.consume(shaped, config)? {
            deliver_summary(&summaries, summary).await;
        }
    }

    // The day still in progress is reported too, marked partial: a session
    // stopped at 14:00 UTC has not seen midnight and should not pretend to.
    if let Some(summary) = state.finish_day(&settings.symbol) {
        deliver_summary(&summaries, summary).await;
    }

    if interruptions_before_first_bar > 0 && state.bars.is_empty() {
        eprintln!(
            "live session: {interruptions_before_first_bar} feed interruption(s) happened \
             before any bar arrived — no market data this session"
        );
    }

    let SessionState {
        session,
        bars,
        last_close,
        ..
    } = state;
    let bars_written = bars.len();
    let report = session.finish(last_close);
    let csv_path = persist_bars(config, &settings.symbol, &bars, started_at)?;
    Ok(SessionOutput {
        report,
        csv_path,
        bars_written,
    })
}

/// Hands a finished day's summary to whoever consumes `summaries` (the binary
/// prints it and forwards it to Telegram).
///
/// Strictly best-effort: a closed channel (notifier gone) is logged and the
/// session keeps trading — losing a notification must never cost market data.
async fn deliver_summary(summaries: &mpsc::Sender<DailySummary>, summary: DailySummary) {
    let day = summary.day.clone();
    let partial = summary.partial;
    match summaries.send(summary).await {
        Ok(()) => eprintln!(
            "live session: {day} summary queued for delivery{}",
            if partial { " (partial day)" } else { "" }
        ),
        Err(_closed) => eprintln!(
            "live session: no summary consumer left — the {day} summary could not be delivered"
        ),
    }
}

/// Joins a queued feed-interruption note with a shaper gap note so both land
/// on the same (first bar after the hole) trace line.
fn merge_notes(interrupt: Option<String>, jump: Option<String>) -> Option<String> {
    match (interrupt, jump) {
        (Some(interrupt), Some(jump)) => {
            Some(format!("{interrupt} {}", jump.trim_start_matches(' ')))
        }
        (Some(interrupt), None) => Some(interrupt),
        (None, jump) => jump,
    }
}

/// The live mock-trade log: one line per closed paper trade, with the size
/// that would have been put down, the fees it would have paid and the account
/// state afterwards. Stdout, so it pipes cleanly alongside the final report.
fn log_mock_trade(trade: &ClosedTrade, bar_ts: SystemTime, config: &Config, state: BarState) {
    println!(
        "[{}] MOCK TRADE #{} {} {} x{} entry={} ({}) -> exit={} ({}) | \
         committed={:.2} fees={:.4} ({} bps/side) gross P/L={:+.2} net P/L={:+.2} | \
         cash={:.2} equity={:.2}",
        utc_stamp(bar_ts),
        trade.index,
        if trade.side_is_long { "LONG " } else { "SHORT" },
        config.symbol,
        config.quantity,
        trade.entry_price,
        trade.entry_day,
        trade.exit_price,
        trade.exit_day,
        trade.investment,
        trade.fees_paid,
        config.trade_fee_bps,
        trade.gross_pl,
        trade.net_pl,
        state.available,
        state.equity,
    );
}

/// Writes the session's bars to `<live_csv_dir>/live-<SYMBOL>-<UTC stamp>.csv`
/// in the format [`crate::csv`] reads, so the session can be re-replayed
/// offline and reproduce exactly. Skipped (returning `None`) when the session
/// saw no bars.
fn persist_bars(
    config: &Config,
    symbol: &str,
    bars: &[Bar],
    started_at: SystemTime,
) -> Result<Option<PathBuf>, Error> {
    if bars.is_empty() {
        return Ok(None);
    }
    let path = session_csv_path(&config.live_csv_dir, symbol, started_at);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::MarketData(format!("cannot create {}: {e}", parent.display())))?;
    }
    csv::save_bars(bars.iter().copied(), &path)?;
    Ok(Some(path))
}

/// The CSV path for a session started at `start`: deterministic given the
/// inputs (no clock reads inside), so tests can pin it.
fn session_csv_path(dir: &str, symbol: &str, start: SystemTime) -> PathBuf {
    // `replace` rather than slicing: `string_slice`/`indexing_slicing` are
    // deny lints outside tests.
    let stamp = utc_stamp(start).replace(['-', ':'], "");
    Path::new(dir).join(format!(
        "live-{}-{stamp}.csv",
        symbol.trim().to_ascii_uppercase()
    ))
}

/// Human label for the configured feed channel.
const fn channel_label(channel: FeedChannel) -> &'static str {
    match channel {
        FeedChannel::Minute => "minute windows",
        FeedChannel::Ticks => "ticks (1s bars)",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)] // bounded test offsets

    use super::*;
    use crate::feed::{TickTrade, WindowUpdate};

    /// A window with sane OHLC around `close`, one minute long, for `AAPL`.
    fn window_update(start_ms: u64, close: f64) -> WindowUpdate {
        WindowUpdate {
            symbol: "AAPL".into(),
            start_ms,
            end_ms: Some(start_ms + MINUTE_MS),
            open: close - 1.0,
            high: close + 1.0,
            low: close - 2.0,
            close,
            volume: 1_000.0,
        }
    }

    /// The same window wrapped as an event.
    fn window(start_ms: u64, close: f64) -> RawEvent {
        RawEvent::Window(window_update(start_ms, close))
    }

    /// Strips a live-only annotation (a gap note appended after `equity=`) so
    /// a live trace line can be compared with its offline re-replay twin,
    /// which has no feed gaps to report.
    fn strip_live_note(line: &str) -> String {
        match line.split_once("   [") {
            Some((before, _note)) => before.to_string(),
            None => line.to_string(),
        }
    }

    fn tick(ts_ms: u64, price: f64, size: f64) -> RawEvent {
        RawEvent::Tick(TickTrade {
            symbol: "AAPL".into(),
            price,
            ts_ms,
            size,
        })
    }

    /// A config with a temp CSV dir; everything else at defaults.
    fn test_config(dir: &Path) -> Config {
        Config {
            live_csv_dir: dir.display().to_string(),
            ..Config::default()
        }
    }

    fn settings() -> FeedSettings {
        FeedSettings::for_stocks(
            "socket.massive.com",
            "k".into(),
            "AAPL",
            FeedChannel::Minute,
        )
    }

    #[test]
    fn minute_windows_are_held_until_the_next_one_starts() {
        let mut shaper = BarShaper::new_minute();
        // First sight of a window: nothing emitted (it may still be updating).
        assert!(shaper.on_event(window(0, 100.0)).unwrap().is_empty());
        // Re-emission of the *same* window with fresher numbers: still nothing.
        let updated = RawEvent::Window(WindowUpdate {
            close: 105.0,
            high: 106.0,
            ..window_update(0, 100.0)
        });
        assert!(shaper.on_event(updated).unwrap().is_empty());
        // The next window closes the held one — with its FINAL numbers.
        let closed = shaper.on_event(window(MINUTE_MS, 110.0)).unwrap();
        assert_eq!(closed.len(), 1);
        assert!((closed[0].bar.close() - 105.0).abs() < f64::EPSILON);
        assert!((closed[0].bar.high() - 106.0).abs() < f64::EPSILON);
        assert!(
            closed[0].gap_note.is_none(),
            "adjacent minutes are not a gap"
        );
        // Flushing emits the window still in flight.
        let flushed = shaper.flush().unwrap();
        assert_eq!(flushed.len(), 1);
        assert!((flushed[0].bar.close() - 110.0).abs() < f64::EPSILON);
        assert!(shaper.flush().unwrap().is_empty());
    }

    #[test]
    fn stale_windows_are_dropped_and_never_reopen_a_bar() {
        let mut shaper = BarShaper::new_minute();
        shaper.on_event(window(0, 100.0)).unwrap();
        shaper.on_event(window(MINUTE_MS, 110.0)).unwrap();
        // A window older than the one in flight is out-of-order: ignored.
        assert!(shaper.on_event(window(0, 999.0)).unwrap().is_empty());
        let flushed = shaper.flush().unwrap();
        assert!((flushed[0].bar.close() - 110.0).abs() < f64::EPSILON);
    }

    #[test]
    fn missing_minutes_produce_a_gap_note_on_the_next_bar() {
        let mut shaper = BarShaper::new_minute();
        shaper.on_event(window(0, 100.0)).unwrap();
        shaper.on_event(window(MINUTE_MS, 110.0)).unwrap(); // closes the 0 window
                                                            // Jump to minute 4: minutes 2 and 3 never arrived. The hole is the
                                                            // 120s between the end of the last window seen (minute 1 ends at
                                                            // t=120s) and the start of the next one (t=240s), and it is reported
                                                            // on the bar that *follows* the hole.
        shaper.on_event(window(4 * MINUTE_MS, 120.0)).unwrap();
        let flushed = shaper.flush().unwrap();
        let note = flushed[0].gap_note.as_deref().unwrap_or_default();
        assert!(note.contains("feed gap"), "{note}");
        assert!(note.contains("~120s"), "{note}");
        assert!(note.contains("missing windows"), "{note}");
    }

    #[test]
    fn ticks_accumulate_within_a_second_and_close_on_the_next() {
        let mut shaper = BarShaper::new_ticks();
        assert!(shaper
            .on_event(tick(1_000, 100.0, 10.0))
            .unwrap()
            .is_empty());
        assert!(shaper.on_event(tick(1_200, 103.0, 5.0)).unwrap().is_empty());
        assert!(shaper.on_event(tick(1_900, 99.0, 7.0)).unwrap().is_empty());
        // Next second closes the bucket: open=first, close=last, high/low
        // widened, volume summed, adjacent second → no gap note.
        let closed = shaper.on_event(tick(2_000, 101.0, 1.0)).unwrap();
        assert_eq!(closed.len(), 1);
        let bar = closed[0].bar;
        assert!((bar.open() - 100.0).abs() < f64::EPSILON);
        assert!((bar.close() - 99.0).abs() < f64::EPSILON);
        assert!((bar.high() - 103.0).abs() < f64::EPSILON);
        assert!((bar.low() - 99.0).abs() < f64::EPSILON);
        assert!((bar.volume() - 22.0).abs() < f64::EPSILON);
        assert_eq!(unix_secs(bar.timestamp()), 1);
        assert!(closed[0].gap_note.is_none());
        // Flush emits the open bucket.
        let flushed = shaper.flush().unwrap();
        assert!((flushed[0].bar.close() - 101.0).abs() < f64::EPSILON);
    }

    #[test]
    fn skipped_seconds_produce_a_tick_gap_note() {
        let mut shaper = BarShaper::new_ticks();
        shaper.on_event(tick(1_000, 100.0, 1.0)).unwrap();
        let closed = shaper.on_event(tick(4_000, 101.0, 1.0)).unwrap();
        let note = closed[0].gap_note.as_deref().unwrap_or_default();
        assert!(note.contains("~3s"), "{note}");
        assert!(note.contains("missing ticks"), "{note}");
    }

    #[test]
    fn stale_ticks_are_dropped_and_never_replace_the_active_bucket() {
        let mut shaper = BarShaper::new_ticks();
        // Second 1 accumulates two trades…
        shaper.on_event(tick(1_000, 100.0, 1.0)).unwrap();
        shaper.on_event(tick(1_500, 101.0, 2.0)).unwrap();
        // …an out-of-order arrival from second 0 must not close the bucket
        // into an older bar or replace it.
        assert!(shaper.on_event(tick(900, 42.0, 7.0)).unwrap().is_empty());
        let flushed = shaper.flush().unwrap();
        assert_eq!(flushed.len(), 1);
        let bar = flushed[0].bar;
        // The held second-1 bucket is intact: open/close/high/low/volume are
        // exactly the two in-second trades, not the stale one.
        assert_eq!(
            unix_secs(bar.timestamp()),
            1,
            "the stale tick must not move the active second"
        );
        assert!((bar.open() - 100.0).abs() < f64::EPSILON);
        assert!((bar.close() - 101.0).abs() < f64::EPSILON);
        assert!((bar.high() - 101.0).abs() < f64::EPSILON);
        assert!((bar.low() - 100.0).abs() < f64::EPSILON);
        assert!((bar.volume() - 3.0).abs() < f64::EPSILON,);
    }

    #[test]
    fn cross_mode_events_and_interruptions_make_no_bars() {
        let mut minute = BarShaper::new_minute();
        assert!(minute.on_event(tick(1_000, 100.0, 1.0)).unwrap().is_empty());
        assert!(minute
            .on_event(RawEvent::FeedInterrupted)
            .unwrap()
            .is_empty());
        let mut ticks = BarShaper::new_ticks();
        assert!(ticks.on_event(window(0, 100.0)).unwrap().is_empty());
        assert!(ticks
            .on_event(RawEvent::FeedInterrupted)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn unusable_values_are_rejected_not_silently_booked() {
        let mut shaper = BarShaper::new_ticks();
        let bad_price = tick(1_000, f64::NAN, 1.0);
        assert!(matches!(
            shaper.on_event(bad_price),
            Err(Error::MarketData(_))
        ));
        let negative = tick(1_000, -5.0, 1.0);
        assert!(matches!(
            shaper.on_event(negative),
            Err(Error::MarketData(_))
        ));

        let mut minute = BarShaper::new_minute();
        let bad_window = RawEvent::Window(WindowUpdate {
            close: f64::INFINITY,
            ..window_update(0, 100.0)
        });
        assert!(matches!(
            minute.on_event(bad_window),
            Err(Error::MarketData(_))
        ));
    }

    #[test]
    fn timestamps_render_as_utc_and_csv_names_are_stable() {
        // 2021-01-19T18:00:00Z — the fixture date used across the bundle.
        let ts = ms_to_systemtime(1_611_079_200_000);
        assert_eq!(utc_stamp(ts), "2021-01-19T18:00:00Z");
        let path = session_csv_path("./sessions", " aapl ", ts);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("live-AAPL-20210119T180000Z.csv")
        );
        assert_eq!(path.parent().and_then(|p| p.to_str()), Some("./sessions"));
    }

    #[test]
    fn gap_notes_merge_without_losing_either_half() {
        let merged = merge_notes(
            Some("   [feed interrupted; reconnecting]".to_string()),
            Some("   [feed gap: ~120s of missing windows before this bar]".to_string()),
        )
        .unwrap_or_default();
        assert!(merged.contains("interrupted"), "{merged}");
        assert!(merged.contains("~120s"), "{merged}");
        assert!(merge_notes(None, None).is_none());
        assert!(merge_notes(Some("a".into()), None).is_some());
    }

    /// Runs a session with a summary consumer attached, returning both the
    /// session output and every [`DailySummary`] it delivered.
    ///
    /// The session drops its sender when it returns, so the drain loop below
    /// terminates on its own — no test has to know how many days to expect.
    async fn run_collecting(
        config: &Config,
        rx: mpsc::Receiver<RawEvent>,
        shutdown: impl Future<Output = ()>,
    ) -> (SessionOutput, Vec<DailySummary>) {
        let (sum_tx, mut sum_rx) = mpsc::channel(SUMMARY_CHANNEL_SIZE);
        let feed_settings = settings();
        let output = run_session(config, &feed_settings, rx, shutdown, sum_tx)
            .await
            .unwrap();
        let mut summaries = Vec::new();
        while let Some(summary) = sum_rx.recv().await {
            summaries.push(summary);
        }
        (output, summaries)
    }

    /// 2021-01-19T23:58:00Z / 23:59:00Z in Unix ms — the last two minutes of
    /// the fixture day used across the bundle.
    const DAY1_2358_MS: u64 = 1_611_100_680_000;
    const DAY1_2359_MS: u64 = 1_611_100_740_000;
    /// 2021-01-20T00:00:00Z / 00:01:00Z — across the UTC midnight.
    const DAY2_0000_MS: u64 = 1_611_100_800_000;
    const DAY2_0001_MS: u64 = 1_611_100_860_000;
    const MS_PER_DAY: u64 = 86_400_000;

    /// Threshold 1 so a single close move trades, into a temp CSV dir.
    fn trading_config(dir: &Path) -> Config {
        Config {
            consecutive_closes_threshold: 1,
            live_csv_dir: dir.display().to_string(),
            ..Config::default()
        }
    }

    /// Default config, ticks channel (the shaper mode exercised by the
    /// session-loop resilience tests), temp CSV dir.
    fn tick_channel_config(dir: &Path) -> Config {
        Config {
            live_feed_channel: FeedChannel::Ticks,
            live_csv_dir: dir.display().to_string(),
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn a_session_shapes_events_into_a_report_and_persists_them() {
        let dir = std::env::temp_dir().join("price-action-live-test-session");
        std::fs::create_dir_all(&dir).unwrap();
        let config = test_config(&dir);

        let (tx, rx) = mpsc::channel::<RawEvent>(EVENT_CHANNEL_SIZE);
        // Enough bars for the default threshold (3 consecutive closes) to fire.
        let closes = [100.0, 101.0, 102.0, 103.0, 104.0, 100.0, 99.0, 98.0, 97.0];
        for (i, close) in closes.iter().enumerate() {
            let start = u64::try_from(i).unwrap() * MINUTE_MS;
            tx.send(window(start, *close)).await.unwrap();
        }
        // A hole, then one more window so the held one is emitted.
        tx.send(window(20 * MINUTE_MS, 96.0)).await.unwrap();
        tx.send(window(21 * MINUTE_MS, 95.0)).await.unwrap();
        drop(tx); // closes the channel → the session ends

        let (output, summaries) = run_collecting(&config, rx, std::future::pending::<()>()).await;

        // 11 windows in, 11 bars out (the last one is flushed at shutdown).
        assert_eq!(output.report.bars, 11);
        assert_eq!(output.bars_written, 11);
        // Every bar lands in the same UTC day, so exactly one summary — the
        // partial one emitted at shutdown.
        assert_eq!(summaries.len(), 1, "{summaries:?}");
        assert_eq!(summaries[0].day, "1970-01-01");
        assert!(summaries[0].partial);
        assert_eq!(summaries[0].bars, 11);
        // The jump from minute 9 to minute 20 is reported honestly.
        assert!(
            output
                .report
                .trace_lines
                .iter()
                .any(|line| line.contains("feed gap")),
            "expected a gap note in {:?}",
            output.report.trace_lines
        );
        // Paper accounting ran: a session total exists and is finite.
        assert!(output.report.session_totals.final_equity.is_finite());

        // The persisted CSV re-replays to the same numbers (the whole point).
        let csv_path = output.csv_path.clone().unwrap();
        let bars = csv::load_bars(&csv_path).unwrap();
        assert_eq!(bars.len(), 11);
        let rerun = crate::replay::replay_bars(&config, &bars).unwrap();
        // Byte-identical apart from the live-only gap annotation.
        let stripped: Vec<String> = output
            .report
            .trace_lines
            .iter()
            .map(|line| strip_live_note(line))
            .collect();
        assert_eq!(rerun.trace_lines, stripped);
        assert_eq!(rerun.entries, output.report.entries);
        assert_eq!(rerun.closed_trades.len(), output.report.closed_trades.len());
        std::fs::remove_file(&csv_path).unwrap();
    }

    #[tokio::test]
    async fn unusable_events_are_dropped_and_the_session_continues() {
        let dir = std::env::temp_dir().join("price-action-live-test-badevent");
        std::fs::create_dir_all(&dir).unwrap();
        let config = tick_channel_config(&dir);

        let (tx, rx) = mpsc::channel::<RawEvent>(EVENT_CHANNEL_SIZE);
        // A tick whose `consume` booking succeeds…
        tx.send(tick(1_000, 100.0, 1.0)).await.unwrap();
        // …an unusable event (non-finite price) must not end the session…
        tx.send(tick(2_000, f64::NAN, 1.0)).await.unwrap();
        // …and later, valid ticks are still shaped and booked.
        tx.send(tick(3_000, 101.0, 1.0)).await.unwrap();
        tx.send(tick(4_000, 102.0, 1.0)).await.unwrap();
        drop(tx); // closes the channel → the session ends

        let (output, summaries) = run_collecting(&config, rx, std::future::pending::<()>()).await;
        // The three good ticks land in seconds 1, 3 and 4 — one bar each,
        // the last flushed at shutdown; the NaN tick produced none and did
        // not abort the loop.
        assert_eq!(
            output.report.bars, 3,
            "the bad event must not end the session"
        );
        assert_eq!(output.bars_written, 3);
        assert_eq!(summaries.len(), 1, "{summaries:?}");

        let csv_path = output.csv_path.clone().unwrap();
        let bars = csv::load_bars(&csv_path).unwrap();
        assert_eq!(bars.len(), 3);
        // Second 2 never held a usable trade, so its bar is absent and the
        // hole is reported, not hidden.
        let secs: Vec<i64> = bars.iter().map(|bar| unix_secs(bar.timestamp())).collect();
        assert_eq!(secs, vec![1, 3, 4]);
        assert!((bars[2].close() - 102.0).abs() < f64::EPSILON);
        assert!(
            output
                .report
                .trace_lines
                .iter()
                .any(|line| line.contains("feed gap")),
            "missing second must carry a gap note in {:?}",
            output.report.trace_lines
        );
        std::fs::remove_file(&csv_path).unwrap();
    }

    #[tokio::test]
    async fn shutdown_stops_a_session_whose_channel_is_still_open() {
        let dir = std::env::temp_dir().join("price-action-live-test-shutdown");
        std::fs::create_dir_all(&dir).unwrap();
        let config = test_config(&dir);

        let (tx, rx) = mpsc::channel::<RawEvent>(EVENT_CHANNEL_SIZE);
        tx.send(window(0, 100.0)).await.unwrap();
        tx.send(window(MINUTE_MS, 101.0)).await.unwrap();
        // Sender stays alive, so only the shutdown future can end the
        // session. Yielding first keeps that branch Pending while the buffered
        // events are still ready, so the events are consumed before the stop
        // lands — deterministic without racing the scheduler.
        let shutdown = async {
            for _ in 0..3 {
                tokio::task::yield_now().await;
            }
        };
        let (output, summaries) = run_collecting(&config, rx, shutdown).await;
        // Minute 0's window closed when minute 1 arrived; minute 1's window is
        // still in flight and must be flushed rather than dropped.
        assert_eq!(output.report.bars, 2, "the held window is flushed on stop");
        assert_eq!(output.bars_written, 2);
        // The day in progress is reported on stop, and flagged as partial.
        assert_eq!(summaries.len(), 1, "{summaries:?}");
        assert!(summaries[0].partial);
        drop(tx);
        if let Some(path) = output.csv_path {
            std::fs::remove_file(path).unwrap();
        }
    }

    #[tokio::test]
    async fn interruptions_before_any_bar_are_counted_not_annotated() {
        let dir = std::env::temp_dir().join("price-action-live-test-nodata");
        std::fs::create_dir_all(&dir).unwrap();
        let config = test_config(&dir);

        let (tx, rx) = mpsc::channel::<RawEvent>(EVENT_CHANNEL_SIZE);
        tx.send(RawEvent::FeedInterrupted).await.unwrap();
        tx.send(RawEvent::FeedInterrupted).await.unwrap();
        drop(tx);
        let (output, summaries) = run_collecting(&config, rx, std::future::pending::<()>()).await;
        assert_eq!(output.report.bars, 0);
        assert!(output.csv_path.is_none(), "nothing to persist");
        assert!(output.report.trace_lines.is_empty());
        assert!(summaries.is_empty(), "no bars means no day to summarize");
    }

    #[tokio::test]
    async fn a_utc_day_boundary_emits_exactly_one_summary_per_day() {
        let dir = std::env::temp_dir().join("price-action-live-test-day-boundary");
        std::fs::create_dir_all(&dir).unwrap();
        let config = trading_config(&dir);

        let (tx, rx) = mpsc::channel::<RawEvent>(EVENT_CHANNEL_SIZE);
        // 23:58 flat (no previous close yet), 23:59 enters long, 00:00 holds,
        // 00:01 reverses to short — one UTC midnight crossed mid-session.
        for (start_ms, close) in [
            (DAY1_2358_MS, 100.0),
            (DAY1_2359_MS, 101.0),
            (DAY2_0000_MS, 102.0),
            (DAY2_0001_MS, 100.0),
        ] {
            tx.send(window(start_ms, close)).await.unwrap();
        }
        drop(tx);

        let (output, summaries) = run_collecting(&config, rx, std::future::pending::<()>()).await;

        // Exactly one summary per day: day 1 fired at the boundary, day 2 at
        // shutdown for the day still in progress.
        assert_eq!(summaries.len(), 2, "{summaries:?}");
        let (day1, day2) = (&summaries[0], &summaries[1]);

        assert_eq!(day1.day, "2021-01-19");
        assert!(!day1.partial, "day 1 closed at the boundary");
        assert_eq!(day1.symbol, "AAPL");
        assert_eq!(day1.bars, 2);
        assert_eq!(day1.entries, 1, "the long opened at 23:59");
        assert_eq!(day1.exits, 0, "nothing closed on day 1");
        assert!(day1.fees_paid.abs() < f64::EPSILON, "{}", day1.fees_paid);
        assert!(
            day1.net_realized_pl.abs() < f64::EPSILON,
            "{}",
            day1.net_realized_pl
        );
        assert_eq!(day1.open_position, Signal::Long);

        assert_eq!(day2.day, "2021-01-20");
        assert!(day2.partial, "day 2 was cut short by shutdown");
        assert_eq!(day2.bars, 2);
        assert_eq!(day2.entries, 1, "the reversal opened a short");
        assert_eq!(day2.exits, 1, "the reversal closed the long");
        assert!(day2.fees_paid > 0.0, "both legs of the round trip pay");
        assert!(day2.net_realized_pl < 0.0, "the long lost 101 → 100");
        assert_eq!(day2.open_position, Signal::Short);

        // The money is the account's own, not a second implementation: it
        // reconciles with the report's closed-trade row and session totals.
        let trade = output
            .report
            .closed_trades
            .first()
            .expect("one closed trade");
        assert_eq!(trade.entry_day, day1.day);
        assert_eq!(trade.exit_day, day2.day);
        assert!((day2.fees_paid - trade.fees_paid).abs() < 1e-9);
        assert!((day2.net_realized_pl - trade.net_pl).abs() < 1e-9);
        assert!((day2.ending_cash - output.report.session_totals.final_available).abs() < 1e-9);
        assert!((day2.ending_equity - output.report.session_totals.final_equity).abs() < 1e-9);

        // ...and with the report's own per-UTC-day roll-up, so the two views of
        // the same day cannot drift apart.
        let per_day = &output.report.per_day_totals;
        assert_eq!(per_day.len(), 2, "{per_day:?}");
        assert_eq!(per_day[0].day, day1.day);
        assert_eq!(per_day[0].entries, day1.entries);
        assert_eq!(per_day[0].exits, day1.exits);
        assert_eq!(per_day[1].day, day2.day);
        assert_eq!(per_day[1].entries, day2.entries);
        assert_eq!(per_day[1].exits, day2.exits);
        assert!((per_day[1].net_realized_pl - day2.net_realized_pl).abs() < 1e-9);

        if let Some(path) = output.csv_path {
            std::fs::remove_file(path).unwrap();
        }
    }

    #[tokio::test]
    async fn each_utc_day_boundary_fires_exactly_once() {
        // Three UTC days in one session → three summaries in chronological
        // order: two at boundaries, one partial at shutdown. No duplicates,
        // no day skipped.
        let dir = std::env::temp_dir().join("price-action-live-test-three-days");
        std::fs::create_dir_all(&dir).unwrap();
        let config = trading_config(&dir);

        let (tx, rx) = mpsc::channel::<RawEvent>(EVENT_CHANNEL_SIZE);
        for day in 0..3u64 {
            let base = DAY1_2358_MS + day * MS_PER_DAY;
            for (offset, close) in [(0u64, 100.0), (MINUTE_MS, 101.0)] {
                tx.send(window(base + offset, close)).await.unwrap();
            }
        }
        drop(tx);

        let (output, summaries) = run_collecting(&config, rx, std::future::pending::<()>()).await;
        let days: Vec<&str> = summaries.iter().map(|s| s.day.as_str()).collect();
        assert_eq!(days, ["2021-01-19", "2021-01-20", "2021-01-21"]);
        assert!(!summaries[0].partial, "day 1 closed at a boundary");
        assert!(!summaries[1].partial, "day 2 closed at a boundary");
        assert!(summaries[2].partial, "day 3 was cut short by shutdown");
        assert_eq!(summaries.iter().map(|s| s.bars).sum::<usize>(), 6);
        assert_eq!(output.report.bars, 6);

        if let Some(path) = output.csv_path {
            std::fs::remove_file(path).unwrap();
        }
    }

    #[tokio::test]
    async fn a_quiet_day_still_reports_its_balance() {
        // A threshold nothing can reach: no trade at all. The day still gets a
        // summary with zero activity and an unchanged balance, which is the
        // honest answer — silence is not the same as "no report".
        let dir = std::env::temp_dir().join("price-action-live-test-quiet-day");
        std::fs::create_dir_all(&dir).unwrap();
        let config = Config {
            consecutive_closes_threshold: 50,
            live_csv_dir: dir.display().to_string(),
            ..Config::default()
        };

        let (tx, rx) = mpsc::channel::<RawEvent>(EVENT_CHANNEL_SIZE);
        tx.send(window(DAY1_2358_MS, 100.0)).await.unwrap();
        tx.send(window(DAY1_2359_MS, 101.0)).await.unwrap();
        drop(tx);

        let (output, summaries) = run_collecting(&config, rx, std::future::pending::<()>()).await;
        assert_eq!(summaries.len(), 1, "{summaries:?}");
        let only = &summaries[0];
        assert_eq!(only.day, "2021-01-19");
        assert_eq!(only.bars, 2);
        assert_eq!(only.entries, 0);
        assert_eq!(only.exits, 0);
        assert!(only.fees_paid.abs() < f64::EPSILON);
        assert!(only.net_realized_pl.abs() < f64::EPSILON);
        assert!((only.ending_cash - config.starting_balance).abs() < f64::EPSILON);
        assert!((only.ending_equity - config.starting_balance).abs() < f64::EPSILON);
        assert_eq!(only.open_position, Signal::Flat);

        if let Some(path) = output.csv_path {
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn a_rendered_summary_names_the_day_symbol_and_stays_paper_only() {
        let summary = DailySummary {
            day: "2021-01-19".into(),
            symbol: "AAPL".into(),
            bars: 390,
            entries: 2,
            exits: 1,
            fees_paid: 1.234_5,
            net_realized_pl: -12.5,
            ending_cash: 9_987.5,
            ending_equity: 9_990.25,
            open_position: Signal::Short,
            partial: false,
        };
        let text = summary.render();
        assert!(text.contains("2021-01-19"), "{text}");
        assert!(text.contains("AAPL"), "{text}");
        assert!(text.contains("bars: 390"), "{text}");
        assert!(text.contains("entries: 2"), "{text}");
        assert!(text.contains("exits: 1"), "{text}");
        assert!(text.contains("fees paid: 1.23"), "{text}");
        assert!(text.contains("-12.50"), "{text}");
        assert!(text.contains("9987.50"), "{text}");
        assert!(text.contains("9990.25"), "{text}");
        assert!(text.contains("Short"), "{text}");
        assert!(!text.contains("partial"), "{text}");
        // The framing the repo's disclaimer requires: this is a paper account,
        // never a recommendation or a claim of profit.
        assert!(text.contains("paper account only"), "{text}");
        assert!(text.contains("no orders were placed"), "{text}");
        assert!(text.contains("not financial advice"), "{text}");

        let partial = DailySummary {
            partial: true,
            ..summary
        };
        assert!(partial.render().contains("partial"), "{}", partial.render());
    }
}
