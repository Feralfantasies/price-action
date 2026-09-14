# price-action — scratch-friendly container image.
#
# The final image is `FROM scratch`: it contains only the statically linked
# binary. This requires:
#   - building for x86_64-unknown-linux-musl (scratch has no libc), and
#   - any TLS in dependencies must be rustls with bundled webpki roots
#     (scratch has no CA-certificate store and no openssl).

FROM rust:1-bookworm AS builder

RUN apt-get update \
  && apt-get install -y --no-install-recommends musl-tools \
  && rm -rf /var/lib/apt/lists/* \
  && rustup target add x86_64-unknown-linux-musl

WORKDIR /build

# Cache dependencies by building stub sources against the real manifests first.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
  && echo 'fn main() {}' > src/main.rs \
  && touch src/lib.rs \
  && cargo build --release --target x86_64-unknown-linux-musl \
  && rm -rf src

# Build the real binary.
COPY src src
RUN touch src/main.rs src/lib.rs \
  && cargo build --release --target x86_64-unknown-linux-musl

FROM scratch

COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/price-action /price-action

# Configuration is supplied at runtime via PRICE_ACTION_* environment
# variables and/or a TOML file mounted at the path in PRICE_ACTION_CONFIG
# (see price-action.example.toml). Nothing is baked into the image.

ENTRYPOINT ["/price-action"]
