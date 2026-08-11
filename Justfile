# Developer and CI command surface. Keep CI workflows and the testing guide
# aligned with these recipes.

default:
    @just --list

fmt:
    cargo fmt --all -- --check

clippy:
    cargo clippy \
        --workspace \
        --all-targets \
        --all-features \
        --locked \
        -- \
        -D warnings

# Fast test cycle using nextest (process-isolated).
test:
    cargo nextest run \
        --workspace \
        --all-targets \
        --all-features \
        --locked

# CI profile: no fail-fast, global timeout.
test-ci:
    cargo nextest run \
        --profile ci \
        --workspace \
        --all-targets \
        --all-features \
        --locked

# Same-process libtest semantics (kept for Windows CI parity).
test-libtest:
    cargo test \
        --workspace \
        --all-targets \
        --all-features \
        --locked

features:
    cargo hack check \
        --package xlfn \
        --feature-powerset \
        --no-dev-deps
    cargo hack check \
        --package xlfn-core \
        --feature-powerset \
        --depth 2 \
        --exclude-features bench-internals \
        --no-dev-deps

deny:
    cargo deny check

quick: fmt clippy test

check: fmt clippy features test bench-check deny

# --- Benchmark recipes ---

bench:
    cargo bench --package xlfn-core --features "bench-internals async" --locked

bench-async:
    cargo bench --package xlfn-core --bench async_spawn --features "bench-internals async" --locked

bench-sync:
    cargo bench --package xlfn-core --bench sync_boundary --features bench-internals --locked

bench-handle:
    cargo bench --package xlfn-core --bench handle_prepare --features bench-internals --locked

bench-check:
    cargo clippy --package xlfn-core --benches --features "bench-internals async" --locked -- -D warnings
