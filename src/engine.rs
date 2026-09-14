//! The trading loop: feeds bars to a strategy and acts on its signals.

use crate::{
    error::Error,
    execution::{Broker, Position},
    market::Bar,
    strategy::{Signal, Strategy},
};

/// Drives one [`Strategy`] against one [`Broker`].
///
/// If the broker fails to execute a signal, the desired position is kept as
/// pending and retried before the next bar is processed; bars are not fed to
/// the strategy until execution succeeds, so strategy and broker state cannot
/// drift apart.
#[derive(Debug)]
pub struct Engine<S, B> {
    strategy: S,
    broker: B,
    last_signal: Signal,
    pending: Option<Position>,
}

impl<S: Strategy, B: Broker> Engine<S, B> {
    /// Creates an engine around the given strategy and broker.
    pub const fn new(strategy: S, broker: B) -> Self {
        Self {
            strategy,
            broker,
            last_signal: Signal::Flat,
            pending: None,
        }
    }

    /// The most recent signal that was successfully executed on the broker.
    #[must_use]
    pub const fn last_signal(&self) -> Signal {
        self.last_signal
    }

    /// The position awaiting a retry after a failed execution, if any.
    #[must_use]
    pub const fn pending_position(&self) -> Option<Position> {
        self.pending
    }

    /// Processes a single bar: asks the strategy, then moves the broker to the
    /// position implied by the returned signal.
    ///
    /// If a previous execution failed, the pending position is retried first
    /// and `bar` is *not* consumed by the strategy until that retry succeeds.
    /// When execution of a fresh signal fails, the target position becomes
    /// pending and the error is returned; `last_signal` only advances on
    /// successful execution.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Strategy`] if the strategy rejects the bar or
    /// [`Error::Execution`] if the broker cannot reach the desired position.
    pub fn on_bar(&mut self, bar: &Bar) -> Result<Signal, Error> {
        if let Some(target) = self.pending {
            self.broker.set_position(target)?;
            self.pending = None;
        }
        let signal = self.strategy.on_bar(bar)?;
        let target = match signal {
            Signal::Flat => Position::Flat,
            Signal::Long => Position::Long,
            Signal::Short => Position::Short,
        };
        if let Err(e) = self.broker.set_position(target) {
            self.pending = Some(target);
            return Err(e);
        }
        self.last_signal = signal;
        Ok(signal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{execution::PaperBroker, market::Bar, strategy::ConsecutiveCloses};
    use std::time::SystemTime;

    fn bar(close: f64) -> Bar {
        Bar::new(SystemTime::UNIX_EPOCH, close, close, close, close, 0.0).unwrap()
    }

    /// Strategy that counts how many bars it was fed and always signals long.
    struct CountingStrategy {
        calls: usize,
    }

    impl Strategy for CountingStrategy {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn on_bar(&mut self, _bar: &Bar) -> Result<Signal, Error> {
            self.calls = self.calls.saturating_add(1);
            Ok(Signal::Long)
        }
    }

    /// Broker that fails the next `failures_left` executions.
    struct FailingBroker {
        failures_left: usize,
        position: Position,
    }

    impl Broker for FailingBroker {
        fn set_position(&mut self, target: Position) -> Result<(), Error> {
            if self.failures_left > 0 {
                self.failures_left = self.failures_left.saturating_sub(1);
                return Err(Error::Execution("broker unavailable".to_string()));
            }
            self.position = target;
            Ok(())
        }
    }

    #[test]
    fn engine_moves_broker_with_signals() {
        let mut engine = Engine::new(ConsecutiveCloses::new(2), PaperBroker::new());

        for close in [10.0, 11.0, 12.0] {
            engine.on_bar(&bar(close)).unwrap();
        }

        assert_eq!(engine.last_signal(), Signal::Long);
    }

    #[test]
    fn failed_execution_becomes_pending_and_blocks_later_bars() {
        let mut engine = Engine::new(
            CountingStrategy { calls: 0 },
            FailingBroker {
                failures_left: 2,
                position: Position::Flat,
            },
        );

        // Bar 1: strategy emits Long, execution fails -> pending.
        let err = engine.on_bar(&bar(1.0)).unwrap_err();
        assert!(matches!(err, Error::Execution(_)));
        assert_eq!(engine.pending_position(), Some(Position::Long));
        assert_eq!(engine.last_signal(), Signal::Flat);
        assert_eq!(engine.strategy.calls, 1);

        // Bar 2: retry fails again -> bar is NOT fed to the strategy.
        assert!(engine.on_bar(&bar(2.0)).is_err());
        assert_eq!(engine.strategy.calls, 1);
        assert_eq!(engine.pending_position(), Some(Position::Long));

        // Bar 3: retry succeeds, then bar 3 is processed normally.
        let signal = engine.on_bar(&bar(3.0)).unwrap();
        assert_eq!(signal, Signal::Long);
        assert_eq!(engine.strategy.calls, 2);
        assert_eq!(engine.pending_position(), None);
        assert_eq!(engine.last_signal(), Signal::Long);
        assert_eq!(engine.broker.position, Position::Long);
    }
}
