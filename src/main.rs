//! Entry point for the price-action trading application.
//!
//! Loads the layered configuration (env > config file > defaults), then builds
//! the engine. No market-data source is wired up yet, so the engine only
//! reports that it is ready.

use price_action::{
    config::{Config, Mode},
    engine::Engine,
    error::Error,
    execution::PaperBroker,
    strategy::ConsecutiveCloses,
};

fn main() -> Result<(), Error> {
    let config = Config::load()?;

    if config.mode == Mode::Live {
        return Err(Error::Execution(
            "live trading is not implemented yet; run in paper mode".to_string(),
        ));
    }

    println!(
        "price-action: symbol={} mode={} quantity={} bar_interval={}s threshold={}",
        config.symbol,
        config.mode,
        config.quantity,
        config.bar_interval_secs,
        config.consecutive_closes_threshold
    );

    let engine = Engine::new(
        ConsecutiveCloses::new(config.consecutive_closes_threshold),
        PaperBroker::new(),
    );
    println!(
        "price-action: engine ready, last signal = {:?} (no market-data source configured yet)",
        engine.last_signal()
    );
    Ok(())
}
