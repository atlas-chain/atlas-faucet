# syntax=docker/dockerfile:1

FROM rust:1.96-bookworm AS builder
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release && cp target/release/atlas-faucet /usr/local/bin/atlas-faucet

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /usr/local/bin/atlas-faucet /usr/local/bin/atlas-faucet

ENV LISTEN_HOST=0.0.0.0 \
    LISTEN_PORT=28884 \
    HTML_TITLE="Atlas Faucet" \
    RPC_URL="http://host.docker.internal:8545" \
    FAUCET_DRIP_WEI=1000000000000000000 \
    POW_BITS=16 \
    POW_PUZZLES=480 \
    FAUCET_QUEUE_CAPACITY=2 \
    FAUCET_COOLDOWN_SECS=60

EXPOSE 28884
ENTRYPOINT ["atlas-faucet"]
