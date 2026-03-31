alias c := check
alias f := fix
alias t := test
alias ff := fix-format
alias cf := check-format
alias cc := check-clippy
alias fc := fix-clippy
alias b := build
alias bd := build-dev
alias bm := build-maxperf
alias bt := build-tools
alias btm := build-tools-maxperf
alias i := install
alias im := install-maxperf
alias it := install-tools
alias itm := install-tools-maxperf
alias cl := clean
alias docker := build-docker
alias dockerdev := build-docker-dev
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
    @echo "Running tests (include_e2e={{include_e2e}})"
    cargo test --workspace --exclude xlayer-e2e-test --all-features
    @if [ "{{include_e2e}}" = "true" ]; then \
        cargo test -p xlayer-e2e-test --test e2e_tests -- --nocapture --test-threads=1; \
    fi
    @if [ "{{include_flashblocks}}" = "true" ]; then \
        cargo test -p xlayer-e2e-test --test flashblocks_tests -- --nocapture --test-threads=1; \
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
    #!/usr/bin/env bash
    # Restore any dev-mode patches before production build
    if [ -f .cargo/.tools-toml-patched ]; then
        git checkout bin/tools/Cargo.toml 2>/dev/null && echo "🔄 Restored bin/tools/Cargo.toml from dev-mode patch"
    fi
    rm -rf .cargo  # Clean dev mode files
    cargo build --release

[no-exit-message]
build-dev reth_path="" build="true":
    #!/usr/bin/env bash
    set -e

    if [ -z "{{reth_path}}" ]; then
        if [ -f .cargo/config.toml ]; then
            echo "📦 Using existing .cargo/config.toml"
        else
            echo "⚠️  First time setup needed: just build-dev /absolute/path/to/reth"
            exit 1
        fi
    else
        RETH_PATH=$(cd "{{reth_path}}" && pwd)
        echo "🔄 Generating .cargo/config.toml using local reth: $RETH_PATH"
        mkdir -p .cargo
        just _gen-dev-config "$RETH_PATH" "$RETH_PATH"
        echo "✅ Using local reth: $RETH_PATH"
    fi

    if [ "{{build}}" = "true" ]; then
        cargo build --release
    fi

# Internal: generate .cargo/config.toml patching paradigmxyz/reth with local paths.
# Scans the root Cargo.toml and deps/optimism for all required reth crates, then
# discovers their actual paths by scanning the local reth repository.
# reth_src: local reth repository to scan for crate locations
# reth_cfg: path prefix written into config.toml (same as reth_src locally, /reth in Docker)
_gen-dev-config reth_src reth_cfg:
    #!/usr/bin/env bash
    set -e

    RETH_SRC="{{reth_src}}"
    RETH_CFG="{{reth_cfg}}"

    if [ ! -d "$RETH_SRC" ]; then
        echo "❌ Error: reth path does not exist: $RETH_SRC"
        exit 1
    fi

    # Collect all reth crate names required by the root workspace
    NEEDED=$(mktemp)
    grep 'git = "https://github.com/paradigmxyz/reth"' Cargo.toml | \
        grep -oE '^[a-z][a-z0-9-]+' | sort -u > "$NEEDED"

    # Also scan deps/optimism for additional reth deps needed by the op-reth submodule
    if [ -d deps/optimism ]; then
        find deps/optimism -name "Cargo.toml" -not -path "*/target/*" 2>/dev/null | \
            xargs grep -h 'git = "https://github.com/paradigmxyz/reth"' 2>/dev/null | \
            grep -oE '^[a-z][a-z0-9-]+' | sort -u >> "$NEEDED"
        sort -u "$NEEDED" -o "$NEEDED"
    fi

    # Build crate index: name -> absolute directory path
    echo "📋 Scanning $RETH_SRC for reth crate paths..."
    CRATE_MAP=$(mktemp)
    find "$RETH_SRC" -name "Cargo.toml" -not -path "*/target/*" | while read -r toml; do
        name=$(grep '^name = ' "$toml" 2>/dev/null | head -1 | sed 's/name = "\(.*\)"/\1/')
        if [ -n "$name" ]; then
            echo "$name	$(dirname "$toml")"
        fi
    done > "$CRATE_MAP"

    # Write [patch."https://github.com/paradigmxyz/reth"] section to .cargo/config.toml
    echo '[patch."https://github.com/paradigmxyz/reth"]' > .cargo/config.toml
    while IFS= read -r crate; do
        crate_dir=$(grep "^${crate}	" "$CRATE_MAP" | cut -f2 | head -1)
        if [ -n "$crate_dir" ]; then
            rel="${crate_dir#$RETH_SRC/}"
            echo "$crate = { path = \"$RETH_CFG/$rel\" }" >> .cargo/config.toml
        else
            echo "⚠️  Warning: '$crate' not found in $RETH_SRC" >&2
        fi
    done < "$NEEDED"

    rm -f "$CRATE_MAP" "$NEEDED"

    # Restore any previous rocksdb patch before re-checking compatibility
    if [ -f .cargo/.tools-toml-patched ]; then
        git checkout bin/tools/Cargo.toml 2>/dev/null || true
        rm -f .cargo/.tools-toml-patched
    fi

    # Check if local reth-provider exposes 'rocksdb' as an optional feature.
    # Some reth versions have rocksdb as a non-optional (always-included) dep instead.
    # In that case, requesting features = ["rocksdb"] in consumers fails at workspace
    # feature resolution, so we strip the request from bin/tools/Cargo.toml.
    local_provider_toml=$(find "$RETH_SRC" -name "Cargo.toml" -not -path "*/target/*" \
        -exec grep -l '^name = "reth-provider"$' {} \; 2>/dev/null | head -1)

    if [ -n "$local_provider_toml" ]; then
        has_rocksdb_feature=$(awk '
            /^\[features\]/ { in_features=1; next }
            /^\[/ { in_features=0 }
            in_features && /^rocksdb/ { print "yes"; exit }
        ' "$local_provider_toml")

        if [ "$has_rocksdb_feature" != "yes" ]; then
            echo "ℹ️  Local reth-provider: 'rocksdb' is a required dep (not optional feature) — rocksdb is always included"
            echo "   Patching bin/tools/Cargo.toml to remove explicit feature request..."
            python3 -c "import sys; f='bin/tools/Cargo.toml'; open(f,'w').write(open(f).read().replace('reth-provider = { workspace = true, features = [\"rocksdb\"] }', 'reth-provider.workspace = true'))"
            touch .cargo/.tools-toml-patched
            echo "   ✅ Patched. Restore any time with: git checkout bin/tools/Cargo.toml"
        fi
    fi

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

[no-exit-message]
build-docker-dev reth_path="":
    #!/usr/bin/env bash
    set -e
    PATH_FILE=".cargo/.reth_source_path"

    # Determine source path: provided > saved > error
    if [ -n "{{reth_path}}" ]; then
        RETH_SRC=$(cd "{{reth_path}}" && pwd)
    elif [ -f "$PATH_FILE" ]; then
        RETH_SRC=$(cat "$PATH_FILE")
        echo "📦 Using saved path: $RETH_SRC"
    else
        echo "❌ First time: just build-docker-dev /path/to/reth" && exit 1
    fi

    if [ ! -d "$RETH_SRC" ]; then
        echo "❌ Error: reth path does not exist: $RETH_SRC"
        exit 1
    fi

    mkdir -p .cargo
    echo "$RETH_SRC" > "$PATH_FILE"

    echo "📦 Syncing $RETH_SRC → .cargo/reth..."
    rsync -au --delete --exclude='.git' --exclude='target' "$RETH_SRC/" .cargo/reth/
    echo "✅ Sync complete"

    # Extract git info from local reth for version metadata
    RETH_GIT_SHA=$(cd "$RETH_SRC" && git rev-parse HEAD 2>/dev/null || echo "unknown")
    RETH_GIT_TIMESTAMP=$(cd "$RETH_SRC" && git log -1 --format=%cI 2>/dev/null || echo "")
    echo "📋 Local reth commit: $RETH_GIT_SHA"

    # Generate config with /reth path (DockerfileOp moves .cargo/reth to /reth)
    just _gen-dev-config "$RETH_SRC" "/reth"

    # Build Docker image with reth git info
    just build-docker dev "$RETH_GIT_SHA" "$RETH_GIT_TIMESTAMP"

    # Clean up synced reth source
    rm -rf .cargo/reth
    echo "🧹 Cleaned up .cargo/reth"

    # Restore local config pointing to actual local path
    just _gen-dev-config "$RETH_SRC" "$RETH_SRC"
    echo "✅ Restored local development config"

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
