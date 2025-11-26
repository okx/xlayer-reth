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
    @rm -rf .cargo  # Clean dev mode files
    cargo build --release

[no-exit-message]
build-dev reth_path="":
    #!/usr/bin/env bash
    set -e
    
    # If no path provided, check if .cargo/config.toml exists
    if [ -z "{{reth_path}}" ]; then
        if [ -f .cargo/config.toml ]; then
            echo "📦 Using existing .cargo/config.toml"
        else
            echo "⚠️  First time setup needed: just build-dev /absolute/path/to/reth"
            exit 1
        fi
    else
        just check-dev-template
        mkdir -p .cargo
        sed "s|RETH_PATH_PLACEHOLDER|{{reth_path}}|g" .reth-dev.toml > .cargo/config.toml
        echo "Using local reth: {{reth_path}}"
    fi
    
    cargo build --release

# Check dev template has all reth crates
check-dev-template:
    #!/usr/bin/env bash
    M=$(comm -23 <(grep 'git = "https://github.com/okx/reth"' Cargo.toml | grep -oE '^[a-z][a-z0-9-]+' | sort) <(grep 'RETH_PATH_PLACEHOLDER' .reth-dev.toml | grep -oE '^[a-z][a-z0-9-]+' | sort))
    [ -z "$M" ] && echo "✅ Template OK" || (echo "❌ Missing: $M" && exit 1)

build-maxperf:
    RUSTFLAGS="-C target-cpu=native" cargo build --profile maxperf --features jemalloc,asm-keccak

install:
    cargo install --path crates/node --bin xlayer-reth-node --force --locked --profile release

install-maxperf:
    RUSTFLAGS="-C target-cpu=native" cargo install --path crates/node --bin xlayer-reth-node --force --locked --profile maxperf --features jemalloc,asm-keccak

clean:
    cargo clean

build-docker:
    @rm -rf .cargo  # Clean dev mode files
    docker build -t op-reth:latest -f Dockerfile .

[no-exit-message]
build-docker-dev reth_path="":
    #!/usr/bin/env bash
    set -e
    PATH_FILE=".cargo/.reth_source_path"
    # Determine source path: provided > saved > error
    if [ -n "{{reth_path}}" ]; then
        RETH_SRC=$(cd {{reth_path}} && pwd)
    elif [ -f "$PATH_FILE" ]; then
        RETH_SRC=$(cat "$PATH_FILE")
        echo "📦 Using saved path: $RETH_SRC"
    elif [ -d .cargo/reth ]; then
        echo "⚠️  .cargo/reth exists but no source path. Using as-is (may be outdated)"
        echo "   To enable auto-sync: just build-docker-dev /path/to/reth" && RETH_SRC=""
    else
        echo "❌ First time: just build-docker-dev /path/to/reth" && exit 1
    fi
    # Sync if source path exists
    [ -z "$RETH_SRC" ] && just build-docker && exit 0

    just check-dev-template
    mkdir -p .cargo
    
    echo "$RETH_SRC" > "$PATH_FILE"
    echo "📦 Syncing $RETH_SRC → .cargo/reth..."
    rsync -au --delete --exclude='.git' --exclude='target' "$RETH_SRC/" .cargo/reth/
    echo "✅ Sync complete"
    
    # Generate config with /reth path (Docker will move .cargo/reth to /reth to avoid nesting)
    sed "s|RETH_PATH_PLACEHOLDER|/reth|g" .reth-dev.toml > .cargo/config.toml
    echo "🐳 Building Docker image..."
    docker build -t op-reth:latest -f Dockerfile .

watch-test:
    cargo watch -x test

watch-check:
    cargo watch -x "fmt --all -- --check" -x "clippy --all-targets -- -D warnings" -x test

xlayer:
	cp .github/scripts/pre-commit-xlayer .git/hooks/pre-commit && \
	chmod +x .git/hooks/pre-commit
