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
        --baseline-rev 0.1.0 \
        --release-type major

publish-check:
    cargo publish --workspace --dry-run --locked

quick: fmt clippy test

check: fmt clippy features test bench-check deny semver

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

bench-one name features="bench-internals":
    cargo bench --package xlfn --bench {{name}} --features "{{features}}" --locked

bench-one-filter name filter features="bench-internals":
    cargo bench --package xlfn --bench {{name}} --features "{{features}}" --locked -- "{{filter}}"

bench-check:
    cargo clippy --package xlfn --benches --all-features --locked
