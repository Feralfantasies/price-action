//! Layered application configuration.
//!
//! Values are resolved in strict precedence order:
//!
//! 1. **Environment variables** (`PRICE_ACTION_*`) — highest priority, so
//!    container deployments can override anything without touching files.
//! 2. **Config file** — TOML at the path named by `PRICE_ACTION_CONFIG`
//!    (default: `price-action.toml` in the working directory). A missing file
//!    is not an error.
//! 3. **Compiled defaults** — [`Config::default`].
//!
//! Load once at startup with [`Config::load`] and pass the result through the
//! application; never read `std::env` directly afterwards.

use std::{fmt, path::Path};

use serde::Deserialize;

/// Environment variable naming the config-file path.
pub const CONFIG_PATH_VAR: &str = "PRICE_ACTION_CONFIG";

/// Config-file path used when [`CONFIG_PATH_VAR`] is unset.
pub const DEFAULT_CONFIG_PATH: &str = "price-action.toml";

/// Prefix of every environment variable this module reads.
pub const ENV_PREFIX: &str = "PRICE_ACTION_";

/// Directory `live` writes its persisted session CSVs into when neither the
/// config file nor an environment var sets [`Config::live_csv_dir`].
pub const DEFAULT_LIVE_CSV_DIR: &str = "./sessions";

/// Lookup function for environment variables. Returns `None` for unset or
/// empty values (an empty variable is treated as unset).
pub type EnvFn<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Where the application should place trades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Simulated fills only; no real orders. The default.
    #[default]
    Paper,
    /// Real orders via a broker. Requires `broker_url` to be configured.
    Live,
}

impl Mode {
    fn parse(raw: &str) -> Result<Self, ConfigError> {
        match raw.to_ascii_lowercase().as_str() {
            "paper" => Ok(Self::Paper),
            "live" => Ok(Self::Live),
            other => Err(ConfigError::invalid(
                "mode",
                format!("unknown mode {other:?}; expected \"paper\" or \"live\""),
            )),
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Paper => f.write_str("paper"),
            Self::Live => f.write_str("live"),
        }
    }
}

/// Where the `live` subcommand's market data comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeedSource {
    /// Massive.com stocks WebSocket (authenticated). The default.
    #[default]
    Massive,
    /// Kraken Spot WebSocket **v2** public OHLC channel — no authentication
    /// exists for it and none is used; like every other path in this crate it
    /// can only supply data, never place an order.
    Kraken,
}

/// Parses the configured live feed source name (case-insensitive, trimmed).
///
/// # Errors
///
/// [`ConfigError::invalid`] naming `live_feed_source` when `raw` is neither
/// `"massive"` nor `"kraken"`.
pub fn parse_feed_source(raw: &str) -> Result<FeedSource, ConfigError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "massive" => Ok(FeedSource::Massive),
        "kraken" => Ok(FeedSource::Kraken),
        other => Err(ConfigError::invalid(
            "live_feed_source",
            format!("unknown feed source {other:?}; expected \"massive\" or \"kraken\""),
        )),
    }
}

/// Fully resolved application configuration.
///
/// The [`fmt::Debug`] implementation is manual so that `broker_api_key`,
/// `massive_api_key` and `telegram_bot_token` are always redacted — formatting
/// a `Config` (logs, assertion failures) never exposes a credential.
#[derive(Clone, PartialEq)]
/// `Eq` intentionally absent: `Config` holds an f64 (`starting_balance`).
pub struct Config {
    /// Instrument symbol to trade, e.g. `AAPL`.
    pub symbol: String,
    /// Shares/contracts per trade. Must be at least 1.
    pub quantity: u32,
    /// Paper (simulated) or live trading. Defaults to paper.
    pub mode: Mode,
    /// Bar interval in seconds. Must be at least 1.
    pub bar_interval_secs: u64,
    /// Number of recent bars retained in the rolling window.
    pub series_capacity: usize,
    /// Consecutive higher/lower closes before the example strategy signals.
    pub consecutive_closes_threshold: u32,
    /// Starting funds for replay paper-accounting. Must be greater than 0 and
    /// finite; every replay trade is funded from it (see `replay` accounting).
    pub starting_balance: f64,
    /// Fee charged in basis points on each side's trade notional at entry and
    /// exit during replay accounting (5 = 0.05%).
    pub trade_fee_bps: u32,
    /// Broker/venue API base URL. Required in live mode.
    pub broker_url: Option<String>,
    /// Broker API key. Prefer the environment variable or a mounted secret
    /// over committing it to a config file.
    pub broker_api_key: Option<String>,
    /// Massive.com socket host for the `live` subcommand's market data (bare
    /// hostname, no scheme): `socket.massive.com` (real-time) or
    /// `delayed.massive.com` (15-minute delayed). Live-session only.
    pub live_feed_host: String,
    /// Native feed channel the live session consumes: `Minute` → per-minute
    /// OHLCV windows (`AM.<SYMBOL>`, one bar per traded minute); `Ticks` →
    /// tick trades (`T.<SYMBOL>`), aggregated locally into 1-second bars.
    /// Defined in [`crate::feed`] (the module that speaks the wire).
    pub live_feed_channel: crate::feed::FeedChannel,
    /// Where `live` gets its market data from: [`FeedSource::Massive`] (the
    /// authenticated stocks WebSocket, needs `massive_api_key`) or
    /// [`FeedSource::Kraken`] (public Spot WebSocket v2 OHLC channel — no key;
    /// minute-granular windows only, so `live_feed_channel` must be `"minute"`
    /// and `bar_interval_secs` must name one of Kraken's candle intervals). Live-session
    /// only.
    pub live_feed_source: FeedSource,
    /// Massive.com API key for the market-data feed. Secret — prefer the
    /// environment variable over committing it to a config file.
    pub massive_api_key: Option<String>,
    /// Directory where `live` persists its session CSV; see
    /// [`DEFAULT_LIVE_CSV_DIR`] (used when unset in every layer).
    pub live_csv_dir: String,
    /// Telegram bot token used to deliver daily summaries. Secret — prefer the
    /// environment variable or a mounted secret over committing it to a file.
    /// Optional: with neither Telegram setting present, summaries go to the
    /// console only (see [`crate::notify::TelegramNotifier::from_config`]).
    pub telegram_bot_token: Option<String>,
    /// Telegram destination chat id for daily summaries (may be negative for a
    /// group or channel). Not a credential, but meaningless without the token:
    /// setting exactly one of the two is rejected as a configuration error.
    pub telegram_chat_id: Option<String>,
}

/// Parses the configured live feed channel name.
///
/// The wire-facing enum lives in [`crate::feed`]; this free function is its
/// `ConfigError`-aware parser (keeps string-parsing concerns with
/// configuration).
///
/// # Errors
///
/// [`ConfigError::invalid`] naming `live_feed_channel` when `raw` is neither
/// `"minute"` nor `"ticks"` (case-insensitive, trimmed).
pub fn parse_feed_channel(raw: &str) -> Result<crate::feed::FeedChannel, ConfigError> {
    match raw.to_ascii_lowercase().trim() {
        "minute" => Ok(crate::feed::FeedChannel::Minute),
        "ticks" => Ok(crate::feed::FeedChannel::Ticks),
        other => Err(ConfigError::invalid(
            "live_feed_channel",
            format!("unknown channel {other:?}; expected \"minute\" or \"ticks\""),
        )),
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            symbol: "AAPL".to_string(),
            quantity: 1,
            mode: Mode::default(),
            bar_interval_secs: 60,
            series_capacity: 500,
            consecutive_closes_threshold: 3,
            starting_balance: 10_000.0,
            trade_fee_bps: 5,
            broker_url: None,
            broker_api_key: None,
            live_feed_host: "socket.massive.com".to_string(),
            live_feed_channel: crate::feed::FeedChannel::Minute,
            live_feed_source: FeedSource::Massive,
            massive_api_key: None,
            live_csv_dir: DEFAULT_LIVE_CSV_DIR.to_string(),
            telegram_bot_token: None,
            telegram_chat_id: None,
        }
    }
}

/// Shape of the TOML config file: every field optional so the file only
/// overrides what it actually sets. Unknown keys are rejected to catch typos.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    symbol: Option<String>,
    quantity: Option<u32>,
    mode: Option<String>,
    bar_interval_secs: Option<u64>,
    series_capacity: Option<usize>,
    consecutive_closes_threshold: Option<u32>,
    starting_balance: Option<f64>,
    trade_fee_bps: Option<u32>,
    broker_url: Option<String>,
    broker_api_key: Option<String>,
    live_feed_host: Option<String>,
    live_feed_channel: Option<String>,
    live_feed_source: Option<String>,
    massive_api_key: Option<String>,
    live_csv_dir: Option<String>,
    telegram_bot_token: Option<String>,
    telegram_chat_id: Option<String>,
}

impl Config {
    /// Loads configuration from the real process environment.
    ///
    /// The config-file path comes from `PRICE_ACTION_CONFIG`, falling back to
    /// [`DEFAULT_CONFIG_PATH`]. A missing file is not an error; an unreadable
    /// or malformed one is.
    ///
    /// This path enforces *all* validation, including execution-specific
    /// rules (live mode requires `broker_url`) — use it for anything the app
    /// might actually run.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the config file cannot be read or parsed,
    /// when an environment variable holds an unparseable value, or when the
    /// resolved configuration fails validation.
    pub fn load() -> Result<Self, ConfigError> {
        let env = |var: &str| std::env::var(var).ok().filter(|v| !v.is_empty());
        let path = env(CONFIG_PATH_VAR).unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string());
        Self::load_into(Path::new(&path), &env, true)
    }

    /// Loads configuration for the `live` subcommand: everything
    /// [`Config::load`] enforces, **plus** a resolvable market-data API key
    /// (the feed is authenticated; data-only — no execution involved yet).
    ///
    /// # Errors
    ///
    /// Same as [`Config::load`], rejecting an absent/blank `massive_api_key`.
    pub fn load_for_live() -> Result<Self, ConfigError> {
        let env = |var: &str| std::env::var(var).ok().filter(|v| !v.is_empty());
        let path = env(CONFIG_PATH_VAR).unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string());
        Self::load_live_from(Path::new(&path), &env)
    }

    /// Testing seam for [`Config::load_for_live`] on an explicit file path and
    /// injected environment lookup; layering/validation semantics are
    /// identical.
    ///
    /// # Errors
    ///
    /// Same as [`Config::load_for_live`].
    pub fn load_live_from(path: &Path, env: EnvFn<'_>) -> Result<Self, ConfigError> {
        let config = Self::load_into(path, env, true)?;
        config.validate_live_market_data()?;
        Ok(config)
    }

    /// Loads configuration for tool paths that never execute orders — e.g.
    /// bar replay.
    ///
    /// File/env layering and general validation (symbol presence, positive
    /// quantities, strategy parameters) are identical to [`Config::load`];
    /// only the execution-only rule (live mode demanding a non-empty broker
    /// URL) is skipped, which a paper-broker replay has no use for.
    ///
    /// # Errors
    ///
    /// Same as [`Config::load`] minus the live-mode `broker_url` requirement.
    pub fn load_for_replay() -> Result<Self, ConfigError> {
        let env = |var: &str| std::env::var(var).ok().filter(|v| !v.is_empty());
        let path = env(CONFIG_PATH_VAR).unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string());
        Self::load_into(Path::new(&path), &env, false)
    }

    /// Shared layering: defaults ← config file ← environment.
    ///
    /// # Errors
    ///
    /// See [`Config::load`]; when `check_execution` is `false`, the
    /// live-mode broker requirement does not apply.
    fn load_into(path: &Path, env: EnvFn<'_>, check_execution: bool) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        config.apply_file(path)?;
        config.apply_env(env)?;
        config.validate_general()?;
        if check_execution {
            config.validate_execution()?;
        }
        Ok(config)
    }

    /// Loads configuration from an explicit file path and environment lookup.
    /// Layering and validation match [`Config::load`] (i.e. it also applies
    /// execution validation).
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] on file, parse, environment, or validation
    /// failures.
    pub fn load_from(path: &Path, env: EnvFn<'_>) -> Result<Self, ConfigError> {
        Self::load_into(path, env, true)
    }

    fn apply_file(&mut self, path: &Path) -> Result<(), ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(ConfigError::file(path, e.to_string())),
        };
        let file: FileConfig =
            toml::from_str(&text).map_err(|e| ConfigError::file(path, e.to_string()))?;

        if let Some(v) = file.symbol {
            self.symbol = v;
        }
        if let Some(v) = file.quantity {
            self.quantity = v;
        }
        if let Some(v) = file.mode {
            self.mode = Mode::parse(&v)
                .map_err(|e| ConfigError::file(path, format!("invalid mode: {e}")))?;
        }
        if let Some(v) = file.bar_interval_secs {
            self.bar_interval_secs = v;
        }
        if let Some(v) = file.series_capacity {
            self.series_capacity = v;
        }
        if let Some(v) = file.consecutive_closes_threshold {
            self.consecutive_closes_threshold = v;
        }
        if let Some(v) = file.starting_balance {
            self.starting_balance = v;
        }
        if let Some(v) = file.trade_fee_bps {
            self.trade_fee_bps = v;
        }
        if let Some(v) = file.broker_url {
            self.broker_url = Some(v);
        }
        if let Some(v) = file.broker_api_key {
            self.broker_api_key = Some(v);
        }
        if let Some(v) = file.live_feed_host {
            self.live_feed_host = v;
        }
        if let Some(v) = file.live_feed_channel {
            self.live_feed_channel =
                parse_feed_channel(&v).map_err(|e| ConfigError::file(path, e.to_string()))?;
        }
        if let Some(v) = file.live_feed_source {
            self.live_feed_source =
                parse_feed_source(&v).map_err(|e| ConfigError::file(path, e.to_string()))?;
        }
        if let Some(v) = file.massive_api_key {
            self.massive_api_key = Some(v);
        }
        if let Some(v) = file.live_csv_dir {
            self.live_csv_dir = v;
        }
        if let Some(v) = file.telegram_bot_token {
            self.telegram_bot_token = Some(v);
        }
        if let Some(v) = file.telegram_chat_id {
            self.telegram_chat_id = Some(v);
        }
        Ok(())
    }

    fn apply_env(&mut self, env: EnvFn<'_>) -> Result<(), ConfigError> {
        env_override(env, "SYMBOL", &mut self.symbol)?;
        env_override(env, "QUANTITY", &mut self.quantity)?;
        env_override(env, "BAR_INTERVAL_SECS", &mut self.bar_interval_secs)?;
        env_override(env, "SERIES_CAPACITY", &mut self.series_capacity)?;
        env_override(
            env,
            "CONSECUTIVE_CLOSES_THRESHOLD",
            &mut self.consecutive_closes_threshold,
        )?;
        env_override(env, "STARTING_BALANCE", &mut self.starting_balance)?;
        env_override(env, "TRADE_FEE_BPS", &mut self.trade_fee_bps)?;

        if let Some(raw) = env("PRICE_ACTION_MODE") {
            self.mode = Mode::parse(&raw)
                .map_err(|e| ConfigError::invalid("PRICE_ACTION_MODE", e.to_string()))?;
        }
        if let Some(v) = env("PRICE_ACTION_BROKER_URL") {
            self.broker_url = Some(v);
        }
        if let Some(v) = env("PRICE_ACTION_BROKER_API_KEY") {
            self.broker_api_key = Some(v);
        }
        if let Some(v) = env("PRICE_ACTION_LIVE_FEED_HOST") {
            self.live_feed_host = v;
        }
        if let Some(raw) = env("PRICE_ACTION_LIVE_FEED_CHANNEL") {
            self.live_feed_channel = parse_feed_channel(&raw)?;
        }
        if let Some(raw) = env("PRICE_ACTION_LIVE_FEED_SOURCE") {
            self.live_feed_source = parse_feed_source(&raw)?;
        }
        if let Some(v) = env("PRICE_ACTION_MASSIVE_API_KEY") {
            self.massive_api_key = Some(v);
        }
        if let Some(v) = env("PRICE_ACTION_LIVE_CSV_DIR") {
            self.live_csv_dir = v;
        }
        if let Some(v) = env("PRICE_ACTION_TELEGRAM_BOT_TOKEN") {
            self.telegram_bot_token = Some(v);
        }
        if let Some(v) = env("PRICE_ACTION_TELEGRAM_CHAT_ID") {
            self.telegram_chat_id = Some(v);
        }
        Ok(())
    }

    /// General validation: symbol presence, positive quantities and strategy
    /// parameters. Applies to *every* caller, including replay.
    fn validate_general(&self) -> Result<(), ConfigError> {
        if self.symbol.trim().is_empty() {
            return Err(ConfigError::invalid("symbol", "must not be empty".into()));
        }
        if self.quantity == 0 {
            return Err(ConfigError::invalid(
                "quantity",
                "must be at least 1".into(),
            ));
        }
        if self.bar_interval_secs == 0 {
            return Err(ConfigError::invalid(
                "bar_interval_secs",
                "must be at least 1".into(),
            ));
        }
        if self.series_capacity == 0 {
            return Err(ConfigError::invalid(
                "series_capacity",
                "must be at least 1".into(),
            ));
        }
        if self.consecutive_closes_threshold == 0 {
            return Err(ConfigError::invalid(
                "consecutive_closes_threshold",
                "must be at least 1".into(),
            ));
        }
        // `f64`'s `FromStr` accepts strings like `inf`/`NaN`, so the explicit
        // finiteness check matters (same as for bar prices).
        if !self.starting_balance.is_finite() || self.starting_balance <= 0.0 {
            return Err(ConfigError::invalid(
                "starting_balance",
                "must be a finite value greater than 0".into(),
            ));
        }
        // The feed host is a bare hostname (no scheme, path, whitespace, or
        // userinfo): `live` builds `wss://{host}/stocks` from it. A stray `@`
        // would turn the prefix into URL userinfo that the WebSocket client
        // parses as credentials, so reject it like any other non-hostname
        // character.
        if self.live_feed_host.trim().is_empty()
            || self
                .live_feed_host
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '/' | ':' | '@' | '\\'))
        {
            return Err(ConfigError::invalid(
                "live_feed_host",
                format!(
                    "must be a bare hostname without scheme, path, userinfo (`@`) or whitespace (e.g. `socket.massive.com`); got {:?}",
                    self.live_feed_host
                ),
            ));
        }
        if self.live_csv_dir.trim().is_empty() {
            return Err(ConfigError::invalid(
                "live_csv_dir",
                "must not be empty (it names where live session CSVs are written)".into(),
            ));
        }
        Ok(())
    }

    /// Execution-specific validation: the live-mode broker requirement.
    /// Kept separate from [`validate_general`](Self::validate_general) so
    /// tool paths that resolve configuration but never execute (replay) can
    /// skip it without weakening anything an actual run enforces.
    fn validate_execution(&self) -> Result<(), ConfigError> {
        if self.mode == Mode::Live
            && self
                .broker_url
                .as_deref()
                .is_none_or(|url| url.trim().is_empty())
        {
            return Err(ConfigError::invalid(
                "broker_url",
                "live mode requires a non-empty PRICE_ACTION_BROKER_URL or broker_url in the config file"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Live-market-data validation, per selected source: Massive requires a
    /// resolvable API key (env or file); Kraken uses public endpoints and needs
    /// no secrets, but offers only minute-granular OHLC windows — it rejects
    /// the ticks channel, and `bar_interval_secs` is then the candle length it
    /// subscribes to, which must be one of Kraken's allowed intervals.
    /// Separate from execution validation because it gates only the `live`
    /// subcommand — replay and tool paths never touch the feed.
    fn validate_live_market_data(&self) -> Result<(), ConfigError> {
        match self.live_feed_source {
            FeedSource::Massive => {
                if self
                    .massive_api_key
                    .as_deref()
                    .is_none_or(|k| k.trim().is_empty())
                {
                    return Err(ConfigError::invalid(
                        "massive_api_key",
                        "the live command needs a Massive.com API key: set PRICE_ACTION_MASSIVE_API_KEY or massive_api_key in the config file".into(),
                    ));
                }
            }
            FeedSource::Kraken => {
                if self.live_feed_channel == crate::feed::FeedChannel::Ticks {
                    return Err(ConfigError::invalid(
                        "live_feed_channel",
                        "the Kraken source provides minute-granular OHLC windows; live_feed_channel must be \"minute\"".into(),
                    ));
                }
                let interval = u32::try_from(self.bar_interval_secs).map_err(|_| {
                    ConfigError::invalid(
                        "bar_interval_secs",
                        "too large to name a Kraken candle interval".into(),
                    )
                })?;
                // Same rule both Kraken endpoints accept; names the allowed set.
                crate::kraken::validate_interval_secs(interval)
                    .map_err(|e| ConfigError::invalid("bar_interval_secs", e.to_string()))?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("symbol", &self.symbol)
            .field("quantity", &self.quantity)
            .field("mode", &self.mode)
            .field("bar_interval_secs", &self.bar_interval_secs)
            .field("series_capacity", &self.series_capacity)
            .field(
                "consecutive_closes_threshold",
                &self.consecutive_closes_threshold,
            )
            .field("starting_balance", &self.starting_balance)
            .field("trade_fee_bps", &self.trade_fee_bps)
            .field("broker_url", &self.broker_url)
            .field(
                "broker_api_key",
                &self.broker_api_key.as_ref().map(|_| "[redacted]"),
            )
            .field("live_feed_host", &self.live_feed_host)
            .field("live_feed_channel", &self.live_feed_channel)
            .field("live_feed_source", &self.live_feed_source)
            .field(
                "massive_api_key",
                &self.massive_api_key.as_ref().map(|_| "[redacted]"),
            )
            .field("live_csv_dir", &self.live_csv_dir)
            .field(
                "telegram_bot_token",
                &self.telegram_bot_token.as_ref().map(|_| "[redacted]"),
            )
            // A chat id is not a credential (Telegram quotes it in its own
            // errors) and it is what makes "is delivery configured?" legible.
            .field("telegram_chat_id", &self.telegram_chat_id)
            .finish()
    }
}

/// Overrides `target` from `PRICE_ACTION_<suffix>` when that variable is set.
fn env_override<T>(env: EnvFn<'_>, suffix: &str, target: &mut T) -> Result<(), ConfigError>
where
    T: std::str::FromStr,
    T::Err: fmt::Display,
{
    let var = format!("{ENV_PREFIX}{suffix}");
    if let Some(raw) = env(&var) {
        let parsed: T = raw
            .parse()
            .map_err(|e: T::Err| ConfigError::invalid(&var, e.to_string()))?;
        *target = parsed;
    }
    Ok(())
}

/// Configuration loading or validation failure.
#[derive(Debug)]
pub enum ConfigError {
    /// The config file could not be read or parsed.
    File {
        /// Path that failed.
        path: String,
        /// What went wrong.
        reason: String,
    },
    /// A value from the environment or the resolved config is invalid.
    Invalid {
        /// Variable or field name.
        source: String,
        /// What went wrong.
        reason: String,
    },
}

impl ConfigError {
    fn file(path: &Path, reason: String) -> Self {
        Self::File {
            path: path.display().to_string(),
            reason,
        }
    }

    fn invalid(source: &str, reason: String) -> Self {
        Self::Invalid {
            source: source.to_string(),
            reason,
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File { path, reason } => write!(f, "config file {path:?}: {reason}"),
            Self::Invalid { source, reason } => {
                write!(f, "invalid config value for '{source}': {reason}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<ConfigError> for crate::error::Error {
    fn from(e: ConfigError) -> Self {
        Self::Config(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Writes a config file to a unique temp path; returns the path.
    fn write_temp_config(contents: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "price-action-config-test-{}-{n}.toml",
            std::process::id()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn env_from(map: BTreeMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
        move |var: &str| map.get(var).map(|v| (*v).to_string())
    }

    fn no_file() -> std::path::PathBuf {
        std::env::temp_dir().join("price-action-config-test-missing.toml")
    }

    #[test]
    fn defaults_apply_with_no_file_no_env() {
        let cfg = Config::load_from(&no_file(), &env_from(BTreeMap::new())).unwrap();
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.mode, Mode::Paper);
    }

    #[test]
    fn file_overrides_defaults() {
        let path = write_temp_config(
            r#"
symbol = "MSFT"
quantity = 10
mode = "paper"
bar_interval_secs = 300
"#,
        );
        let cfg = Config::load_from(&path, &env_from(BTreeMap::new())).unwrap();
        assert_eq!(cfg.symbol, "MSFT");
        assert_eq!(cfg.quantity, 10);
        assert_eq!(cfg.bar_interval_secs, 300);
        // untouched fields keep their defaults
        assert_eq!(cfg.series_capacity, Config::default().series_capacity);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn env_overrides_file() {
        let path = write_temp_config("symbol = \"MSFT\"\nquantity = 10\n");
        let env = env_from(BTreeMap::from([
            ("PRICE_ACTION_SYMBOL", "NVDA"),
            ("PRICE_ACTION_MODE", "paper"),
        ]));
        let cfg = Config::load_from(&path, &env).unwrap();
        assert_eq!(cfg.symbol, "NVDA"); // env wins over file
        assert_eq!(cfg.quantity, 10); // file wins over default
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn invalid_env_value_is_reported_with_var_name() {
        let env = env_from(BTreeMap::from([("PRICE_ACTION_QUANTITY", "lots")]));
        let err = Config::load_from(&no_file(), &env).unwrap_err();
        assert!(err.to_string().contains("PRICE_ACTION_QUANTITY"), "{err}");
    }

    #[test]
    fn unknown_mode_is_rejected() {
        let env = env_from(BTreeMap::from([("PRICE_ACTION_MODE", "yolo")]));
        let err = Config::load_from(&no_file(), &env).unwrap_err();
        assert!(err.to_string().contains("unknown mode"), "{err}");
    }

    #[test]
    fn unknown_file_key_is_rejected() {
        let path = write_temp_config("symbole = \"MSFT\"\n");
        let err = Config::load_from(&path, &env_from(BTreeMap::new())).unwrap_err();
        assert!(err.to_string().contains("symbole"), "{err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn live_mode_requires_broker_url() {
        let env = env_from(BTreeMap::from([("PRICE_ACTION_MODE", "live")]));
        let err = Config::load_from(&no_file(), &env).unwrap_err();
        assert!(err.to_string().contains("broker_url"), "{err}");

        let env = env_from(BTreeMap::from([
            ("PRICE_ACTION_MODE", "live"),
            ("PRICE_ACTION_BROKER_URL", "https://broker.example/api"),
        ]));
        let cfg = Config::load_from(&no_file(), &env).unwrap();
        assert_eq!(cfg.mode, Mode::Live);
    }

    #[test]
    fn zero_quantity_is_rejected() {
        let env = env_from(BTreeMap::from([("PRICE_ACTION_QUANTITY", "0")]));
        let err = Config::load_from(&no_file(), &env).unwrap_err();
        assert!(err.to_string().contains("quantity"), "{err}");
    }

    #[test]
    fn accounting_defaults() {
        let cfg = Config::default();
        assert!((cfg.starting_balance - 10_000.0).abs() < f64::EPSILON * 32.0);
        assert_eq!(cfg.trade_fee_bps, 5);
    }

    #[test]
    fn accounting_settings_file_and_env_precedence() {
        let path = write_temp_config("starting_balance = 25_000\ntrade_fee_bps = 10\n");
        let env = env_from(BTreeMap::from([
            ("PRICE_ACTION_STARTING_BALANCE", "500"),
            ("PRICE_ACTION_TRADE_FEE_BPS", "2"),
        ]));
        let cfg = Config::load_from(&path, &env).unwrap();
        assert!((cfg.starting_balance - 500.0).abs() < f64::EPSILON * 32.0); // env beats file
        assert_eq!(cfg.trade_fee_bps, 2); // env beats file

        let cfg = Config::load_from(&path, &env_from(BTreeMap::new())).unwrap();
        assert!((cfg.starting_balance - 25_000.0).abs() < f64::EPSILON * 32.0); // file beats default
        assert_eq!(cfg.trade_fee_bps, 10);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn zero_or_non_finite_starting_balance_is_rejected() {
        for raw in ["0", "-1", "inf", "NaN"] {
            let env = env_from(BTreeMap::from([("PRICE_ACTION_STARTING_BALANCE", raw)]));
            let err = Config::load_from(&no_file(), &env).unwrap_err();
            assert!(
                err.to_string().contains("starting_balance"),
                "for {raw}: {err}"
            );
        }
    }

    #[test]
    fn live_mode_rejects_whitespace_only_broker_url() {
        let env = env_from(BTreeMap::from([
            ("PRICE_ACTION_MODE", "live"),
            ("PRICE_ACTION_BROKER_URL", "   "),
        ]));
        let err = Config::load_from(&no_file(), &env).unwrap_err();
        assert!(err.to_string().contains("broker_url"), "{err}");
    }

    #[test]
    fn debug_output_redacts_api_key() {
        let env = env_from(BTreeMap::from([(
            "PRICE_ACTION_BROKER_API_KEY",
            "super-secret-value",
        )]));
        let cfg = Config::load_from(&no_file(), &env).unwrap();
        let debug = format!("{cfg:?}");
        assert!(debug.contains("[redacted]"), "{debug}");
        assert!(!debug.contains("super-secret-value"), "{debug}");
        // other fields remain visible for diagnostics
        assert!(debug.contains("symbol"), "{debug}");
    }

    #[test]
    fn debug_output_shows_absent_api_key_as_none() {
        let cfg = Config::load_from(&no_file(), &env_from(BTreeMap::new())).unwrap();
        let debug = format!("{cfg:?}");
        assert!(debug.contains("broker_api_key: None"), "{debug}");
    }

    #[test]
    fn live_settings_defaults_and_layering() {
        let cfg = Config::default();
        assert_eq!(cfg.live_feed_host, "socket.massive.com");
        assert_eq!(cfg.live_feed_channel, crate::feed::FeedChannel::Minute);
        assert_eq!(cfg.live_csv_dir, DEFAULT_LIVE_CSV_DIR);
        assert!(cfg.massive_api_key.is_none());

        let path = write_temp_config(
            r#"
live_feed_host = "delayed.massive.com"
live_feed_channel = "ticks"
live_csv_dir = "/var/tmp/sessions"
massive_api_key = "file-key"
"#,
        );
        let env = env_from(BTreeMap::from([
            ("PRICE_ACTION_LIVE_FEED_HOST", "socket.massive.com"),
            ("PRICE_ACTION_MASSIVE_API_KEY", "env-key"),
        ]));
        let cfg = Config::load_live_from(&path, &env).unwrap();
        assert_eq!(cfg.live_feed_host, "socket.massive.com"); // env beats file
        assert_eq!(cfg.live_feed_channel, crate::feed::FeedChannel::Ticks); // file beats default
        assert_eq!(cfg.live_csv_dir, "/var/tmp/sessions"); // file beats default
        assert_eq!(cfg.massive_api_key.as_deref(), Some("env-key")); // env beats file

        let cfg = Config::load_live_from(&path, &env_from(BTreeMap::new())).unwrap();
        assert_eq!(cfg.live_feed_host, "delayed.massive.com");
        assert_eq!(cfg.massive_api_key.as_deref(), Some("file-key"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn live_without_api_key_is_rejected_and_names_the_setting() {
        let err = Config::load_live_from(&no_file(), &env_from(BTreeMap::new())).unwrap_err();
        assert!(err.to_string().contains("massive_api_key"), "{err}");
        // Blank key is treated as absent.
        let env = env_from(BTreeMap::from([("PRICE_ACTION_MASSIVE_API_KEY", "   ")]));
        let err = Config::load_live_from(&no_file(), &env).unwrap_err();
        assert!(err.to_string().contains("massive_api_key"), "{err}");
    }

    #[test]
    fn live_feed_host_rejects_schemes_and_paths() {
        for bad in [
            "wss://socket.massive.com",
            "socket.massive.com/stocks",
            // `@` would decode as URL userinfo inside the built
            // `wss://<host>/stocks` URL, so it is a non-hostname character.
            "evil@socket.massive.com",
            "user:pass@socket.massive.com",
            "a b",
        ] {
            let env = env_from(BTreeMap::from([
                ("PRICE_ACTION_LIVE_FEED_HOST", bad),
                ("PRICE_ACTION_MASSIVE_API_KEY", "k"),
            ]));
            let err = Config::load_live_from(&no_file(), &env).unwrap_err();
            assert!(
                err.to_string().contains("live_feed_host"),
                "for {bad}: {err}"
            );
        }
    }

    #[test]
    fn unknown_feed_channel_is_rejected() {
        let env = env_from(BTreeMap::from([
            ("PRICE_ACTION_LIVE_FEED_CHANNEL", "candles"),
            ("PRICE_ACTION_MASSIVE_API_KEY", "k"),
        ]));
        let err = Config::load_live_from(&no_file(), &env).unwrap_err();
        assert!(err.to_string().contains("live_feed_channel"), "{err}");
    }

    #[test]
    fn debug_output_redacts_massive_api_key() {
        let env = env_from(BTreeMap::from([(
            "PRICE_ACTION_MASSIVE_API_KEY",
            "feed-secret-value",
        )]));
        let cfg = Config::load_live_from(&no_file(), &env).unwrap();
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("feed-secret-value"), "{debug}");
    }

    #[test]
    fn feed_source_names_parse_case_insensitively() {
        assert!(matches!(
            parse_feed_source("massive"),
            Ok(FeedSource::Massive)
        ));
        assert!(
            matches!(parse_feed_source("  KRAKEN "), Ok(FeedSource::Kraken)),
            "trimmed + case-insensitive"
        );
        let err = parse_feed_source("binance").unwrap_err();
        assert!(err.to_string().contains("live_feed_source"), "{err}");
    }

    #[test]
    fn live_feed_source_defaults_to_massive_and_layers_file_and_env() {
        let cfg = Config::load_from(&no_file(), &env_from(BTreeMap::new())).unwrap();
        assert_eq!(cfg.live_feed_source, FeedSource::Massive);

        // Config file.
        let path = write_temp_config("live_feed_source = \"kraken\"\n");
        let cfg = Config::load_live_from(&path, &env_from(BTreeMap::new())).unwrap();
        assert_eq!(cfg.live_feed_source, FeedSource::Kraken);
        std::fs::remove_file(&path).ok();

        // Env over file. The resolved source becomes Massive, whose live-load
        // path validates its API key — a dummy supplies one so this test
        // exercises layering, not validation (the per-source tests below own
        // that behaviour).
        let path = write_temp_config("live_feed_source = \"kraken\"\n");
        let env = env_from(BTreeMap::from([
            ("PRICE_ACTION_LIVE_FEED_SOURCE", "massive"),
            ("PRICE_ACTION_MASSIVE_API_KEY", "dummy-layering-test"),
        ]));
        let cfg = Config::load_live_from(&path, &env).unwrap();
        assert_eq!(cfg.live_feed_source, FeedSource::Massive);
        std::fs::remove_file(&path).ok();

        // Unknown value is rejected naming the setting.
        let env = env_from(BTreeMap::from([(
            "PRICE_ACTION_LIVE_FEED_SOURCE",
            "bitfinex",
        )]));
        let err = Config::load_live_from(&no_file(), &env).unwrap_err();
        assert!(err.to_string().contains("live_feed_source"), "{err}");
    }

    #[test]
    fn kraken_source_needs_no_key_but_validates_channel_and_candle_interval() {
        // Public endpoints: no API key exists or is read, so `live` loads
        // without one at a valid candle length (60s is the default).
        let env = env_from(BTreeMap::from([
            ("PRICE_ACTION_LIVE_FEED_SOURCE", "kraken"),
            ("PRICE_ACTION_SYMBOL", "BTC/USD"),
            ("PRICE_ACTION_BAR_INTERVAL_SECS", "300"),
        ]));
        assert!(Config::load_live_from(&no_file(), &env).is_ok());

        // The Kraken OHLC channel is minute-granular: no tick equivalent.
        let env = env_from(BTreeMap::from([
            ("PRICE_ACTION_LIVE_FEED_SOURCE", "kraken"),
            ("PRICE_ACTION_SYMBOL", "BTC/USD"),
            ("PRICE_ACTION_LIVE_FEED_CHANNEL", "ticks"),
        ]));
        let err = Config::load_live_from(&no_file(), &env).unwrap_err();
        assert!(err.to_string().contains("live_feed_channel"), "{err}");

        // An arbitrary bar interval is not a Kraken candle length.
        let env = env_from(BTreeMap::from([
            ("PRICE_ACTION_LIVE_FEED_SOURCE", "kraken"),
            ("PRICE_ACTION_SYMBOL", "BTC/USD"),
            ("PRICE_ACTION_BAR_INTERVAL_SECS", "7"),
        ]));
        let err = Config::load_live_from(&no_file(), &env).unwrap_err();
        assert!(err.to_string().contains("bar_interval_secs"), "{err}");

        // The default Massive gate is untouched: it still asks for its key.
        assert!(Config::load_live_from(&no_file(), &env_from(BTreeMap::new())).is_err());
    }
}
