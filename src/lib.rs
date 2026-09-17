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
//! - [`replay`] replays recorded bars through the engine; its report prices
//!   those signals as a funded paper account (see [`accounting`]).
//!
//! - [`config`] resolves the layered configuration: environment variables
//!   override the TOML config file, which overrides compiled defaults.
//!
//! Data-source adapters (broker APIs, market-data feeds) are intentionally not
//! part of the initial scaffold; recorded data can be replayed with
//! [`replay`].

pub mod accounting;
pub mod config;
pub mod csv;
pub mod engine;
pub mod error;
pub mod execution;
pub mod market;
pub mod replay;
pub mod strategy;
