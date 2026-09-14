//! Turning raw price movement into trading signals.

use crate::{error::Error, market::Bar};

/// Direction of a trade suggested by a strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// No position should be held.
    Flat,
    /// A long position should be held.
    Long,
    /// A short position should be held.
    Short,
}

/// A price-action strategy: consumes bars, emits signals.
pub trait Strategy {
    /// Human-readable name used in logs and metrics.
    fn name(&self) -> &'static str;

    /// Feeds the next bar and returns the strategy's current signal.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Strategy`] when the bar cannot be processed.
    fn on_bar(&mut self, bar: &Bar) -> Result<Signal, Error>;
}

/// Example strategy: go long after `threshold` consecutive higher closes and
/// short after `threshold` consecutive lower closes; otherwise flat.
#[derive(Debug)]
pub struct ConsecutiveCloses {
    threshold: i32,
    run: i32,
    prev_close: Option<f64>,
}

impl ConsecutiveCloses {
    /// Creates the strategy. A `threshold` below `1` is treated as `1`.
    #[must_use]
    pub fn new(threshold: u32) -> Self {
        Self {
            threshold: i32::try_from(threshold).unwrap_or(i32::MAX).max(1),
            run: 0,
            prev_close: None,
        }
    }

    const fn evaluate(&self) -> Signal {
        if self.run >= self.threshold {
            Signal::Long
        } else if self.run <= self.threshold.saturating_neg() {
            Signal::Short
        } else {
            Signal::Flat
        }
    }
}

impl Strategy for ConsecutiveCloses {
    fn name(&self) -> &'static str {
        "consecutive-closes"
    }

    fn on_bar(&mut self, bar: &Bar) -> Result<Signal, Error> {
        let signal = match self.prev_close {
            Some(prev) if bar.close > prev => {
                self.run = if self.run > 0 {
                    self.run.saturating_add(1)
                } else {
                    1
                };
                self.evaluate()
            }
            Some(prev) if bar.close < prev => {
                self.run = if self.run < 0 {
                    self.run.saturating_sub(1)
                } else {
                    -1
                };
                self.evaluate()
            }
            _ => {
                self.run = 0;
                Signal::Flat
            }
        };
        self.prev_close = Some(bar.close);
        Ok(signal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::Bar;
    use std::time::SystemTime;

    fn feed(strategy: &mut ConsecutiveCloses, closes: &[f64]) -> Vec<Signal> {
        closes
            .iter()
            .map(|&close| {
                let bar = Bar::new(SystemTime::UNIX_EPOCH, close, close, close, close, 0.0);
                strategy.on_bar(&bar).unwrap()
            })
            .collect()
    }

    #[test]
    fn goes_long_after_threshold_higher_closes() {
        let mut strategy = ConsecutiveCloses::new(2);
        let signals = feed(&mut strategy, &[10.0, 11.0, 12.0]);
        assert_eq!(signals, vec![Signal::Flat, Signal::Flat, Signal::Long]);
    }

    #[test]
    fn goes_short_after_threshold_lower_closes() {
        let mut strategy = ConsecutiveCloses::new(2);
        let signals = feed(&mut strategy, &[12.0, 11.0, 10.0]);
        assert_eq!(signals, vec![Signal::Flat, Signal::Flat, Signal::Short]);
    }

    #[test]
    fn run_resets_on_direction_change() {
        let mut strategy = ConsecutiveCloses::new(3);
        let signals = feed(&mut strategy, &[10.0, 11.0, 12.0, 11.5]);
        assert_eq!(
            signals,
            vec![Signal::Flat, Signal::Flat, Signal::Flat, Signal::Flat]
        );
    }

    #[test]
    fn zero_threshold_is_treated_as_one() {
        let mut strategy = ConsecutiveCloses::new(0);
        let signals = feed(&mut strategy, &[10.0, 11.0]);
        assert_eq!(signals, vec![Signal::Flat, Signal::Long]);
    }
}
