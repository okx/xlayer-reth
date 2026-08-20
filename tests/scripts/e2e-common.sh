#!/usr/bin/env bash

# Print and execute a command without flattening or re-evaluating its arguments.
run_logged() {
    printf 'Running:'
    printf ' %q' "$@"
    printf '\n'
    "$@"
}

# Install a recipe-wide EXIT timer. The trap preserves the command status.
start_total_timer() {
    E2E_TIMER_LABEL="${1:-total time}"
    E2E_STARTED_AT="$(date +%s)"
    trap report_total_time EXIT
}

report_total_time() {
    local status=$?
    local finished_at elapsed
    finished_at="$(date +%s)"
    elapsed=$((finished_at - E2E_STARTED_AT))
    printf '%s: %dm%02ds (exit=%d)\n' \
        "$E2E_TIMER_LABEL" \
        "$((elapsed / 60))" \
        "$((elapsed % 60))" \
        "$status"
    trap - EXIT
    exit "$status"
}

# Run Go E2E tests with the shared binary/environment/default-package policy.
run_e2e_tests() {
    local repo_root=$1
    local tests_dir=$2
    local package_workers=$3
    local in_package_parallel=$4
    local test_timeout=$5
    local consensus_client=$6
    shift 6

    export OPTIMISM_ROOT="${OPTIMISM_ROOT:-${repo_root}/deps/optimism}"
    export RUST_BINARY_PATH_OP_RETH="${RUST_BINARY_PATH_OP_RETH:-${repo_root}/target/debug/xlayer-reth-node}"

    case "$consensus_client" in
        op-node)
            ;;
        kona-node)
            export RUST_BINARY_PATH_KONA_NODE="${RUST_BINARY_PATH_KONA_NODE:-${repo_root}/deps/optimism/rust/target/debug/kona-node}"
            ;;
        *)
            printf 'Unsupported DEVSTACK_L2CL_KIND=%q; expected op-node or kona-node\n' "$consensus_client" >&2
            return 2
            ;;
    esac
    export DEVSTACK_L2CL_KIND="$consensus_client"

    cd "$tests_dir"

    if [ "$#" -eq 0 ] || [[ "$1" == -* ]]; then
        set -- ./... "$@"
    fi

    local go_test_cmd=(
        go test
        -p "$package_workers"
        -parallel "$in_package_parallel"
        -timeout "$test_timeout"
        "$@"
    )
    run_logged "${go_test_cmd[@]}"
}
