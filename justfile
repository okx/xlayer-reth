alias c := check
alias f := fix
alias t := test
alias ff := fix-format
alias cf := check-format
alias cc := check-clippy
alias fc := fix-clippy
alias b := build
alias bm := build-maxperf
alias bt := build-tools
alias btm := build-tools-maxperf
alias i := install
alias im := install-maxperf
alias it := install-tools
alias itm := install-tools-maxperf
alias cl := clean
alias docker := build-docker
alias wt := watch-test
alias wc := watch-check
alias xl := xlayer
alias sc := sweep-check

default:
    @just --list

# Runs target checks on all crates, except [crate1], [crate2], ...
sweep-check *crates="":
    #!/usr/bin/env bash
    set -e
    # Check all local crates, skipping any specified in the parameters.
    if [ -z "{{crates}}" ]; then
        echo "📦 Checking all local crates..."
        cargo metadata --no-deps --format-version 1 | \
        jq -r '.packages[] | select(.source == null) | .name' | \
        xargs -I {} sh -c 'echo "=== Checking {} ===" && cargo check -p {} || exit 255'
    else
        echo "📦 Checking all local crates except: {{crates}}"
        # Get all local crates
        all_crates=$(cargo metadata --no-deps --format-version 1 | \
            jq -r '.packages[] | select(.source == null) | .name')

        # Convert skip list to array
        skip_crates=({{crates}})

        # Check each crate unless it's in the skip list
        for crate in $all_crates; do
            skip=false
            for skip_crate in "${skip_crates[@]}"; do
                if [ "$crate" = "$skip_crate" ]; then
                    echo "⏭️  Skipping $crate"
                    skip=true
                    break
                fi
            done
            if [ "$skip" = false ]; then
                echo "=== Checking $crate ==="
                cargo check -p "$crate" || exit 255
            fi
        done
    fi

check:
    # Upstream flashblocks inner dependency reth-optimism-primitives does not
    # specify feats reth-codec and serde-bincode-compat. So we skip.
    just sweep-check
    just check-format
    just check-clippy
    just test

fix: fix-format fix-clippy

# Run `just test true` to run e2e tests.
test include_e2e="false" include_flashblocks="false":
    #!/usr/bin/env bash
    set -e
    if cargo nextest --version &>/dev/null; then
        CMD="nextest run" E2E_FLAGS="--test-threads 1 --no-capture"
    else
        CMD="test" E2E_FLAGS="-- --nocapture --test-threads=1"
    fi
    echo "Running tests via cargo $CMD (include_e2e={{include_e2e}})"
    cargo $CMD --workspace --exclude xlayer-e2e-test --all-features
    if [ "{{include_e2e}}" = "true" ]; then
        cargo $CMD -p xlayer-e2e-test --test e2e_tests $E2E_FLAGS
    fi
    if [ "{{include_flashblocks}}" = "true" ]; then
        cargo $CMD -p xlayer-e2e-test --test flashblocks_tests $E2E_FLAGS
    fi

check-format:
    cargo +nightly fmt --all -- --check

fix-format:
    cargo fix --allow-dirty --allow-staged
    cargo +nightly fmt --all

check-clippy:
    cargo clippy --all-targets --workspace -- -D warnings

fix-clippy:
    cargo clippy --all-targets --workspace --fix --allow-dirty --allow-staged

build:
    @rm -rf .cargo  # Clean dev mode files
    cargo build --release

build-maxperf:
    RUSTFLAGS="-C target-cpu=native" cargo build --profile maxperf --features jemalloc,asm-keccak

build-tools:
    cargo build --release --package xlayer-reth-tools

build-tools-maxperf:
    RUSTFLAGS="-C target-cpu=native" cargo build --package xlayer-reth-tools --profile maxperf --features jemalloc,asm-keccak

install:
    cargo install --path bin/node --bin xlayer-reth-node --force --locked --profile release

install-maxperf:
    RUSTFLAGS="-C target-cpu=native" cargo install --path bin/node --bin xlayer-reth-node --force --locked --profile maxperf --features jemalloc,asm-keccak

install-tools:
    cargo install --path bin/tools --bin xlayer-reth-tools --force --locked --profile release

install-tools-maxperf:
    RUSTFLAGS="-C target-cpu=native" cargo install --path bin/tools --bin xlayer-reth-tools --force --locked --profile maxperf --features jemalloc,asm-keccak

clean:
    cargo clean

build-docker suffix="" git_sha="" git_timestamp="":
    #!/usr/bin/env bash
    set -e
    # Only clean .cargo in production mode, preserve it for dev builds
    if [ "{{suffix}}" != "dev" ]; then
        rm -rf .cargo
    fi
    GITHASH=$(git rev-parse --short HEAD)
    SUFFIX=""
    if [ -n "{{suffix}}" ]; then
        SUFFIX="-{{suffix}}"
    fi
    TAG="op-reth:$GITHASH$SUFFIX"
    echo "🐳 Building XLayer Reth Docker image: $TAG ..."

    # Build with optional git info for version metadata
    BUILD_ARGS=""
    if [ -n "{{git_sha}}" ]; then
        BUILD_ARGS="--build-arg VERGEN_GIT_SHA={{git_sha}}"
        echo "📋 Using git SHA: {{git_sha}}"
    fi
    if [ -n "{{git_timestamp}}" ]; then
        BUILD_ARGS="$BUILD_ARGS --build-arg VERGEN_GIT_COMMIT_TIMESTAMP={{git_timestamp}}"
    fi

    docker build $BUILD_ARGS -t $TAG -f DockerfileOp .
    docker tag $TAG op-reth:latest
    echo "🔖 Tagged $TAG as op-reth:latest"

build-docker-tools suffix="" git_sha="" git_timestamp="":
    #!/usr/bin/env bash
    set -e
    # Only clean .cargo in production mode, preserve it for dev builds
    if [ "{{suffix}}" != "dev" ] && [ -d .cargo ]; then
        rm -rf .cargo
    fi
    GITHASH=$(git rev-parse --short HEAD)
    SUFFIX=""
    if [ -n "{{suffix}}" ]; then
        SUFFIX="-{{suffix}}"
    fi
    TAG="xlayer-reth-tools:$GITHASH$SUFFIX"
    echo "🐳 Building XLayer Reth Tools Docker image: $TAG ..."

    # Build with optional git info for version metadata
    BUILD_ARGS=""
    if [ -n "{{git_sha}}" ]; then
        BUILD_ARGS="--build-arg VERGEN_GIT_SHA={{git_sha}}"
        echo "📋 Using git SHA: {{git_sha}}"
    fi
    if [ -n "{{git_timestamp}}" ]; then
        BUILD_ARGS="$BUILD_ARGS --build-arg VERGEN_GIT_COMMIT_TIMESTAMP={{git_timestamp}}"
    fi

    docker build $BUILD_ARGS -t $TAG -f DockerfileTools .
    docker tag $TAG xlayer-reth-tools:latest
    echo "🔖 Tagged $TAG as xlayer-reth-tools:latest"

watch-test:
    @command -v bacon >/dev/null 2>&1 || cargo install bacon
    bacon test

watch-check:
    @command -v bacon >/dev/null 2>&1 || cargo install bacon
    bacon clippy

xlayer:
    cp .github/scripts/pre-commit-xlayer .git/hooks/pre-commit && \
    chmod +x .git/hooks/pre-commit
