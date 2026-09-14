//! Raw price data: bars and the series they form.

use std::time::SystemTime;

/// A single OHLCV bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    /// Start of the bar's time window.
    pub timestamp: SystemTime,
    /// First traded price in the window.
    pub open: f64,
    /// Highest traded price in the window.
    pub high: f64,
    /// Lowest traded price in the window.
    pub low: f64,
    /// Last traded price in the window.
    pub close: f64,
    /// Quantity traded in the window.
    pub volume: f64,
}

impl Bar {
    /// Creates a bar, correcting inverted high/low values so `low <= high`.
    #[must_use]
    pub const fn new(
        timestamp: SystemTime,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
    ) -> Self {
        Self {
            timestamp,
            open,
            high: high.max(low),
            low: high.min(low),
            close,
            volume,
        }
    }

    /// Returns `true` when the bar closed at or above its open.
    #[must_use]
    pub fn is_bullish(self) -> bool {
        self.close >= self.open
    }

    /// Returns `true` when the bar closed at or below its open.
    #[must_use]
    pub fn is_bearish(self) -> bool {
        self.close <= self.open
    }

    /// Full high-to-low range of the bar.
    #[must_use]
    pub fn range(self) -> f64 {
        self.high - self.low
    }

    /// Size of the real body (open to close), always non-negative.
    #[must_use]
    pub fn body(self) -> f64 {
        (self.close - self.open).abs()
    }
}

/// A rolling window of the most recent bars, oldest first.
#[derive(Debug, Default, Clone)]
pub struct BarSeries {
    bars: Vec<Bar>,
    capacity: usize,
}

impl BarSeries {
    /// Creates an empty series holding at most `capacity` bars.
    ///
    /// A capacity of `0` is treated as `1`.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            bars: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    /// Appends a bar, evicting the oldest bar when at capacity.
    pub fn push(&mut self, bar: Bar) {
        if self.bars.len() == self.capacity {
            self.bars.remove(0);
        }
        self.bars.push(bar);
    }

    /// Most recent bar, if any.
    #[must_use]
    pub fn last(&self) -> Option<&Bar> {
        self.bars.last()
    }

    /// Number of bars currently held.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bars.len()
    }

    /// Returns `true` when no bars are held.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    /// Iterates over the bars, oldest first.
    pub fn iter(&self) -> std::slice::Iter<'_, Bar> {
        self.bars.iter()
    }
}

impl<'a> IntoIterator for &'a BarSeries {
    type Item = &'a Bar;
    type IntoIter = std::slice::Iter<'a, Bar>;

    fn into_iter(self) -> Self::IntoIter {
        self.bars.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(close: f64) -> Bar {
        Bar::new(SystemTime::UNIX_EPOCH, close, close, close, close, 0.0)
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "{actual} != {expected}"
        );
    }

    #[test]
    fn series_respects_capacity() {
        let mut series = BarSeries::new(3);
        for i in 0..5 {
            series.push(bar(f64::from(i)));
        }
        assert_eq!(series.len(), 3);
        let closes: Vec<f64> = series.iter().map(|b| b.close).collect();
        assert_eq!(closes, vec![2.0, 3.0, 4.0]);
        assert_eq!(series.last().map(|b| b.close), Some(4.0));
    }

    #[test]
    fn new_corrects_inverted_high_low() {
        let b = Bar::new(SystemTime::UNIX_EPOCH, 5.0, 3.0, 7.0, 5.0, 1.0);
        assert_close(b.high, 7.0);
        assert_close(b.low, 3.0);
        assert_close(b.range(), 4.0);
    }

    #[test]
    fn body_and_direction() {
        let b = Bar::new(SystemTime::UNIX_EPOCH, 10.0, 12.0, 9.0, 8.0, 100.0);
        assert!(b.is_bearish());
        assert!(!b.is_bullish());
        assert_close(b.body(), 2.0);
    }
}
