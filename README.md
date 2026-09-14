# price-action

> ⚠️ **DISCLAIMER — READ BEFORE USE.** This software is for **educational and
> research purposes only** and is **not financial advice**. Automated trading
> involves substantial risk of loss, including loss of your entire capital.
> You use it **entirely at your own risk** and are solely responsible for any
> orders placed or losses incurred. The authors accept **no liability** for
> any damages or trading losses. See [DISCLAIMER.md](DISCLAIMER.md).

Automated price-action trading in Rust: strategies driven by raw price
movement (bars), not by derived indicators.

## Layout

| Module | Purpose |
|---|---|
| `src/market.rs` | `Bar` (OHLCV) and `BarSeries` rolling window |
| `src/strategy.rs` | `Signal`, the `Strategy` trait, example `ConsecutiveCloses` strategy |
| `src/execution.rs` | `Broker` trait and in-memory `PaperBroker` |
| `src/engine.rs` | The trading loop: bar → signal → broker position |
| `src/config.rs` | Layered configuration: env → config file → defaults |
| `src/error.rs` | Shared error type |

Market-data source adapters (broker/venue APIs) are not part of the initial
scaffold.

## Configuration

Trading options resolve in strict precedence order — **environment variables
override the config file, which overrides compiled defaults**:

1. **Environment variables** (`PRICE_ACTION_*`) — highest priority.
2. **Config file** — TOML at the path in `PRICE_ACTION_CONFIG` (default
   `price-action.toml`). A missing file is not an error. See
   [`price-action.example.toml`](price-action.example.toml).
3. **Compiled defaults** — `Config::default()`.

| Setting | Env var | Config-file key | Default |
|---|---|---|---|
| Instrument symbol | `PRICE_ACTION_SYMBOL` | `symbol` | `AAPL` |
| Quantity per trade | `PRICE_ACTION_QUANTITY` | `quantity` | `1` |
| Trading mode | `PRICE_ACTION_MODE` | `mode` | `paper` |
| Bar interval (secs) | `PRICE_ACTION_BAR_INTERVAL_SECS` | `bar_interval_secs` | `60` |
| Rolling-window size | `PRICE_ACTION_SERIES_CAPACITY` | `series_capacity` | `500` |
| Strategy threshold | `PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD` | `consecutive_closes_threshold` | `3` |
| Broker API base URL | `PRICE_ACTION_BROKER_URL` | `broker_url` | _(unset)_ |
| Broker API key | `PRICE_ACTION_BROKER_API_KEY` | `broker_api_key` | _(unset)_ |
| Config-file path | `PRICE_ACTION_CONFIG` | — | `price-action.toml` |

Unknown config-file keys are rejected (typos fail fast). `live` mode requires
`broker_url`; an empty variable is treated as unset. Prefer the environment or
a mounted secret for `broker_api_key` rather than committing it to a file.

## Container deployment

The project is built to run **`FROM scratch`** — the final image contains only
a statically linked binary. See [`Dockerfile`](Dockerfile). Two rules keep the
project self-contained enough for that:

- **Statically linked.** The binary is built for `x86_64-unknown-linux-musl`,
  because `scratch` ships no libc.
- **`rustls`, never OpenSSL.** Any crate that does TLS/HTTPS must be pure-Rust
  `rustls` with bundled `webpki` roots — `scratch` has no OpenSSL and no
  CA-certificate store. When adding an HTTP client, depend on it like
  larderly does:

  ```toml
  reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }
  ```

  Always disable default features and opt into the `rustls` variant; never pull
  in `native-tls`/`openssl`.

Build and run:

```sh
docker build -t price-action .
docker run --rm \
  -e PRICE_ACTION_SYMBOL=MSFT \
  -e PRICE_ACTION_MODE=paper \
  price-action
```

Configuration is supplied at runtime via `PRICE_ACTION_*` env vars and/or a
TOML file mounted at the path named by `PRICE_ACTION_CONFIG`; nothing is baked
into the image.

## Code quality

The project is strict about what counts as passing code:

- `[workspace.lints.clippy]` in `Cargo.toml` denies clippy's `pedantic` group
  and a set of panic-prevention restriction lints (`unwrap_used`,
  `expect_used`, `indexing_slicing`, `panic`, `todo`, ...).
- `clippy.toml` relaxes those panic lints **in tests only**.

```sh
cargo fmt --check
cargo clippy --all-targets
cargo test
```

## License

MIT — see [LICENSE](LICENSE). Use is additionally subject to
[DISCLAIMER.md](DISCLAIMER.md).
