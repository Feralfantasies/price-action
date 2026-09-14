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
| `src/error.rs` | Shared error type |

Market-data source adapters (broker/venue APIs) are not part of the initial
scaffold.

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
