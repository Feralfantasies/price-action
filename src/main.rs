//! Entry point for the price-action trading application.
//!
//! Loads the layered configuration (env > config file > defaults), then:
//! - with no arguments, reports that the engine is ready (no market-data
//!   source is wired up yet); or
//! - with `replay <bars.csv>`, replays recorded OHLCV bars through the
//!   configured strategy against a paper broker — the recommended first step
//!   for checking how settings behave against genuine historic data.

use std::env;

use price_action::{
    config::Config, engine::Engine, error::Error, execution::PaperBroker, replay,
    strategy::ConsecutiveCloses,
};

fn main() {
    if let Err(err) = run() {
        eprintln!("price-action: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Error> {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.first().map(String::as_str) == Some("replay") {
        let path = args.get(1).ok_or_else(|| {
            Error::Config(
                "usage: price-action replay <bars.csv> \u{2014} e.g. `price-action replay samples/sample-bars.csv`"
                    .to_string(),
            )
        })?;
        let config = Config::load()?;
        println!(
            "price-action: symbol={} mode={:?} quantity={} bar_interval={}s threshold={}",
            config.symbol,
            config.mode,
            config.quantity,
            config.bar_interval_secs,
            config.consecutive_closes_threshold
        );
        let report = replay::run(&config, path)?;
        replay::print_report(&config, &report, std::io::stdout())
            .map_err(|e| Error::MarketData(format!("cannot print replay report: {e}")))?;
        return Ok(());
    }

    // No subcommand: the default (and only other) behaviour. Live trading is
    // not implemented, and no market-data source exists yet, so this branch
    // just verifies configuration loads and reports readiness.
    let config = Config::load()?;

    if config.mode == price_action::config::Mode::Live {
        return Err(Error::Execution(
            "live trading is not implemented yet; run in paper mode".to_string(),
        ));
    }

    println!(
        "price-action: symbol={} mode={:?} quantity={} bar_interval={}s threshold={}",
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
    println!("hint: try `price-action replay samples/sample-bars.csv` for a worked example");
    Ok(())
}
