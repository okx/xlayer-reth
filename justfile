default:
    @just --list

check: check-format check-clippy test

fix: fix-format fix-clippy

test:
    cargo test --workspace --all-features

check-format:
    cargo fmt --all -- --check

fix-format:
    cargo fix --allow-dirty --allow-staged
    cargo fmt --all

check-clippy:
    cargo clippy --all-targets -- -D warnings

fix-clippy:
    cargo clippy --all-targets --fix --allow-dirty --allow-staged

build:
    cargo build --release

build-maxperf:
    RUSTFLAGS="-C target-cpu=native" cargo build --profile maxperf --features jemalloc,asm-keccak

install:
    cargo install --path crates/node --bin xlayer-reth-node --force --locked --profile release

install-maxperf:
    RUSTFLAGS="-C target-cpu=native" cargo install --path crates/node --bin xlayer-reth-node --force --locked --profile maxperf --features jemalloc,asm-keccak

clean:
    cargo clean

watch-test:
    cargo watch -x test

watch-check:
    cargo watch -x "fmt --all -- --check" -x "clippy --all-targets -- -D warnings" -x test

