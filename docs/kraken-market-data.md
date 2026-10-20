---
type: Reference
title: Kraken Market Data & Optional Read-Only Keys
description: How price-action consumes Kraken market data (public REST candles, public WebSocket OHLC), and the strictly optional, environment-injected read-only API-key pair — what the authenticated check does, how to inject the keys with 1Password, and what can never happen with them.
tags: [kraken, market-data, rest, websocket, api-keys, secrets, read-only]
status: draft
sources:
  - id: krakenrs
    resource: /src/kraken.rs
    title: Kraken transport, signing primitives, authenticated_check
  - id: configsrc
    resource: /src/config.rs
    title: kraken_api_key / kraken_private_key fields (environment-only, redacted)
  - id: mainrs
    resource: /src/main.rs
    title: kraken backtest / kraken check-keys subcommands, live-session wiring
generated: { by: pi-agent/qwen3.8-max, at: 2026-10-18T19:30:00Z }
---

# Kraken Market Data & Optional Read-Only Keys

All Kraken **market data** this binary consumes is public and keyless:

* `kraken backtest` — committed candles over REST
  (`https://api.kraken.com/0/public/OHLC`).[^krakenrs]
* `live` with `PRICE_ACTION_LIVE_FEED_SOURCE=kraken` — streaming OHLC over the
  public Spot WebSocket v2 (`wss://ws.kraken.com/v2`).[^krakenrs]

Neither **market-data** request reads, needs or accepts an API key. The
optional pair below is used only by the separate read-only key check
(`kraken check-keys`, or `live`'s one-time startup verification) — never to
fetch candles or subscribe to the stream.[^mainrs]

# `kraken backtest`

```console
$ price-action kraken backtest --pair XBT/USD --interval 300
```

* `--pair` (optional) — any Kraken spelling (`XBT/USD`, `BTC/USD`, `XBTUSD`,
  …); resolved against Kraken's public `AssetPairs`/`Assets` tables so the run
  trades and reports under the resolved **display** form. Defaults to the
  configured `symbol`.
* `--interval` (optional) — candle length in seconds; must name one of
  Kraken's intervals (60/300/900/1800/3600/14400). Defaults to 300.
  Rejected with a config error otherwise.
* Fetches `0/public/OHLC`, **drops the trailing in-progress candle** (only
  committed candles are traded), and feeds the rest through the exact same
  `ReplaySession`/paper account as file replay — the report is the replay
  report shape under the source label `price-action kraken`.
* Transport: hand-rolled HTTP/1.1 GET over rustls (no HTTP-client crate,
  matching the repository rule), 30 s request timeout, chunked bodies
  reassembled before JSON decoding; Kraken business-error arrays surface as
  `MarketData` errors naming the endpoint.

# The optional key pair

An operator **may** inject a Kraken trade-API key pair for exactly one purpose:
a read-only sanity check that the credentials are valid before a long-running
session or before trusting them elsewhere. There is no other use in this code
base.[^configsrc]

| Env variable | Config field (env-only) | Meaning |
|---|---|---|
| `PRICE_ACTION_KRAKEN_API_KEY` | `kraken_api_key` | The public key identifier |
| `PRICE_ACTION_KRAKEN_PRIVATE_KEY` | `kraken_private_key` | The base64 secret that signs requests |

Rules, enforced in code:[^configsrc][^krakenrs]

* **Environment-only.** `FileConfig` rejects `kraken_api_key` /
  `kraken_private_key` in `config.toml` outright — a committed file can never
  carry them.
* **Both or neither.** Half a pair is a configuration error, both in
  `kraken check-keys` and at `live` startup.
* **Never printed at all.** `Debug` shows `[redacted]` for both halves, and
  no output — logs, errors, reports or the check's success line — displays
  any part of either value, not even a prefix. The secret is used only as
  HMAC key material.
* **Read-only by construction.** The only private endpoint this binary ever
  calls is `POST /0/private/Balance` (Kraken's private REST is POST-only;
  Balance reads state and changes nothing). There is no order-placement code path in
  the repository — execution is paper-only in every mode — so even a key with
  trade permissions cannot cause an order from here. Kraken-side, create the
  key with *no* permissions beyond what Balance needs (funds/query) and no
  withdrawal rights.

# What the check does

`kraken::authenticated_check` builds the POST body once — `nonce=<n>`, the
nonce being Unix **milliseconds** forced strictly increasing per process
(Kraken rejects a reused nonce with `EAPI:Invalid nonce`) — signs it per
Kraken's scheme, and issues `POST /0/private/Balance`:[^krakenrs]

`API-Sign = base64(HMAC-SHA512(key = base64_decode(secret), msg = URI path +
SHA-256(nonce + POST data) as raw digest bytes))`

The identical body bytes go into the signature and onto the wire. On success
the check returns a one-line summary — how many asset balances are visible —
carrying **no key material** (not even a prefix). Balance **values** are
never surfaced anywhere.

# Running it

```console
$ op run --env-file=.env.tpl -- price-action kraken check-keys
kraken read-only key check OK: 2 asset balance(s) visible
no order was sent — this binary is paper-only in every mode
```

* **Both variables unset** → exit 1 with a config error naming both variables.
* **Exactly one set** → exit 1: "the pair must be injected together".
* **Invalid/revoked key** → exit 1 with Kraken's authentication refusal (its
  `EAPI:Invalid key` / `EAPI:Invalid signature` business errors, surfaced as
  a market-data error); nothing secret appears in the message.
* **Unusable secret** (not standard base64) → exit 1 config error *before any
  network call*.

The signing primitives are unit-tested known-answers (RFC 4648 base64
vectors; HMAC-SHA512 signatures over an obviously fake secret, computed
independently and cross-checked against CCXT's Kraken `sign()` reference
implementation; request-bytes shape and nonce-monotonicity tests), so the
scheme is verified without any network access.[^krakenrs]

## `live` startup behaviour

With `PRICE_ACTION_LIVE_FEED_SOURCE=kraken`:[^mainrs]

* neither variable set → the session starts normally on public data;
* both set → the check runs once at startup: success prints
  `kraken read-only key check OK: <summary>` (no key material), failure
  prints a `WARNING` line
  and the session **continues on public market data** (the stream does not
  depend on the keys);
* half a pair → the session refuses to start (config error).

# Injecting the keys with 1Password (`op run`)

Store the two values as fields of one 1Password item (e.g. a *Login* named
`price-action-kraken`, fields `PRICE_ACTION_KRAKEN_API_KEY` and
`PRICE_ACTION_KRAKEN_PRIVATE_KEY`), then reference them from an env template.
The values are never exported into your (parent) shell, written to a file, or
visible in shell history: `op run` places them only in the environment of the
one spawned command. (Any child processes that command itself spawns inherit
them, like any environment variable.)

```ini
# .env.tpl — committed; contains references, never values
PRICE_ACTION_LIVE_FEED_SOURCE=kraken
PRICE_ACTION_KRAKEN_API_KEY=op://Vault/price-action-kraken/PRICE_ACTION_KRAKEN_API_KEY
PRICE_ACTION_KRAKEN_PRIVATE_KEY=op://Vault/price-action-kraken/PRICE_ACTION_KRAKEN_PRIVATE_KEY
```

```console
$ op run --env-file=.env.tpl -- price-action kraken check-keys
$ op run --env-file=.env.tpl -- price-action live
```

Do **not** `export` the variables into a long-lived shell, put them in
`config.toml`, `.env` files with real values, docker-compose files or CI
variables that get echoed. `op run` injects them only into the spawned
process (and its children) — never into the parent shell.

# Maintenance

Any change to the signing scheme, the check's behaviour or these two
configuration fields must update this page and `docs/log.md` in the same PR
(see [Bundle Update Guide](bundle-update-guide.md) and `AGENTS.md`).

[^krakenrs]: `/src/kraken.rs` — `api_sign`, `sha256_raw`, `next_nonce_ms`,
    `api_sign_request_bytes`, `http_private_post`, `base64_encode`/`base64_decode`,
    `authenticated_check` and their known-answer tests.
[^configsrc]: `/src/config.rs` — field docs, env reading, `FileConfig`
    rejection and redacting `Debug`.
[^mainrs]: `/src/main.rs` — `run_kraken_check_keys`,
    `verify_kraken_keys_if_configured`, usage line.
