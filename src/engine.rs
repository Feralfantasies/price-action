//! The trading loop: feeds bars to a strategy and acts on its signals.

use crate::{
    error::Error,
    execution::{Broker, Position},
    market::Bar,
    strategy::{Signal, Strategy},
};

/// Drives one [`Strategy`] against one [`Broker`].
#[derive(Debug)]
pub struct Engine<S, B> {
    strategy: S,
    broker: B,
    last_signal: Signal,
}

impl<S: Strategy, B: Broker> Engine<S, B> {
    /// Creates an engine around the given strategy and broker.
    pub const fn new(strategy: S, broker: B) -> Self {
        Self {
            strategy,
            broker,
            last_signal: Signal::Flat,
        }
    }

    /// The most recent signal emitted by the strategy.
    #[must_use]
    pub const fn last_signal(&self) -> Signal {
        self.last_signal
    }

    /// Processes a single bar: asks the strategy, then moves the broker to the
    /// position implied by the returned signal.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Strategy`] if the strategy rejects the bar or
    /// [`Error::Execution`] if the broker cannot reach the desired position.
    pub fn on_bar(&mut self, bar: &Bar) -> Result<Signal, Error> {
        let signal = self.strategy.on_bar(bar)?;
        let target = match signal {
            Signal::Flat => Position::Flat,
            Signal::Long => Position::Long,
            Signal::Short => Position::Short,
        };
        self.broker.set_position(target)?;
        self.last_signal = signal;
        Ok(signal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{execution::PaperBroker, market::Bar, strategy::ConsecutiveCloses};
    use std::time::SystemTime;

    #[test]
    fn engine_moves_broker_with_signals() {
        let mut engine = Engine::new(ConsecutiveCloses::new(2), PaperBroker::new());

        for close in [10.0, 11.0, 12.0] {
            let bar = Bar::new(SystemTime::UNIX_EPOCH, close, close, close, close, 0.0);
            engine.on_bar(&bar).unwrap();
        }

        assert_eq!(engine.last_signal(), Signal::Long);
    }
}
