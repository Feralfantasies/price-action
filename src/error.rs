//! Error types shared across the crate.

/// Errors produced while running the trading pipeline.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A market-data source failed or produced unusable data.
    #[error("market data error: {0}")]
    MarketData(String),

    /// The execution backend rejected or failed an order.
    #[error("execution error: {0}")]
    Execution(String),

    /// A strategy failed to process a bar.
    #[error("strategy error: {0}")]
    Strategy(String),
}
