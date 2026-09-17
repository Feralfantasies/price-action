//! Funded paper accounting for replayed bars.
//!
//! The engine loop decides `Flat`/`Long`/`Short` per bar; this module prices
//! those decisions as if a small paper account (see `Config::starting_balance`)
//! were trading them at the **close of each bar**: capital committed or
//! released per trade, fees in basis points on each side's notional, realized
//! P/L per closed trade, and roll-ups by UTC calendar day plus the whole
//! session. Replay stays read-only by design: no venue is ever touched.
//!
//! Arithmetic note: every value here is a finite, display-grade money or date
//! quantity whose operands are validated upstream (config and `Bar`), so the
//! workspace-wide `arithmetic_side_effects` warning is allowed per-site —
//! exactly as `csv.rs` documents for its own bounded date arithmetic.

use crate::{market::Bar, strategy::Signal};

/// One closed paper trade with its realized results (money fields net of
/// fees where labelled).
#[derive(Debug, Clone, PartialEq)]
pub struct ClosedTrade {
    /// 1-based index in exit order.
    pub index: usize,
    /// Whether the position was long (vs short) while open.
    pub side_is_long: bool,
    /// 0-based bar index of the entry bar (the signal's bar).
    pub entry_bar_index: usize,
    /// Close price the entry executed at.
    pub entry_price: f64,
    /// Entry bar Unix seconds.
    pub entry_ts_secs: i64,
    /// UTC calendar date (`YYYY-MM-DD`) of the entry day.
    pub entry_day: String,
    /// 0-based bar index of the exit bar.
    pub exit_bar_index: usize,
    /// Close price the exit executed at.
    pub exit_price: f64,
    /// Exit bar Unix seconds.
    pub exit_ts_secs: i64,
    /// UTC calendar date (`YYYY-MM-DD`) of the exit day.
    pub exit_day: String,
    /// `quantity * entry_price`, capital committed at entry.
    pub investment: f64,
    /// Entry and exit fees paid for this trade (each side's notional in bps).
    pub fees_paid: f64,
    /// P/L ignoring fees: `(exit - entry) * q` long, else `(entry - exit) * q`.
    pub gross_pl: f64,
    /// Realized P/L net of `fees_paid`; this is what rolls up into totals.
    pub net_pl: f64,
}

/// Realized activity for one UTC calendar day (only days that saw an entry or
/// exit appear).
#[derive(Debug, Clone, PartialEq)]
pub struct DayTotal {
    /// `YYYY-MM-DD` in UTC.
    pub day: String,
    /// Trades entered during the day.
    pub entries: usize,
    /// Trades exited during the day.
    pub exits: usize,
    /// Sum of net P/L over trades that **exited** during the day.
    pub net_realized_pl: f64,
}

/// Session-wide roll-up across a whole replay run.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTotals {
    /// Starting funds before any trade.
    pub starting_balance: f64,
    /// Funds free after the last bar (open long capital still unavailable).
    pub final_available: f64,
    /// Equity including a mark of any position open at `final_close`.
    pub final_equity: f64,
    /// Fees paid on every entry and exit leg.
    pub total_fees_paid: f64,
    /// Sum of net P/L over all closed trades.
    pub total_net_pl: f64,
}

/// Account funds right after a bar is priced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarState {
    /// Funds currently free (open-position capital/collateral is not free).
    pub available: f64,
    /// `available` plus the open position's mark at the bar's close.
    pub equity: f64,
}

/// The complete result of pricing one strategy signal for the account.
#[derive(Debug, Clone)]
pub struct BarOutcome {
    /// Funds/equity after this bar (equity marked at its close).
    pub state: BarState,
    /// Position actually held afterwards; differs from the raw strategy signal
    /// only when an unfunded entry was skipped.
    pub effective_signal: Signal,
    /// True if this bar's desired entry was skipped for lack of funds — render
    /// as an `(insufficient funds)` note on the trace line.
    pub entry_skipped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Side {
    Long,
    Short,
}

/// A funded paper account. Every position sizes `quantity` units; each leg to
/// or from it pays a fee in basis points on that leg's notional.
#[derive(Debug)]
pub struct PaperAccount {
    quantity: f64,
    starting_balance: f64,
    /// Fee rate as a fraction (bps / `10_000`); zero bps disables fees.
    fee_rate: f64,
    available: f64,
    open_side: Option<Side>,
    entry_price: f64,
    entry_notional: f64,
    entry_fee: f64,
    entry_bar_index: usize,
    entry_ts_secs: i64,
    entry_day: String,
    closed_trades: Vec<ClosedTrade>,
    total_fees_paid: f64,
    /// Running realized net P/L in the chronological close sequence; this is
    /// the canonical session total (see `session_totals`). Keeping one
    /// addition order for day rows and the session row makes the displayed
    /// figures coherent by construction on multi-day files — floating-point
    /// sums are order-sensitive at sub-cent scale, so two different orders
    /// can drift by a reported cent.
    canonical_net_running: f64,
    /// Same idea for fees paid across all closed legs.
    canonical_fees_running: f64,
}

impl PaperAccount {
    /// Creates the account. Reaching this constructor is gated by config
    /// validation (`starting_balance > 0` and finite), so no re-checks here.
    #[must_use]
    pub fn new(quantity: u32, starting_balance: f64, trade_fee_bps: u32) -> Self {
        Self {
            quantity: f64::from(quantity),
            starting_balance,
            fee_rate: f64::from(trade_fee_bps) / 10_000.0,
            available: starting_balance,
            open_side: None,
            entry_price: 0.0,
            entry_notional: 0.0,
            entry_fee: 0.0,
            entry_bar_index: 0,
            entry_ts_secs: 0,
            entry_day: String::new(),
            closed_trades: Vec::new(),
            total_fees_paid: 0.0,
            canonical_net_running: 0.0,
            canonical_fees_running: 0.0,
        }
    }

    /// Whether a position is currently open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open_side.is_some()
    }

    /// Funds currently free (open-position capital/collateral unavailable).
    #[must_use]
    pub const fn available(&self) -> f64 {
        self.available
    }

    /// Fees paid on every executed entry and exit leg so far.
    #[must_use]
    pub const fn total_fees_paid(&self) -> f64 {
        self.total_fees_paid
    }

    /// Closed trades in chronological exit order (immutable view).
    #[must_use]
    pub fn closed_trades(&self) -> &[ClosedTrade] {
        &self.closed_trades
    }

    fn held_signal(&self) -> Signal {
        self.open_side.map_or(Signal::Flat, |side| match side {
            Side::Long => Signal::Long,
            Side::Short => Signal::Short,
        })
    }

    /// Prices the account's move to the strategy's `next` signal, executed at
    /// `bar`'s close.
    ///
    /// # Rules
    /// - Exit: if the position held no longer matches the new signal (flat
    ///   target or a reversal), it exits **first** at this same close —
    ///   committed capital returns, signed P/L is credited, and an exit fee on
    ///   this bar's notional is charged.
    /// - Entry: then, if flat and the signal wants long/short, entering costs
    ///   `quantity * close` plus its entry fee. When that exceeds available
    ///   funds the entry is **skipped** (the bar effectively stays flat; after
    ///   a reversal the old leg's exit stands and no new position opens) — the
    ///   account never reports negative funds.
    /// - The outcome carries post-bar funds/equity (any still-open position is
    ///   marked at this close), the effective signal, and a skip flag for trace
    ///   notes. This account tracks its own held position, so transitions
    ///   (hold / exit / enter / reverse) are derived from it — `next` only has
    ///   to be the strategy's raw per-bar signal.
    #[allow(clippy::arithmetic_side_effects)] // bounded money math
    pub fn on_bar(&mut self, next: &Signal, bar_index: usize, bar: &Bar) -> BarOutcome {
        let close = bar.close();
        let ts_secs = unix_secs(bar.timestamp());

        // Exit first (a reversal exits the old leg at this same close).
        if self.is_open() && self.held_signal() != *next {
            self.exit_position(bar_index, ts_secs, close);
        }

        let mut entry_skipped = false;
        if !self.is_open() && next != &Signal::Flat {
            let notional = self.quantity * close;
            let fee = notional * self.fee_rate;
            if notional + fee > self.available {
                entry_skipped = true;
            } else {
                self.apply_costs(notional, fee);
                self.open_side = Some(match next {
                    Signal::Long => Side::Long,
                    _ => Side::Short,
                });
                self.entry_price = close;
                self.entry_notional = notional;
                self.entry_fee = fee;
                self.entry_bar_index = bar_index;
                self.entry_ts_secs = ts_secs;
                self.entry_day = utc_day(ts_secs);
            }
        }

        BarOutcome {
            state: BarState {
                available: self.available,
                equity: self.mark_equity(close),
            },
            effective_signal: self.held_signal(),
            entry_skipped,
        }
    }

    /// Total account value at `price`, marking any open position there.
    ///
    /// Longs mark to market (`available` plus shares at price). Shorts mark
    /// the locked-in collateral plus unrealized P/L — a short's available is
    /// not inflated by full-notional capital that never existed, so its mark
    /// must not drop by a full notional while open.
    #[allow(clippy::arithmetic_side_effects)] // bounded money math
    #[must_use]
    pub fn mark_equity(&self, price: f64) -> f64 {
        match self.open_side {
            Some(Side::Long) => {
                let long_mark = self.quantity * price; // free funds + shares at price
                self.available + long_mark
            }
            Some(Side::Short) => {
                let short_pl = (self.entry_price - price) * self.quantity;
                // Collateral stays reserved while the lock lasts:
                self.available + self.entry_notional + short_pl
            }
            None => self.available,
        }
    }

    /// Per-UTC-day roll-ups: entries started, exits completed, and net realized
    /// P/L of trades that **exited** the day. ISO date strings sort
    /// chronologically.
    #[allow(clippy::arithmetic_side_effects)] // bounded money math
    #[must_use]
    pub fn per_day_totals(&self) -> Vec<DayTotal> {
        let mut days: Vec<DayTotal> = Vec::new();
        for trade in &self.closed_trades {
            if days.iter().all(|d| d.day != trade.exit_day) {
                days.push(DayTotal {
                    day: trade.exit_day.clone(),
                    entries: 0,
                    exits: 0,
                    net_realized_pl: 0.0,
                });
            }
        }
        for trade in &self.closed_trades {
            if let Some(d) = days.iter_mut().find(|d| d.day == trade.entry_day) {
                d.entries += 1;
            } else {
                days.push(DayTotal {
                    day: trade.entry_day.clone(),
                    entries: 1,
                    exits: 0,
                    net_realized_pl: 0.0,
                });
            }
        }
        for trade in &self.closed_trades {
            if let Some(d) = days.iter_mut().find(|d| d.day == trade.exit_day) {
                d.exits += 1;
                d.net_realized_pl += trade.net_pl;
            }
        }
        days.sort_by(|a, b| a.day.cmp(&b.day));
        days
    }

    /// Session roll-up; `final_close` marks any still-open position.
    ///
    /// Realized P/L and fees use the canonical chronological totals (same
    /// close-ordered additions as the per-day rows), so on multi-day files
    /// they stay coherent with what the day table displays, cent for cent.
    #[allow(clippy::arithmetic_side_effects)] // bounded money math
    #[must_use]
    pub fn session_totals(&self, final_close: Option<f64>) -> SessionTotals {
        SessionTotals {
            starting_balance: self.starting_balance,
            final_available: self.available,
            final_equity: final_close.map_or(self.available, |p| self.mark_equity(p)),
            total_fees_paid: self.canonical_fees_running,
            total_net_pl: self.canonical_net_running,
        }
    }

    fn apply_costs(&mut self, notional: f64, fee: f64) {
        self.available -= notional;
        self.available -= fee;
        self.total_fees_paid += fee;
    }

    /// Closes the open position at `close`: committed capital returns, signed
    /// P/L is credited, and the exit fee (on this bar's notional) is charged.
    fn exit_position(&mut self, bar_index: usize, ts_secs: i64, close: f64) {
        if !self.is_open() {
            return; // defensive: idempotent under repeated calls
        }
        let side = self.open_side.unwrap_or(Side::Long);
        let entry_price = self.entry_price;
        let committed = self.entry_notional;
        let entry_fee = self.entry_fee;

        let exit_fee = (self.quantity * close) * self.fee_rate;
        let gross_pl = match side {
            Side::Long => (close - entry_price) * self.quantity,
            Side::Short => (entry_price - close) * self.quantity,
        };

        // Release capital, credit P/L, charge the exit fee — in that order.
        self.available += committed;
        self.available += gross_pl;
        self.available -= exit_fee;
        self.total_fees_paid += exit_fee;

        let fees_paid = entry_fee + exit_fee;
        let net_pl = gross_pl - fees_paid;

        // Canonical chronological totals (close order == vector order).
        self.canonical_net_running += net_pl;
        self.canonical_fees_running += fees_paid;

        self.closed_trades.push(ClosedTrade {
            index: self.closed_trades.len().saturating_add(1),
            side_is_long: matches!(side, Side::Long),
            entry_bar_index: self.entry_bar_index,
            entry_price,
            entry_ts_secs: self.entry_ts_secs,
            entry_day: std::mem::take(&mut self.entry_day),
            exit_bar_index: bar_index,
            exit_price: close,
            exit_ts_secs: ts_secs,
            exit_day: utc_day(ts_secs),
            investment: committed,
            fees_paid,
            gross_pl,
            net_pl,
        });

        self.open_side = None;
    }
}

/// Unix seconds for a `SystemTime`; pre-epoch clamps to 0 (well-formed bar
/// data never regresses; matches the documented clamp in `replay`).
fn unix_secs(ts: std::time::SystemTime) -> i64 {
    let secs = ts
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    i64::try_from(secs).unwrap_or(i64::MAX)
}

/// UTC calendar date (`YYYY-MM-DD`) for Unix seconds.
///
/// Computed from the integer day count with Hinnant's civil-date arithmetic:
/// deterministic, no stdlib time conversions, and free of local-timezone
/// effects (formatting `SystemTime` would leak the process timezone). Seconds
/// below the epoch clamp to day 0, matching [`unix_secs`] documented clamp;
/// all intermediates are then non-negative, so plain integer division is the
/// floor division the algorithm requires. The 400-year Gregorian cycle keeps
/// every intermediate far inside `i64` for any timestamp that fits.
#[allow(clippy::arithmetic_side_effects)] // bounded integer date math
#[must_use]
pub fn utc_day(ts_secs: i64) -> String {
    let secs = ts_secs.max(0);
    let z = (secs / 86_400) + 719_468; // day count against the civil epoch
    let era = z / 146_097; // [0, inf), complete 400-year cycles
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let mut year = era * 400 + yoe;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // month pointer [0, 11]
    let dom = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    if month <= 2 {
        year += 1;
    }
    format!("{year:04}-{month:02}-{dom:02}")
}

#[cfg(test)]
mod tests {
    // Bounded test literals; the same deny-of-wrap policy applies to all
    // non-test code in this crate.
    #![allow(clippy::arithmetic_side_effects)]

    use super::*;
    use crate::strategy::Signal as S;
    use std::time::{Duration, SystemTime};

    /// Epsilon equality for float report values (independently computed refs).
    fn near(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    fn bar(close: f64, ts_secs: u64) -> Bar {
        Bar::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(ts_secs),
            close,
            close,
            close,
            close,
            100.0,
        )
        .expect("valid test bar")
    }

    const D_BASE: u64 = 1_725_458_400; // 2024-09-04 (UTC)

    #[test]
    fn long_round_trip_net_of_fees() {
        let mut acc = PaperAccount::new(1, 10_000.0, 5);
        near(acc.available(), 10_000.0);

        let out = acc.on_bar(&S::Long, 0, &bar(10.0, D_BASE));
        assert!(!out.entry_skipped);
        assert_eq!(out.effective_signal, S::Long);
        near(out.state.available, 9_989.995); // 10_000 - 10 - 10*5e-4
        near(out.state.equity, 9_999.995); // available + shares at close

        let out = acc.on_bar(&S::Flat, 1, &bar(12.0, D_BASE + 900));
        assert!(!out.entry_skipped);
        assert_eq!(out.effective_signal, S::Flat);
        near(out.state.available, 10_001.989); // +10 capital +2 gross -0.006 exit fee

        let trades = acc.closed_trades();
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert!(t.side_is_long);
        near(t.investment, 10.0);
        near(t.gross_pl, 2.0);
        near(t.fees_paid, 0.011); // 0.005 entry + 0.006 exit
        near(t.net_pl, 1.989);
        assert_eq!(t.entry_day, "2024-09-04");
        assert_eq!(t.exit_day, "2024-09-04");
        near(acc.total_fees_paid(), 0.011);
    }

    #[test]
    fn short_round_trip_net_of_fees() {
        let mut acc = PaperAccount::new(1, 10_000.0, 5);
        let out = acc.on_bar(&S::Short, 0, &bar(10.0, D_BASE));
        assert!(!out.entry_skipped);
        near(out.state.available, 9_989.995); // collateral locked at notional
        near(out.state.equity, 9_999.995);

        let out = acc.on_bar(&S::Flat, 1, &bar(8.0, D_BASE + 900));
        assert_eq!(out.effective_signal, S::Flat);
        near(out.state.available, 10_001.991); // +10 +2 gross - 8*5e-4=0.004

        let t = &acc.closed_trades()[0];
        assert!(!t.side_is_long);
        near(t.gross_pl, 2.0);
        near(t.fees_paid, 0.005 + 0.004);
        near(t.net_pl, 1.991);
    }

    #[test]
    fn zero_fee_round_trips_are_exact() {
        let mut acc = PaperAccount::new(2, 5_000.0, 0); // quantity 2 to mix in units
        let out = acc.on_bar(&S::Long, 0, &bar(10.0, D_BASE));
        near(out.state.available, 4_980.0); // - 2*10 notional
        near(out.state.equity, 5_000.0); // 4980 free + 2 shares at 10
        let out = acc.on_bar(&S::Flat, 1, &bar(12.0, D_BASE + 900));
        near(out.state.available, 5_004.0); // +2*10 capital + 2*2 gross
        near(acc.closed_trades()[0].fees_paid, 0.0); // zero bps: entry and exit fee both exactly 0
        near(acc.closed_trades()[0].net_pl, 4.0);
    }

    #[test]
    fn reversal_exits_then_enters_at_same_close() {
        let mut acc = PaperAccount::new(1, 10_000.0, 5);
        acc.on_bar(&S::Long, 0, &bar(10.0, D_BASE));

        // Long -> Short at 12.5: exit long leg, then enter short same close.
        let out = acc.on_bar(&S::Short, 1, &bar(12.5, D_BASE + 900));
        assert!(!out.entry_skipped);
        assert_eq!(out.effective_signal, S::Short);
        near(out.state.available, 9989.9825); // ref: mid state
        assert_eq!(acc.closed_trades().len(), 1);
        near(acc.closed_trades()[0].net_pl, 2.48875);

        let out = acc.on_bar(&S::Flat, 2, &bar(9.0, D_BASE + 1800));
        assert_eq!(out.effective_signal, S::Flat);
        near(out.state.available, 10_005.978); // ref: final state
        let trades = acc.closed_trades();
        assert_eq!(trades.len(), 2);
        near(trades[1].net_pl, 3.48925);
        near(acc.total_fees_paid(), 0.022); // both legs of both trades
    }

    #[test]
    fn insufficient_entry_is_skipped_and_recoverable() {
        let mut acc = PaperAccount::new(1, 10.0, 5);
        let out = acc.on_bar(&S::Long, 0, &bar(30.0, D_BASE));
        assert!(out.entry_skipped); // needs 30 + fee > 10
        assert_eq!(out.effective_signal, S::Flat);
        near(out.state.available, 10.0);
        assert!(acc.closed_trades().is_empty());

        let out = acc.on_bar(&S::Short, 1, &bar(9.0, D_BASE + 900)); // affordable now
        assert!(!out.entry_skipped);
        assert_eq!(out.effective_signal, S::Short);
        near(out.state.available, 0.9955); // 10 - 9 notional - 0.0045 entry fee (9*5e-4)
    }

    #[test]
    fn per_day_totals_split_by_utc_exit_and_entry_days() {
        let mut acc = PaperAccount::new(1, 10_000.0, 0);
        // Cross the UTC midnight between two bars (16h apart).
        let t_next_day = D_BASE + 57_600; // 2024-09-05 (UTC)
        acc.on_bar(&S::Long, 0, &bar(10.0, D_BASE));
        acc.on_bar(&S::Flat, 1, &bar(11.0, t_next_day));

        let days = acc.per_day_totals();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].day, "2024-09-04"); // entry day
        assert_eq!(days[0].entries, 1);
        assert_eq!(days[0].exits, 0);
        near(days[0].net_realized_pl, 0.0);
        assert_eq!(days[1].day, "2024-09-05"); // exit day
        assert_eq!(days[1].entries, 0);
        assert_eq!(days[1].exits, 1);
        near(days[1].net_realized_pl, 1.0); // +1 gross with zero fees
    }

    #[test]
    fn session_totals_sum_closed_trades() {
        let mut acc = PaperAccount::new(1, 10_000.0, 5);
        acc.on_bar(&S::Long, 0, &bar(10.0, D_BASE));
        acc.on_bar(&S::Flat, 1, &bar(12.0, D_BASE + 900));

        let totals = acc.session_totals(Some(12.0)); // flat at end
        near(totals.final_available, 10_001.989);
        near(totals.final_equity, 10_001.989); // no open position: equity == available
        near(totals.total_net_pl, 1.989); // equals the sum of trade net P/Ls
        near(acc.closed_trades()[0].net_pl, totals.total_net_pl); // one trade: totals are its row
    }

    #[allow(clippy::float_cmp)] // asserts bit-equality on purpose (see below)
    #[test]
    fn single_day_session_and_day_totals_coincide_exactly() {
        // With every close on one calendar day, the session row and the day
        // row sum in the same (close-order) sequence — they must coincide to
        // the bit, which is what makes the report self-consistent.
        let mut acc = PaperAccount::new(1, 10_000.0, 5);
        acc.on_bar(&S::Long, 0, &bar(10.0, D_BASE));
        acc.on_bar(&S::Short, 1, &bar(12.0, D_BASE + 900)); // exit long + open short
        acc.on_bar(&S::Flat, 2, &bar(13.5, D_BASE + 1_800)); // close same day

        let totals = acc.session_totals(Some(13.5));
        let days = acc.per_day_totals();
        assert_eq!(days.len(), 1);
        assert_eq!(totals.total_net_pl, days[0].net_realized_pl);
        assert_eq!(totals.total_fees_paid, acc.total_fees_paid());
    }

    #[test]
    fn utc_day_known_dates() {
        // Anchors computed independently against UTC (not local time).
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(86_399), "1970-01-01");
        assert_eq!(utc_day(86_400), "1970-01-02"); // day boundary in UTC
        assert_eq!(utc_day(1_725_458_400), "2024-09-04");
        assert_eq!(utc_day(1_709_164_800), "2024-02-29"); // leap day
        assert_eq!(utc_day(-5), "1970-01-01"); // pre-epoch clamps to 0 (documented)
    }
}
