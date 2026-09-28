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
//! - [`live`] runs one of those sessions against **real** time data: the
//!   [`feed`] module maintains the Massive.com stocks WebSocket, and `live`
//!   turns its events into bars the session consumes (mock/paper execution
//!   only; a report plus an offline-replayable CSV come out). Each closed UTC
//!   day is summarized and handed to [`notify`] for delivery.
//!
//! - [`config`] resolves the layered configuration: environment variables
//!   override the TOML config file, which overrides compiled defaults.
//!
//! Market data arrives one of two ways: recorded bars replayed with
//! [`replay`], or the [`feed`] WebSocket streamed through [`live`]. Broker
//! adapters are intentionally **not** part of this crate — both paths execute
//! against the in-memory [`execution::PaperBroker`] and the funded paper
//! account in [`accounting`], so no order can be placed anywhere.

pub mod accounting;
pub mod config;
pub mod csv;
pub mod engine;
pub mod error;
pub mod execution;
pub mod feed;
pub mod live;
pub mod market;
pub mod notify;
pub mod replay;
pub mod strategy;
