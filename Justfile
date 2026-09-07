# Developer and CI command surface. Keep CI workflows and the testing guide
# aligned with these recipes.

default:
    @just --list

fmt:
    cargo fmt --all -- --check

# Audit every direct catch reference, including test and macro token streams.
panic-boundaries:
    python3 -B -m unittest discover -s tools -p test_check_panic_boundaries.py
    python3 -B tools/check_panic_boundaries.py

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

# Miri temporal pointer reclamation and domain safety regression tests.
miri:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +nightly miri test -p xlfn-kernel --lib -- miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +nightly miri test -p xlfn --no-default-features --features handles --lib -- miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +nightly miri test -p xlfn --no-default-features --features async --lib -- miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +nightly miri test -p xlfn --no-default-features --features rtd --lib -- miri_
    just miri-cache

# Production full-cache and safe shared-flight regressions, both borrow models.
miri-cache:
    just miri-cache-model "-Zmiri-disable-isolation"
    just miri-cache-model "-Zmiri-disable-isolation -Zmiri-tree-borrows"

[private]
miri-cache-model flags:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="{{flags}}" cargo +nightly miri test -p xlfn --no-default-features --features "unstable-cache bench-internals" --locked --lib cache::protocol_tests -- --test-threads=1
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="{{flags}}" cargo +nightly miri test -p xlfn --no-default-features --features "unstable-cache bench-internals" --locked --lib cache::shared_flight::tests -- --test-threads=1

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
        --depth 2 \
        --lib \
        --locked

deny:
    cargo deny check

semver:
    # Derive compatibility requirements from each crate's actual version.
    # A workspace-wide major override would skip checks even for crates
    # whose compatibility line has not changed.
    cargo semver-checks \
        --workspace \
        --exclude xlfn-kernel \
        --baseline-rev 0.1.0

publish-check:
    cargo publish --workspace --dry-run --locked

quick: fmt panic-boundaries clippy test

check: fmt panic-boundaries clippy features test bench-check deny semver

# --- Benchmark recipes ---

# Bencher continuous integration regression suite (representative production cases).
# Used by default for main branch history tracking and pull request regression gating.
bench: bench-ci

# Canonical CI regression suite.
bench-ci:
    just bench-one-filter async_spawn "^(async_spawn/per_iteration/(1|32)|async_spawn/matrix_reschedule/workers_4/16|async_spawn/spawn_and_drain/workers_4/16)\z" "bench-internals async"
    just bench-one-filter sync_boundary "^sync_boundary/(admission|scalar_return/no_subscriber)/(1|32)\z"
    just bench-one-filter handle_prepare "^handle_prepare/(cold_miss_batch_100|warm_hit_batch_100|distinct_key/(1|32))\z"
    just bench-one-filter formula_revision "^formula_revision/warm_hit/(f64|matrix_f64_100k)\z"
    just bench-one-filter handle_lookup "^handle_lookup/(warm_same_token|distinct_tokens)/(1|32)\z"
    just bench-one formula_caller
    just bench-one-filter argument_ingress "^argument_ingress/(f64/with_identity|string_short/borrowed|matrix_string_10k/borrowed|matrix_f64_100k/with_identity|excel_value_matrix_100k/with_identity|handle/with_identity)\z"
    just bench-one-filter array_string_output "^array_string_output/borrowed_str/16384\z"
    just bench-one-filter rtd_publish "^rtd_publish/(number|string|string_8k)/(changing|same_value)\z" "bench-internals rtd"
    just bench-one-filter rtd_refresh "^rtd_refresh/(number/end_to_end/dense|short_string/end_to_end/dense|string_8k/(collection|completion|end_to_end)/dense)\z" "bench-internals rtd"
    just bench-one-filter handle_call_resolution "^handle_call_resolution/handles/(1|8)\z"
    just bench-cache

# Fresh Quick-only metric names start a new history, without Moka-relative gates.
bench-cache:
    just bench-one-filter cache_lookup "^cache_lookup/quick/(cache_hit/u64/current/warm|cache_hit_hot_key/current/threads_(1|8|32)/u64|cache_hit_disjoint/current/threads_(8|32)/u64|eviction_with_live_lease/current)\z" "unstable-cache bench-internals"
    just bench-one-filter cache_reclamation "^cache_reclamation/quick/(churn|live_leases)/payload_64b/threads_(1|32)\z" "unstable-cache bench-internals"

# Pull request benchmark gate (aliases bench-ci to ensure identical thresholds and history).
bench-pr: bench-ci

# Scaling curves across worker thread counts, batch sizes, and data dimensions.
# Run periodically, nightly, or on-demand to observe throughput scaling characteristics.
bench-scaling:
    just bench-one-filter async_spawn "^async_spawn/(matrix_spawn|matrix_reschedule|spawn_and_drain)" "bench-internals async"
    just bench-one-filter sync_boundary "^sync_boundary/(admission|scalar_return/no_subscriber)/(4|16)\z"
    just bench-one-filter handle_prepare "^handle_prepare/(distinct_key/(4|16)|cold_grow|revision_churn)"
    just bench-one-filter handle_lookup "^handle_lookup/(warm_same_token|distinct_tokens)/(4|16)\z"
    just bench-one-filter handle_call_resolution "^handle_call_resolution/handles/(2|4)\z"
    just bench-one-filter rtd_publish "^rtd_publish/string_8k/(changing|same_value)\z" "bench-internals rtd"
    just bench-one-filter rtd_refresh "^rtd_refresh/.+/end_to_end" "bench-internals rtd"

# Full unfiltered benchmark suite.
bench-full:
    just bench-one async_spawn "bench-internals async"
    just bench-one sync_boundary
    just bench-one handle_prepare
    just bench-one formula_revision
    just bench-one handle_lookup
    just bench-one formula_caller
    just bench-one argument_ingress
    just bench-one array_string_output
    just bench-one rtd_publish "bench-internals rtd"
    just bench-one rtd_refresh "bench-internals rtd"
    just bench-one handle_call_resolution
    just bench-one cache_lookup "unstable-cache bench-internals"
    just bench-one cache_reclamation "unstable-cache bench-internals"

bench-one name features="bench-internals":
    cargo bench --package xlfn --bench {{name}} --features "{{features}}" --locked

bench-one-filter name filter features="bench-internals":
    cargo bench --package xlfn --bench {{name}} --features "{{features}}" --locked -- "{{filter}}"

bench-check:
    cargo clippy --package xlfn --benches --all-features --locked
