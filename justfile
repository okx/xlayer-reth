default:
    @just --list

check: check-format check-clippy test

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
build-dev reth_path="" build="true":
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

    if [ "{{build}}" = "true" ]; then
        cargo build --release
    fi

# Check dev template has all reth crates
check-dev-template:
    #!/usr/bin/env bash
    set -e

    # Check for duplicates within each patch section (not across sections)
    OKX_DUPLICATES=$(sed -n '/^\[patch\."https:\/\/github\.com\/okx\/reth"\]/,/^\[patch\./p' .reth-dev.toml | grep 'RETH_PATH_PLACEHOLDER' | grep -oE '^[a-z][a-z0-9-]+' | sort | uniq -d)
    PARADIGM_DUPLICATES=$(sed -n '/^\[patch\."https:\/\/github\.com\/paradigmxyz\/reth"\]/,/^$/p' .reth-dev.toml | grep 'RETH_PATH_PLACEHOLDER' | grep -oE '^[a-z][a-z0-9-]+' | sort | uniq -d)
    DUPLICATES="$OKX_DUPLICATES$PARADIGM_DUPLICATES"

    # Get crates from each section separately
    OKX_CARGO=$(mktemp)
    PARADIGM_CARGO=$(mktemp)
    OKX_TEMPLATE=$(mktemp)
    PARADIGM_TEMPLATE=$(mktemp)

    # Extract okx/reth dependencies from Cargo.toml
    grep 'git = "https://github.com/okx/reth"' Cargo.toml | grep -oE '^[a-z][a-z0-9-]+' | sort -u > "$OKX_CARGO"

    # Extract paradigmxyz/reth patches from Cargo.toml
    sed -n '/^\[patch\."https:\/\/github\.com\/paradigmxyz\/reth"\]/,/^$/p' Cargo.toml | grep -oE '^[a-z][a-z0-9-]+' | sort -u > "$PARADIGM_CARGO"

    # Extract okx/reth patches from .reth-dev.toml
    sed -n '/^\[patch\."https:\/\/github\.com\/okx\/reth"\]/,/^\[patch\./p' .reth-dev.toml | grep 'RETH_PATH_PLACEHOLDER' | grep -oE '^[a-z][a-z0-9-]+' | sort -u > "$OKX_TEMPLATE"

    # Extract paradigmxyz/reth patches from .reth-dev.toml
    sed -n '/^\[patch\."https:\/\/github\.com\/paradigmxyz\/reth"\]/,/^$/p' .reth-dev.toml | grep 'RETH_PATH_PLACEHOLDER' | grep -oE '^[a-z][a-z0-9-]+' | sort -u > "$PARADIGM_TEMPLATE"

    # Check for missing/extra in okx/reth section
    OKX_MISSING=$(comm -23 "$OKX_CARGO" "$OKX_TEMPLATE")
    OKX_EXTRA=$(comm -13 "$OKX_CARGO" "$OKX_TEMPLATE")

    # Check for missing/extra in paradigmxyz/reth section
    PARADIGM_MISSING=$(comm -23 "$PARADIGM_CARGO" "$PARADIGM_TEMPLATE")
    PARADIGM_EXTRA=$(comm -13 "$PARADIGM_CARGO" "$PARADIGM_TEMPLATE")

    # Clean up temp files
    rm -f "$OKX_CARGO" "$PARADIGM_CARGO" "$OKX_TEMPLATE" "$PARADIGM_TEMPLATE"

    if [ -z "$DUPLICATES" ] && [ -z "$OKX_MISSING" ] && [ -z "$OKX_EXTRA" ] && [ -z "$PARADIGM_MISSING" ] && [ -z "$PARADIGM_EXTRA" ]; then
        echo "✅ Template OK"
    else
        if [ -n "$DUPLICATES" ]; then
            echo "❌ Duplicates in .reth-dev.toml:"
            echo "$DUPLICATES" | tr ' ' '\n' | sed 's/^/  - /'
        fi
        if [ -n "$OKX_MISSING" ]; then
            echo "❌ Missing in [patch.\"https://github.com/okx/reth\"] section:"
            echo "$OKX_MISSING" | tr ' ' '\n' | sed 's/^/  - /'
        fi
        if [ -n "$OKX_EXTRA" ]; then
            echo "❌ Extra in [patch.\"https://github.com/okx/reth\"] section:"
            echo "$OKX_EXTRA" | tr ' ' '\n' | sed 's/^/  - /'
        fi
        if [ -n "$PARADIGM_MISSING" ]; then
            echo "❌ Missing in [patch.\"https://github.com/paradigmxyz/reth\"] section:"
            echo "$PARADIGM_MISSING" | tr ' ' '\n' | sed 's/^/  - /'
        fi
        if [ -n "$PARADIGM_EXTRA" ]; then
            echo "❌ Extra in [patch.\"https://github.com/paradigmxyz/reth\"] section:"
            echo "$PARADIGM_EXTRA" | tr ' ' '\n' | sed 's/^/  - /'
        fi
        exit 1
    fi

# Sync .reth-dev.toml with Cargo.toml dependencies
sync-dev-template reth_path:
    #!/usr/bin/env bash
    set -e

    RETH_PATH="{{reth_path}}"

    if [ ! -d "$RETH_PATH" ]; then
        echo "❌ Error: reth path does not exist: $RETH_PATH"
        exit 1
    fi

    # Check if fd is installed, install if not
    if ! command -v fd &> /dev/null; then
        echo "📦 fd not found, installing..."
        if [[ "$OSTYPE" == "linux-gnu"* ]]; then
            if command -v apt-get &> /dev/null; then
                sudo apt-get update && sudo apt-get install -y fd-find
                # Debian/Ubuntu installs it as fdfind
                if command -v fdfind &> /dev/null; then
                    alias fd=fdfind
                fi
            elif command -v dnf &> /dev/null; then
                sudo dnf install -y fd-find
            elif command -v yum &> /dev/null; then
                sudo yum install -y fd-find
            elif command -v pacman &> /dev/null; then
                sudo pacman -S --noconfirm fd
            else
                echo "❌ Unable to install fd automatically. Please install it manually."
                exit 1
            fi
        elif [[ "$OSTYPE" == "darwin"* ]]; then
            if command -v brew &> /dev/null; then
                brew install fd
            else
                echo "❌ Homebrew not found. Please install fd manually: https://github.com/sharkdp/fd"
                exit 1
            fi
        else
            echo "❌ Unsupported OS. Please install fd manually: https://github.com/sharkdp/fd"
            exit 1
        fi
        echo "✅ fd installed successfully"
    fi

    echo "🔄 Syncing .reth-dev.toml with Cargo.toml..."
    echo "📂 Using reth path: $RETH_PATH"

    # Build a lookup table of all crate names to their paths (using fd for speed)
    echo "📋 Building crate index..."
    CRATE_MAP=$(mktemp)

    # Use fdfind if fd is not available (Debian/Ubuntu)
    FD_CMD="fd"
    if ! command -v fd &> /dev/null && command -v fdfind &> /dev/null; then
        FD_CMD="fdfind"
    fi

    $FD_CMD -t f "^Cargo.toml$" "$RETH_PATH" -x grep -H "^name = " | \
        sed 's|/Cargo.toml:name = "\(.*\)"|\t\1|' | \
        awk -F'\t' '{print $2 "\t" $1}' > "$CRATE_MAP"

    # Create a temporary file with the header for okx/reth patches
    echo '[patch."https://github.com/okx/reth"]' > .reth-dev.toml.tmp

    # Extract reth dependencies from Cargo.toml and find their actual paths
    grep 'git = "https://github.com/okx/reth"' Cargo.toml | \
        grep -oE '^[a-z][a-z0-9-]+' | \
        sort -u | \
        while read -r crate; do
            # Look up the crate in our pre-built map
            CRATE_DIR=$(grep "^$crate"$'\t' "$CRATE_MAP" | cut -f2 | head -1)

            if [ -z "$CRATE_DIR" ]; then
                echo "⚠️  Could not find crate '$crate' in $RETH_PATH"
                echo "$crate = { path = \"RETH_PATH_PLACEHOLDER/crates/$crate\" }" >> .reth-dev.toml.tmp
                continue
            fi

            # Make path relative to RETH_PATH
            REL_PATH=$(echo "$CRATE_DIR" | sed "s|^$RETH_PATH/||")

            echo "$crate = { path = \"RETH_PATH_PLACEHOLDER/$REL_PATH\" }" >> .reth-dev.toml.tmp
        done

    # Add paradigmxyz/reth patches section
    echo "" >> .reth-dev.toml.tmp
    echo '[patch."https://github.com/paradigmxyz/reth"]' >> .reth-dev.toml.tmp

    # Extract reth dependencies from the paradigmxyz patch section
    sed -n '/^\[patch\."https:\/\/github\.com\/paradigmxyz\/reth"\]/,/^$/p' Cargo.toml | \
        grep -oE '^[a-z][a-z0-9-]+' | \
        sort -u | \
        while read -r crate; do
            # Look up the crate in our pre-built map
            CRATE_DIR=$(grep "^$crate"$'\t' "$CRATE_MAP" | cut -f2 | head -1)

            if [ -z "$CRATE_DIR" ]; then
                echo "⚠️  Could not find crate '$crate' in $RETH_PATH"
                echo "$crate = { path = \"RETH_PATH_PLACEHOLDER/crates/$crate\" }" >> .reth-dev.toml.tmp
                continue
            fi

            # Make path relative to RETH_PATH
            REL_PATH=$(echo "$CRATE_DIR" | sed "s|^$RETH_PATH/||")

            echo "$crate = { path = \"RETH_PATH_PLACEHOLDER/$REL_PATH\" }" >> .reth-dev.toml.tmp
        done

    # Clean up temp file
    rm -f "$CRATE_MAP"

    # Show diff if there are changes
    if ! diff -q .reth-dev.toml .reth-dev.toml.tmp > /dev/null 2>&1; then
        echo ""
        echo "📋 Changes detected:"
        diff -u .reth-dev.toml .reth-dev.toml.tmp || true
        echo ""
        mv .reth-dev.toml.tmp .reth-dev.toml
        echo "✅ .reth-dev.toml synced successfully"
    else
        rm .reth-dev.toml.tmp
        echo "✅ .reth-dev.toml already in sync"
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

    # Extract git info from local reth for version metadata
    RETH_GIT_SHA=$(cd "$RETH_SRC" && git rev-parse HEAD 2>/dev/null || echo "unknown")
    RETH_GIT_TIMESTAMP=$(cd "$RETH_SRC" && git log -1 --format=%cI 2>/dev/null || echo "")
    echo "📋 Local reth commit: $RETH_GIT_SHA"

    # Generate config with /reth path (Docker will move .cargo/reth to /reth to avoid nesting)
    sed "s|RETH_PATH_PLACEHOLDER|/reth|g" .reth-dev.toml > .cargo/config.toml

    # Build Docker image with reth git info
    just build-docker dev "$RETH_GIT_SHA" "$RETH_GIT_TIMESTAMP"

    # Clean up synced reth source (will be re-synced on next build)
    rm -rf .cargo/reth
    echo "🧹 Cleaned up .cargo/reth"

    # Restore local config for development (point to actual local path, not /reth)
    sed "s|RETH_PATH_PLACEHOLDER|$RETH_SRC|g" .reth-dev.toml > .cargo/config.toml
    echo "✅ Restored local development config"

watch-test:
    cargo watch -x test

watch-check:
    cargo watch -x "fmt --all -- --check" -x "clippy --all-targets -- -D warnings" -x test

xlayer:
	cp .github/scripts/pre-commit-xlayer .git/hooks/pre-commit && \
	chmod +x .git/hooks/pre-commit
