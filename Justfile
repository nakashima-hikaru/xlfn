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
        --all-features \
        --locked

# CI profile: no fail-fast, global timeout.
test-ci:
    cargo nextest run \
        --profile ci \
        --workspace \
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
        --package xlfn \
        --feature-powerset \
        --depth 2 \
        --exclude-features bench-internals \
        --no-dev-deps

deny:
    cargo deny check

semver:
    # The current pre-1.0 release intentionally permits breaking public API
    # cleanup, including the CacheRegistry endpoint redesign. Use the strict
    # major compatibility mode so removed APIs cannot be mistaken for a
    # compatible refactor.
    cargo semver-checks \
        --workspace \
        --baseline-rev 0.1.0 \
        --release-type major

quick: fmt clippy test

check: fmt clippy features test bench-check deny semver

# --- Benchmark recipes ---

bench:
    cargo bench --package xlfn --features "bench-internals async" --locked

bench-async:
    cargo bench --package xlfn --bench async_spawn --features "bench-internals async" --locked

bench-sync:
    cargo bench --package xlfn --bench sync_boundary --features bench-internals --locked

bench-input-identity:
    cargo bench --package xlfn --bench input_identity --features bench-internals --locked

bench-formula-caller:
    cargo bench --package xlfn --bench formula_caller --features bench-internals --locked

bench-handle-prepare:
    cargo bench --package xlfn --bench handle_prepare --features bench-internals --locked

bench-handle-lookup:
    cargo bench --package xlfn --bench handle_lookup --features bench-internals --locked

bench-formula-revision:
    cargo bench --package xlfn --bench formula_revision --features bench-internals --locked

bench-check:
    cargo clippy --package xlfn --benches --features "bench-internals async" --locked -- -D warnings
