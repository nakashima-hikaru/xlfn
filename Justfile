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
        --locked

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
        --all-features \
        --locked \
        -- \
        --test-threads=1

test-all: test-libtest

test-core:
    cargo test \
        --package xlfn \
        --no-default-features \
        --locked \
        -- \
        --test-threads=1

test-handles:
    cargo test \
        --package xlfn \
        --no-default-features \
        --features handles \
        --locked \
        -- \
        --test-threads=1

test-rtd:
    cargo test \
        --package xlfn \
        --no-default-features \
        --features rtd \
        --locked \
        -- \
        --test-threads=1

test-async:
    cargo test \
        --package xlfn \
        --no-default-features \
        --features async \
        --locked \
        -- \
        --test-threads=1

features:
    cargo hack check \
        --package xlfn \
        --feature-powerset \
        --no-dev-deps
    cargo hack check \
        --package xlfn \
        --feature-powerset \
        --depth 2 \
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
        --exclude xlfn-kernel \
        --exclude xlfn-hotpath-cpu \
        --baseline-rev 0.1.0 \
        --release-type major

publish-check:
    cargo publish --workspace --dry-run --locked

quick: fmt clippy test

check: fmt clippy features test bench-check deny semver

# --- Benchmark recipes ---

# Full production-path suite. Diagnostic microbenchmarks are intentionally
# excluded; use `just bench-diagnostics` when investigating their results.
bench: bench-full

bench-full:
    just bench-one async_spawn "bench-internals async"
    just bench-one sync_boundary
    just bench-one handle_prepare
    just bench-one formula_revision
    just bench-one handle_lookup
    just bench-one formula_caller
    just bench-one argument_ingress
    just bench-one rtd_publish "bench-internals rtd"
    just bench-one handle_call_resolution

# Representative PR suite. Scaling curves remain available through
# `bench-full`, while this tier keeps every pull request focused on the
# production paths most likely to regress.
bench-pr:
    just bench-one-filter async_spawn "^async_spawn/per_iteration/(1|32)\z" "bench-internals async"
    just bench-one-filter sync_boundary "^(sync_boundary/ingress_udf_only|sync_boundary/admission|sync_boundary/scalar_return/no_subscriber|sync_boundary/return_tracker_only)/(1|32)\z"
    just bench-one-filter handle_prepare "^handle_prepare/(cold_miss_batch_100|warm_hit_batch_100)\z"
    just bench-one formula_revision
    just bench-one-filter handle_lookup "^handle_lookup/(warm_same_token|distinct_tokens)/(1|32)\z"
    just bench-one formula_caller
    just bench-one-filter argument_ingress "^argument_ingress/(f64/with_identity|string_short/borrowed|matrix_string_10k/borrowed|matrix_f64_100k/with_identity|excel_value_matrix_100k/with_identity|handle/with_identity)\z"
    just bench-one rtd_publish "bench-internals rtd"
    just bench-one-filter handle_call_resolution "^(handle_call_resolution/handles/(1|8)|handle_runtime_resolution/concurrent/(1|32))\z"

# Diagnostic microbenchmarks are useful for diagnosis, but are not part of
# the Bencher regression contract.
bench-diagnostics:
    just bench-one input_identity
    just bench-one handle_lookup_arc_control

bench-one name features="bench-internals":
    cargo bench --package xlfn --bench {{name}} --features "{{features}}" --locked

bench-one-filter name filter features="bench-internals":
    cargo bench --package xlfn --bench {{name}} --features "{{features}}" --locked -- "{{filter}}"

bench-check:
    cargo clippy --package xlfn --benches --all-features --locked
