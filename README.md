# price-action

> ⚠️ **DISCLAIMER — READ BEFORE USE.** This software is for **educational and
> research purposes only** and is **not financial advice**. Automated trading
> involves substantial risk of loss, including loss of your entire capital.
> You use it **entirely at your own risk** and are solely responsible for any
> orders placed or losses incurred. The authors accept **no liability** for
> any damages or trading losses. See [DISCLAIMER.md](DISCLAIMER.md).

Automated price-action trading in Rust: strategies driven by raw price
movement (bars), not by derived indicators. A built-in **replay** mode lets
you check how the configured strategy reacts to historic bar data before that
strategy is trusted anywhere near real money — this is always step 1.

## Quick start: verify settings against historic data

Replay is the recommended way to answer *"do these settings behave the way I
expect on real market history?"*. It feeds a CSV of OHLCV bars through the
exact engine + strategy used in live mode — but **always** against an
in-memory `PaperBroker`, regardless of what mode the configuration sets, and
prints every signal.

### 1. Build it locally

The only requirement is [Rust](https://rustup.rs) (`rustup` installs it in one
command); there are no other runtime or build dependencies:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"      # once per shell, or add to your profile
```

Clone and smoke-test the app (no arguments just loads config and reports that
the engine is ready):

```sh
git clone git@github.com:Feralfantasies/price-action.git
cd price-action
cargo run
# price-action: symbol=AAPL mode=Paper quantity=1 bar_interval=60s threshold=3
# price-action: engine ready, last signal = Flat (no market-data source configured yet)
# hint: try `price-action replay samples/sample-bars.csv` for a worked example
```

### 2. Replay the bundled sample data

A small sample file is committed at [`samples/sample-bars.csv`](samples/sample-bars.csv) —
25 fifteen-minute bars (900-second timestamp increments) with a realistic
shape (an up-leg, a pullback, then recovery).
**It is synthetic demo data shaped like real trades, not actual exchange
prices**; use it to learn the workflow, then point replay at genuine history
(Step 4) to verify settings for real.

```sh
cargo run -- replay samples/sample-bars.csv
```

Expected output (defaults: `consecutive-closes` strategy with threshold 3):

```text
price-action replay - 25 bars
symbol=AAPL mode=Paper quantity=1 strategy=consecutive-closes threshold=3
------------------------------------------------------------------------------
t=1725458400    o=211.98    h=212.97    l=211.59    c=212.50    v=  45731000 -> Flat
t=1725459300    o=212.50    h=213.41    l=212.22    c=213.10    v=  46462000 -> Flat
...
t=1725461100    o=213.90    h=214.86    l=213.62    c=214.55    v=  47924000 -> Long   (entry)
t=1725462000    o=214.55    h=215.49    l=214.16    c=215.02    v=  48655000 -> Long
t=1725462900    o=215.02    h=215.33    l=213.92    c=214.20    v=  49386000 -> Flat
...
result: signal=Flat entries=2 of 25 bars - paper execution only, no orders placed
```

How to read it:

- One line per bar in file order, with OHLCV and the strategy's **signal after
  that bar** (`Flat`, `Long` or `Short`).
- `(entry)` marks transitions from flat into a position — i.e. actual trade
  events. The footer counts them: `entries=2 of 25 bars`.
- Replay always executes on an in-memory `PaperBroker` regardless of the
  configured mode — it never places or simulates costed orders; it reports
  what the strategy *would* decide. General configuration validation still
  runs before any bar is fed (invalid symbol, quantity or threshold are
  refused just like for a real run), but because paper-only execution needs
  no venue, replay takes a config path that skips the live-mode `broker_url`
  requirement — so no broker setup is ever needed to replay.

### 3. Verify how a setting changes behaviour

Settings are layered (see [Configuration](#configuration)); environment
variables win over everything, which makes A/B-testing on the same dataset
trivial — run the same file twice with different variables:

```sh
# Default threshold (signal after 3 consecutive higher/lower closes)
PRICE_ACTION_SYMBOL=AAPL cargo run -- replay samples/sample-bars.csv | tail -1
# result: signal=Flat entries=2 of 25 bars ...

# Stricter: require 5 in a row — fewer, later entries
PRICE_ACTION_SYMBOL=AAPL PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD=5 \
  cargo run -- replay samples/sample-bars.csv | tail -1
# result: signal=Flat entries=0 of 25 bars ...

# Loose: one bar decides
PRICE_ACTION_SYMBOL=AAPL PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD=1 \
  cargo run -- replay samples/sample-bars.csv | grep "(entry)"
```

Diffing the full traces (`tee` each run to a file) is also exactly what you
want for comparing two config files or two datasets:

```sh
cargo run -- replay samples/sample-bars.csv > trace-default.txt
PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD=2 cargo run -- replay samples/sample-bars.csv > trace-t2.txt
diff -u trace-default.txt trace-t2.txt    # shows exactly when signals diverge
```

This is the intended loop: *change one setting → replay the same historic data
→ confirm the signal behaviour moved in a way you can explain.* If it doesn't,
the setting (or your strategy expectation) is wrong before any market is even
connected.

### 4. Replay genuine historic data

The bar file format is plain CSV: a header line
`timestamp,open,high,low,close,volume`, then one row per bar. `timestamp` is
**Unix seconds**; prices/volume are decimal numbers. Any order of bars works as
long as you keep the series in **chronological order** (the strategy compares
each close with the previous close, so shuffled rows mean nothing).

Two ways to obtain real data (adapt to your exchange/venue):

- **Exchange or broker CSV export**: most brokers can export historical daily
  bars. Convert an ISO-date + OHLCV export (`Date,Open,High,Low,Close,Volume`)
  with GNU `date`, keeping one line per row, e.g.:

  ```sh
  tail -n +2 broker-export.csv \
    | awk -F, '{
        cmd = "date -u -d \"" $1 "\" +%s"; cmd | getline ts; close(cmd);
        print ts \",\" $2 \",\" $3 \",\" $4 \",\" $5 \",\" $6 }' > aapl-daily.csv
  ```

- **Free historic CSV sources** (e.g. [stooq.com](https://stooq.com) `q/d/l/`
  downloads for US daily bars, format `Date,Open,High,Low,Close,Volume`) convert
  with the same one-liner above. Verify a couple of rows against the exchange's
  published values before trusting a long backtest.

Then run it:

```sh
cargo run -- replay aapl-daily.csv
```

Good hygiene when validating settings this way:

- Use a dataset that *contains both* conditions you care about (trending and
  choppier periods); 25 bars shows mechanics, not edge cases.
- If a threshold is per-bar-interval (e.g. "3 consecutive closes"), make sure
  your bars match the interval the strategy assumes — daily vs hourly data
  changes what "consecutive" means to a strategy.
- Keep replay output around when debugging: it is a complete audit trail of
  every decision in file order.

### 5. Run the same checks from Docker (optional)

The container works identically; mount your bar file read-only:

```sh
docker build -t price-action .
docker run --rm \
  -v "$PWD/aapl-daily.csv:/data/aapl-daily.csv:ro" \
  -e PRICE_ACTION_SYMBOL=AAPL \
  -e PRICE_ACTION_CONSECUTIVE_CLOSES_THRESHOLD=3 \
  price-action replay /data/aapl-daily.csv
```

## Layout

| Module | Purpose |
|---|---|
| `src/market.rs` | `Bar` (OHLCV) and `BarSeries` rolling window |
| `src/strategy.rs` | `Signal`, the `Strategy` trait, example `ConsecutiveCloses` strategy |
| `src/execution.rs` | `Broker` trait and in-memory `PaperBroker` |
| `src/engine.rs` | The trading loop: bar → signal → broker position |
| `src/csv.rs` | OHLCV CSV reading/writing (the replay data format) |
| `src/replay.rs` | Replay runner + human-readable report |
| `src/config.rs` | Layered configuration: env → config file → defaults |
| `src/error.rs` | Shared error type |

Market-data source adapters (broker/venue APIs, live feeds) are not part of the
initial scaffold; replay is how you exercise the full pipeline offline until
they exist. Live trading (`mode = "live"`) is also **not implemented yet** and
the binary refuses to run it — paper only, by design, for now.

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

`PRICE_ACTION_SYMBOL`, `QUANTITY`, `MODE` and `BAR_INTERVAL_SECS` are cosmetic
during replay (there are no orders); `CONSECUTIVE_CLOSES_THRESHOLD` is the one
that changes what you see, which is why the Step 3 example toggles it.

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
  -v "$PWD/aapl-daily.csv:/data/bars.csv:ro" \
  -e PRICE_ACTION_SYMBOL=MSFT \
  -e PRICE_ACTION_MODE=paper \
  price-action replay /data/bars.csv
```

Released images are published to GitHub Container Registry on every green CI
run of `main` (auto-tagged semver + `latest`): pull with e.g.
`docker pull ghcr.io/feralfantasies/price-action:latest`. Configuration is
supplied at runtime via `PRICE_ACTION_*` env vars and/or a TOML file mounted at
the path named by `PRICE_ACTION_CONFIG`; nothing is baked into the image.

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

Continuous integration (fmt, clippy with `-D warnings`, tests, musl static
build assertion, cargo-audit, Trivy scans incl. the built scratch image) runs
on every push; release containers are built, scanned and pushed to `ghcr.io`
automatically from `main`.

## License

MIT — see [LICENSE](LICENSE). Use is additionally subject to
[DISCLAIMER.md](DISCLAIMER.md).
