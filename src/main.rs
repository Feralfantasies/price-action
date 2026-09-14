//! Entry point for the price-action trading application.
//!
//! No market-data source is wired up yet, so this builds the engine against a
//! paper broker and reports that it is idle.

use price_action::{engine::Engine, execution::PaperBroker, strategy::ConsecutiveCloses};

fn main() {
    let engine = Engine::new(ConsecutiveCloses::new(3), PaperBroker::new());
    println!(
        "price-action: engine ready, last signal = {:?} (no market-data source configured yet)",
        engine.last_signal()
    );
}
