FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

LABEL org.opencontainers.image.source=https://github.com/xlayer/xlayer-reth
LABEL org.opencontainers.image.licenses="MIT OR Apache-2.0"

RUN apt-get update && apt-get -y upgrade && apt-get install -y libclang-dev pkg-config

# Builds a cargo-chef plan
FROM chef AS planner
COPY . .
# For dev builds: move .cargo/reth to /reth (same level as /app, not nested)
# This avoids nested workspace issues. Create empty /reth if not dev mode.
RUN if [ -d .cargo/reth ]; then \
        mv .cargo/reth /reth; \
    else \
        mkdir -p /reth; \
    fi
# Temporarily disable .cargo/config.toml during cargo chef prepare
# to avoid path conflicts (it will be restored and copied to builder stage)
RUN if [ -f .cargo/config.toml ]; then mv .cargo/config.toml .cargo/config.toml.bak; fi
RUN mkdir -p .cargo
RUN cargo chef prepare --recipe-path recipe.json
RUN if [ -f .cargo/config.toml.bak ]; then mv .cargo/config.toml.bak .cargo/config.toml; fi

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
COPY --from=planner /app/.cargo /app/.cargo
# Copy /reth from planner (will be empty dir in prod mode, reth source in dev mode)
COPY --from=planner /reth /reth

ARG BUILD_PROFILE=release
ENV BUILD_PROFILE=$BUILD_PROFILE

ARG RUSTFLAGS=""
ENV RUSTFLAGS="$RUSTFLAGS"

ARG FEATURES=""
ENV FEATURES=$FEATURES

RUN cargo chef cook --profile $BUILD_PROFILE --features "$FEATURES" --recipe-path recipe.json --manifest-path /app/bin/node/Cargo.toml

COPY . .
RUN cargo build --profile $BUILD_PROFILE --bin xlayer-reth-node --manifest-path /app/bin/node/Cargo.toml

# Copy binary to a fixed location
RUN OUTPUT_DIR=$(if [ "$BUILD_PROFILE" = "dev" ] || [ "$BUILD_PROFILE" = "test" ]; then echo debug; else echo "$BUILD_PROFILE"; fi) && \
    cp /app/target/$OUTPUT_DIR/xlayer-reth-node /app/op-reth

FROM ubuntu:24.04 AS runtime

RUN apt-get update && \
    apt-get install -y ca-certificates libssl-dev pkg-config strace curl && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/op-reth /usr/local/bin/
RUN chmod +x /usr/local/bin/op-reth
COPY LICENSE-* ./

EXPOSE 30303 30303/udp 9001 8545 8546 7545 8551
ENTRYPOINT ["/usr/local/bin/op-reth"]
