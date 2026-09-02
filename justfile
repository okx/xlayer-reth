set tempdir := "target/tmp"

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
alias ig := init-git

default:
    @just --list

# Initialize git submodules
init-git:
    git submodule update --init --recursive

# Ensure submodules are initialized (used as dependency)
[private]
ensure-submodules:
    @git submodule status --recursive | grep -q '^-' && git submodule update --init --recursive || true

# Ensure tempdir exists for shebang recipes
[private]
ensure-tempdir:
    @mkdir -p target/tmp

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
    just lint-go
    just test

fix: fix-format fix-clippy lint-go-fix

# Build dependencies then run the Go E2E harness under tests/. Arguments are
# forwarded to `go test` (package selection, -run filters, go test flags). The
# repository tempdir is created first so just never fails writing temp files.
[positional-arguments]
e2e *args: ensure-tempdir ensure-submodules
    cd tests && just e2e "$@"

# Run the Go E2E harness against existing artifacts without building first.
# Arguments are forwarded to `go test` exactly as with `e2e`.
[positional-arguments]
e2e-no-build *args: ensure-tempdir
    cd tests && just e2e-no-build "$@"

# Lint the standalone Go module under tests/.
lint-go: ensure-submodules
    cd tests && just lint-go

# Auto-format and lint the standalone Go module under tests/.
lint-go-fix: ensure-submodules
    cd tests && just lint-go-fix

# Run `just test true` to run the full Go E2E suite after the Rust workspace
# tests. Flashblocks is part of the regular Go E2E suite.
#
# Set docker_tests=true to enable tests gated by the `xl-docker-tests` feature.
test include_e2e="false" docker_tests="false": ensure-tempdir
    #!/usr/bin/env bash
    set -euo pipefail

    if cargo nextest --version &>/dev/null; then
        test_command=(cargo nextest run --workspace)
        runner="cargo nextest run"
    else
        test_command=(cargo test --workspace)
        runner="cargo test"
    fi

    if [ "{{docker_tests}}" = "true" ]; then
        test_command+=(--features xl-docker-tests)
    fi

    echo "Running tests via $runner (include_e2e={{include_e2e}}, docker_tests={{docker_tests}})"
    "${test_command[@]}"
    if [ "{{include_e2e}}" = "true" ]; then
        just e2e
    fi

# Format only workspace members. `cargo fmt --all` ALSO descends into local submodules.
check-format:
    bash -c 'set -uo pipefail; fail=0; for p in $(cargo metadata --no-deps --format-version 1 | jq -r ".packages[].name"); do cargo +nightly fmt -p "$p" -- --check || fail=1; done; exit $fail'

fix-format:
    cargo fix --allow-dirty --allow-staged
    bash -c 'set -euo pipefail; for p in $(cargo metadata --no-deps --format-version 1 | jq -r ".packages[].name"); do cargo +nightly fmt -p "$p"; done'

check-clippy:
    cargo clippy --all-targets --workspace -- -D warnings

fix-clippy:
    cargo clippy --all-targets --workspace --fix --allow-dirty --allow-staged

build: ensure-submodules
    @rm -rf .cargo  # Clean dev mode files
    cargo build --release

build-maxperf: ensure-submodules
    RUSTFLAGS="-C target-cpu=native" cargo build --profile maxperf --features jemalloc,asm-keccak

build-tools: ensure-submodules
    cargo build --release --package xlayer-reth-tools

build-tools-maxperf: ensure-submodules
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

build-docker suffix="" git_sha="" git_timestamp="": ensure-tempdir
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

    SECRET_ARG=""
    if [ -f /etc/ssl/copilot.pem ]; then
        SECRET_ARG="--secret id=copilot,src=/etc/ssl/copilot.pem"
    fi

    docker build $BUILD_ARGS $SECRET_ARG -t $TAG -f DockerfileOp .
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

    SECRET_ARG=""
    if [ -f /etc/ssl/copilot.pem ]; then
        SECRET_ARG="--secret id=copilot,src=/etc/ssl/copilot.pem"
    fi

    docker build $BUILD_ARGS $SECRET_ARG -t $TAG -f DockerfileTools .
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
