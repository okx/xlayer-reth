FROM office-registry-vpc.cn-hongkong.cr.aliyuncs.com/inner-public/cargo-chef/latest-rust-1.92/cargo-chef:latest AS chef
WORKDIR /app
LABEL org.opencontainers.image.source=https://github.com/xlayer/xlayer-reth
LABEL org.opencontainers.image.licenses="MIT OR Apache-2.0"
RUN apt-get update && apt-get -y upgrade && apt-get install -y libclang-dev pkg-config

# Pre-install the toolchain pinned by rust-toolchain.toml (the repo pins 1.94, the
# base image ships 1.92). Done as its own cached layer keyed only on
# rust-toolchain.toml so cook and the final build use the SAME toolchain (otherwise
# cook compiles with 1.92 and the post-`COPY . .` build re-fingerprints on 1.94,
# busting cargo-chef's dependency cache — or fails if 1.94 cannot be fetched late).
COPY rust-toolchain.toml .
RUN rustup show

# Builds a cargo-chef plan
FROM chef AS planner
COPY . .
# Remove test-only workspace members that are excluded from the Docker context
RUN sed -i '/"crates\/tests"/d' Cargo.toml
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# REQUIRED before `cargo chef cook`: the workspace has a `[patch.crates-io]` entry
#   libp2p-yamux = { path = "deps/libp2p-yamux" }
# (a security fork that drops vulnerable yamux 0.12.x — CVE-2026-32314). cargo-chef
# recreates `[dependencies]` path crates from the recipe, but NOT `[patch]` path
# crates, so cook must see the real `deps/` on disk. Without this copy cook fails
# with: failed to read `/app/deps/libp2p-yamux/Cargo.toml`: No such file or directory.
COPY deps/ deps/

ARG BUILD_PROFILE=release
ENV BUILD_PROFILE=$BUILD_PROFILE
ARG RUSTFLAGS=""
ENV RUSTFLAGS="$RUSTFLAGS"
ARG FEATURES=""
ENV FEATURES=$FEATURES
RUN cargo chef cook --profile $BUILD_PROFILE --features "$FEATURES" --recipe-path recipe.json --manifest-path /app/bin/node/Cargo.toml
COPY . .
RUN sed -i '/"crates\/tests"/d' Cargo.toml
RUN cargo build --profile $BUILD_PROFILE --bin xlayer-reth-node --manifest-path /app/bin/node/Cargo.toml
# Copy binary to a fixed location
RUN OUTPUT_DIR=$(if [ "$BUILD_PROFILE" = "dev" ] || [ "$BUILD_PROFILE" = "test" ]; then echo debug; else echo "$BUILD_PROFILE"; fi) && \
    cp /app/target/$OUTPUT_DIR/xlayer-reth-node /app/op-reth

FROM office-registry-vpc.cn-hongkong.cr.aliyuncs.com/okbase/ubuntu:24.04-okg1-sec20251110 AS runtime
RUN apt-get update && \
    apt-get install -y ca-certificates libssl-dev pkg-config strace curl && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/op-reth /usr/local/bin/
RUN chmod +x /usr/local/bin/op-reth
COPY LICENSE* ./
EXPOSE 30303 30303/udp 9001 8545 8546 7545 8551
