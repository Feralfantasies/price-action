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
generated: { by: pi-agent/qwen3.8-max, at: 2026-10-13T00:00:00Z }
---

# Kraken Market Data & Optional Read-Only Keys

All Kraken **market data** this binary consumes is public and keyless:

* `kraken backtest` — committed candles over REST
  (`https://api.kraken.com/0/public/OHLC`).[^krakenrs]
* `live` with `PRICE_ACTION_LIVE_FEED_SOURCE=kraken` — streaming OHLC over the
  public Spot WebSocket v2 (`wss://ws.kraken.com`).[^krakenrs]

Neither path reads, needs or accepts an API key.[^mainrs]

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
* **Redacted everywhere.** `Debug` prints `[redacted]`; logs, errors and
  reports show at most the first six characters of the key followed by `…`
  (`mask_key`). The secret is used only as HMAC key material and never
  printed at all.
* **Read-only by construction.** The only private endpoint this binary ever
  calls is `GET /0/private/Balance`. There is no order-placement code path in
  the repository — execution is paper-only in every mode — so even a key with
  trade permissions cannot cause an order from here. Kraken-side, create the
  key with *no* permissions beyond what Balance needs (funds/query) and no
  withdrawal rights.

# What the check does

`kraken::authenticated_check` signs one request per Kraken's scheme —
`API-Sign = base64(HMAC-SHA512(key = base64_decode(secret), msg = body +
hex(SHA-256(nonce))))`, nonce = current Unix seconds — and issues
`GET /0/private/Balance`.[^krakenrs] On success it returns a one-line masked
summary (key prefix + how many asset balances are visible). Balance **values**
are never surfaced anywhere.

# Running it

```console
$ op run --env-file=.env.tpl -- price-action kraken check-keys
kraken read-only key check OK: ABCDEF… — 2 asset balance(s) visible
no order was sent — this binary is paper-only in every mode
```

* **Both variables unset** → exit 1 with a config error naming both variables.
* **Exactly one set** → exit 1: "the pair must be injected together".
* **Invalid/revoked key** → exit 1 with the transport-level Kraken refusal
  (e.g. a 404 or `EAPI:Invalid key`); nothing secret appears in the message.
* **Unusable secret** (not standard base64) → exit 1 config error *before any
  network call*.

The signing primitives are unit-tested known-answers (RFC 4648 base64
vectors, FIPS 180-4 SHA-256 vectors, an independently computed HMAC-SHA512
signature with an obviously fake secret), so the scheme is verified without
any network access.[^krakenrs]

## `live` startup behaviour

With `PRICE_ACTION_LIVE_FEED_SOURCE=kraken`:[^mainrs]

* neither variable set → the session starts normally on public data;
* both set → the check runs once at startup: success prints
  `kraken read-only key check OK: <masked>`, failure prints a `WARNING` line
  and the session **continues on public market data** (the stream does not
  depend on the keys);
* half a pair → the session refuses to start (config error).

# Injecting the keys with 1Password (`op run`)

Store the two values as fields of one 1Password item (e.g. a *Login* named
`price-action-kraken`, fields `PRICE_ACTION_KRAKEN_API_KEY` and
`PRICE_ACTION_KRAKEN_PRIVATE_KEY`), then reference them from an env template
so the values never appear in shell history, files or the process
environment of anything but the one command:

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
process.

# Maintenance

Any change to the signing scheme, the check's behaviour or these two
configuration fields must update this page and `docs/log.md` in the same PR
(see [Bundle Update Guide](bundle-update-guide.md) and `AGENTS.md`).

[^krakenrs]: `/src/kraken.rs` — `api_sign`, `base64_encode`/`base64_decode`,
    `sha2_digest_hex`, `http_private_get`, `authenticated_check`, `mask_key`
    and their known-answer tests.
[^configsrc]: `/src/config.rs` — field docs, env reading, `FileConfig`
    rejection and redacting `Debug`.
[^mainrs]: `/src/main.rs` — `run_kraken_check_keys`,
    `verify_kraken_keys_if_configured`, usage line.
