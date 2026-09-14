//! Automated price-action trading.
//!
//! The crate is organised as a pipeline:
//!
//! - [`market`] holds the raw price data types ([`market::Bar`],
//!   [`market::BarSeries`]).
//! - [`strategy`] turns bars into [`strategy::Signal`]s; strategies implement
//!   the [`strategy::Strategy`] trait.
//! - [`execution`] carries decisions out via the [`execution::Broker`] trait
//!   (a paper-trading implementation is included).
//! - [`engine`] drives the loop: feed a bar, get a signal, act on it.
//!
//! Data-source adapters (broker APIs, market-data feeds) are intentionally not
//! part of the initial scaffold.

pub mod engine;
pub mod error;
pub mod execution;
pub mod market;
pub mod strategy;
