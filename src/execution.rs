//! Order execution backends.

use crate::error::Error;

/// The position a broker should be holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    /// No open position.
    #[default]
    Flat,
    /// Long the instrument.
    Long,
    /// Short the instrument.
    Short,
}

/// Anything that can carry out trading decisions.
pub trait Broker {
    /// Moves the account to the desired position.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Execution`] when the backend cannot reach `target`.
    fn set_position(&mut self, target: Position) -> Result<(), Error>;
}

/// In-memory broker that only tracks its intended position. A placeholder for
/// a real venue connection; it never places orders.
#[derive(Debug, Default)]
pub struct PaperBroker {
    position: Position,
}

impl PaperBroker {
    /// Creates a flat paper broker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The position currently held.
    #[must_use]
    pub const fn position(&self) -> Position {
        self.position
    }
}

impl Broker for PaperBroker {
    fn set_position(&mut self, target: Position) -> Result<(), Error> {
        self.position = target;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_broker_tracks_position() {
        let mut broker = PaperBroker::new();
        assert_eq!(broker.position(), Position::Flat);
        broker.set_position(Position::Long).unwrap();
        assert_eq!(broker.position(), Position::Long);
        broker.set_position(Position::Flat).unwrap();
        assert_eq!(broker.position(), Position::Flat);
    }
}
