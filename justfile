default:
    @just --list

check: check-format check-clippy test

fix: fix-format fix-clippy

# Run `just test true` to run e2e tests.
test include_e2e="false":
    @echo "Running tests (include_e2e={{include_e2e}})"
    @if [ "{{include_e2e}}" = "true" ]; then \
        cargo test --workspace --all-features; \
    else \
        cargo test --workspace --exclude xlayer-e2e-test --all-features; \
    fi

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

