# Developer and CI command surface. Keep CI workflows and the testing guide
# aligned with these recipes.

# parking_lot_core 0.9.12 passes a reference to Linux's variadic futex syscall;
# newer Miri rejects its argument type. Revisit this pin after an upstream fix.
miri-toolchain := "nightly-2026-08-22"

default:
    @just --list

fmt:
    cargo fmt --all -- --check

# Build the public API reference; missing or ambiguous links block release.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked

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
miri-setup:
    rustup toolchain install {{miri-toolchain}} --profile minimal --component miri,rust-src

miri:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +{{miri-toolchain}} miri test -p xlfn-kernel --lib --locked -- miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features handles --lib --locked -- miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features async --lib --locked -- miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features rtd --lib --locked -- miri_
    just miri-cache-endpoints

# Non-owning TLS endpoint references must survive owner moves and reject reuse
# after owner destruction under both aliasing models.
miri-cache-endpoints:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features cache --lib --locked -- cache::endpoint_cache::tests::miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-tree-borrows" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features cache --lib --locked -- cache::endpoint_cache::tests::miri_

# Verus formal verification of concurrent kernel primitives.
verus:
    python3 -B -m unittest discover -s tools -p test_check_refinement_failures.py
    verus --crate-type=lib verification/verus/sealable_counter/src/lib.rs
    verus --crate-type=lib verification/verus/drain_gate/src/lib.rs
    python3 -B tools/check_drain_gate_refinement.py
    verus --crate-type=lib verification/verus/published_owner/src/lib.rs
    python3 -B tools/check_published_owner_permission.py
    verus --crate-type=lib verification/verus/operation_gate/src/lib.rs
    verus --crate-type=lib verification/verus/rotating_read_domain/src/lib.rs
    python3 -B tools/check_rotating_domain_refinement.py
    python3 -B tools/check_transition_borrows.py
    python3 -B tools/check_native_publication_borrow.py
    verus --crate-type=lib verification/verus/service_slot/src/lib.rs
    verus --crate-type=lib verification/verus/cache_lease/src/lib.rs
    python3 -B tools/check_cache_pin_refinement.py
    python3 -B tools/check_cache_ownership_refinement.py
    verus --crate-type=lib verification/verus/handle_domain/src/lib.rs
    python3 -B tools/check_handle_completion_refinement.py

# Weak-memory mutation gate for the shared final-pin release tail.
cache-release-ordering:
    python3 -B tools/check_cache_release_ordering.py

# Audit Verus verification TCB compliance (enforces 0 assumes and approved external_bodies).
verus-audit:
    python3 -B -m unittest discover -s tools -p test_check_verus_tcb.py
    python3 -B tools/check_verus_tcb.py

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

# Assemble and build registry-ready archives without invoking publication.
# Cargo verifies the archives with local path dependencies removed.
package-check: release-metadata
    cargo package --workspace --exclude xlfn-windows-bindings-gen --all-features --locked

# Keep distributable files and internal dependency pins aligned with the source.
release-metadata:
    python3 -B -m unittest discover -s tools -p test_check_release_metadata.py
    python3 -B tools/check_release_metadata.py

# Retain the existing check name without calling `cargo publish`.
publish-check: package-check

quick: fmt panic-boundaries clippy test

check: fmt panic-boundaries clippy features test bench-check deny semver doc package-check

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
    just bench-one cache_registry "bench-internals cache"
    just bench-one value_boundary_allocations "bench-internals async"

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
    just bench-one rtd_prepare "bench-internals rtd"
    just bench-one rtd_publish "bench-internals rtd"
    just bench-one rtd_refresh "bench-internals rtd"
    just bench-one handle_call_resolution
    just bench-one cache_registry "bench-internals cache"
    just bench-one value_boundary_allocations "bench-internals async"

bench-one name features="bench-internals":
    cargo bench --package xlfn --bench {{name}} --features "{{features}}" --locked

bench-one-filter name filter features="bench-internals":
    cargo bench --package xlfn --bench {{name}} --features "{{features}}" --locked -- "{{filter}}"

bench-check:
    cargo clippy --package xlfn --benches --all-features --locked

# Full-cache production policy and comparators; leak and alias checks remain enabled.
miri-cache-backends:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="" cargo +{{miri-toolchain}} miri test -p xlfn --features "cache bench-internals" --lib cache::backend_tests --locked
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-tree-borrows" cargo +{{miri-toolchain}} miri test -p xlfn --features "cache bench-internals" --lib cache::backend_tests --locked
