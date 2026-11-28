# Developer and CI command surface. Keep CI workflows and the testing guide
# aligned with these recipes.

# Supports the Rust 1.100.0 stabilized APIs (`DropGuard`, `Allocator`,
# `UnsafeCell::as_mut_unchecked`). On `target_os = "linux"` (CI only), `xlfn`
# leaves `quick_cache/parking_lot` disabled so Linux Miri avoids
# `parking_lot_core` 0.9.12's variadic `futex` syscall check (upstream #542 / fix #539).
miri-toolchain := "nightly-2026-09-28"

default:
    @just --list

# Managed Python tooling, shared by local development and CI.
python-setup:
    uv sync --project tools --locked

python-check:
    uv run --project tools --locked ruff check tools
    uv run --project tools --locked ruff format --check tools
    uv run --project tools --locked ty check --project tools --error-on-warning
    uv run --project tools --locked python -B -m unittest discover -s tools/tests -p 'test_*.py'

python-format:
    uv run --project tools --locked ruff check --fix tools
    uv run --project tools --locked ruff format tools

fmt:
    cargo fmt --all -- --check

# Build the public API reference; missing or ambiguous links block release.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked

# Audit every direct catch reference, including test and macro token streams.
panic-boundaries:
    uv run --project tools --locked python -B -m unittest discover -s tools/tests -p test_check_panic_boundaries.py
    uv run --project tools --locked python -B -m xlfn_tools.check_panic_boundaries

# Compile valid and invalid callers against private lifecycle typestates.
typestate:
    uv run --project tools --locked python -B -m unittest discover -s tools/tests -p test_check_typestate.py
    uv run --project tools --locked python -B -m xlfn_tools.check_typestate

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

miri: miri-core-lane miri-async-lane miri-rtd-lane

miri-core-lane:
    CARGO_BUILD_WARNINGS=allow cargo +{{miri-toolchain}} miri test --manifest-path probes/ghostcell-call-model/Cargo.toml --locked
    CARGO_BUILD_WARNINGS=allow MIRIFLAGS="-Zmiri-tree-borrows" cargo +{{miri-toolchain}} miri test --manifest-path probes/ghostcell-call-model/Cargo.toml --locked
    # Layer metadata reads wall-clock time, so run these reentry cases without isolation.
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-disable-isolation" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --lib --locked -- scalar_publication_survives --test-threads=1
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +{{miri-toolchain}} miri test -p xlfn-kernel --lib --locked -- miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +{{miri-toolchain}} miri test -p xlfn-host --lib --locked -- drain::threaded::tests::miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +{{miri-toolchain}} miri test -p xlfn-host --lib --locked -- process_quota::tests::miri_
    just miri-cache-ownership
    just miri-cache-endpoints
    just miri-cache-resident-entry
    just miri-cache-residency

miri-async-lane:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features handles --lib --locked -- miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-disable-isolation" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features async-builtin --lib --locked -- miri_ --test-threads=1
    just miri-async
    just miri-async-handles

miri-rtd-lane:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features rtd --lib --locked -- miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-tree-borrows" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features rtd --lib --locked -- miri_

# Executor-independent registry accounting under both aliasing models.
# Strict provenance is limited to calculation pins: parking_lot's blocking
# paths currently perform integer-to-pointer reconstruction.
miri-async:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-disable-isolation" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features async --lib --locked -- async_udf:: --test-threads=1
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-tree-borrows -Zmiri-disable-isolation" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features async --lib --locked -- async_udf:: --test-threads=1
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-strict-provenance" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features async --lib --locked -- async_udf::registry::registration_tests::miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-strict-provenance -Zmiri-tree-borrows" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features async --lib --locked -- async_udf::registry::registration_tests::miri_

# Scoped futures must dispose user captures before releasing registry authority.
miri-async-handles:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-disable-isolation" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features "async-builtin handles" --lib --locked -- async_udf::boundary::scoped_tests::miri_ --test-threads=1
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-tree-borrows -Zmiri-disable-isolation" cargo +{{miri-toolchain}} miri test -p xlfn --no-default-features --features "async-builtin handles" --lib --locked -- async_udf::boundary::scoped_tests::miri_ --test-threads=1

# Creator rollback, final-pin ordering and zero-budget Box recovery.
miri-cache-ownership:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="" cargo +{{miri-toolchain}} miri test -p xlfn-services --no-default-features --features cache --lib --locked -- cache::tests::miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-tree-borrows" cargo +{{miri-toolchain}} miri test -p xlfn-services --no-default-features --features cache --lib --locked -- cache::tests::miri_

# Resident pointer tags preserve provenance and release ownership exactly once.
miri-cache-resident-entry:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-strict-provenance" cargo +{{miri-toolchain}} miri test -p xlfn-services --no-default-features --features cache --lib --locked -- cache::resident_index::tests
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-strict-provenance -Zmiri-tree-borrows" cargo +{{miri-toolchain}} miri test -p xlfn-services --no-default-features --features cache --lib --locked -- cache::resident_index::tests

# Non-owning TLS endpoint references must survive owner moves and reject reuse
# after owner destruction under both aliasing models.
miri-cache-endpoints:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="" cargo +{{miri-toolchain}} miri test -p xlfn-services --no-default-features --features cache --lib --locked -- cache::endpoint_cache::tests::miri_
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-tree-borrows" cargo +{{miri-toolchain}} miri test -p xlfn-services --no-default-features --features cache --lib --locked -- cache::endpoint_cache::tests::miri_

# Verus formal verification of concurrent kernel primitives.
verus:
    just verus-audit
    just verus-refinement

# Verify all Verus proof targets.
verus-refinement:
    uv run --project tools --locked python -B -m unittest discover -s tools/tests -p 'test_verus_refinement_runner.py'
    uv run --project tools --locked python -B -m xlfn_tools.check_verus_refinement

# Audit Verus verification TCB compliance (enforces 0 assumes and approved external_bodies).
verus-audit:
    uv run --project tools --locked python -B -m unittest discover -s tools/tests -p test_check_verus_tcb.py
    uv run --project tools --locked python -B -m xlfn_tools.check_verus_tcb

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

test-async-builtin:
    cargo test \
        --package xlfn \
        --no-default-features \
        --features async-builtin \
        --locked \
        -- \
        --test-threads=1

# Exercise production feature selections without benchmark/refinement helpers.
test-features partition="1/1":
    uv run --project tools --locked python -B -m xlfn_tools.feature_matrix test --shard {{partition}}

# Check every distinct resolved combination, including internal interactions.
features partition="1/1":
    uv run --project tools --locked python -B -m xlfn_tools.feature_matrix check --shard {{partition}}

deny:
    cargo deny --locked check
    cargo deny --manifest-path benchmarks/excel-comparison/xlfn/Cargo.toml --locked --config deny.toml check
    cargo deny --manifest-path examples/basic-xll/Cargo.toml --locked --config deny.toml check
    cargo deny --manifest-path examples/rtd-source/Cargo.toml --locked --config deny.toml check
    cargo deny --manifest-path probes/excel-abi-probe/Cargo.toml --locked --config deny.toml check
    cargo deny --manifest-path probes/ghostcell-call-model/Cargo.toml --locked --config deny.toml check
    cargo deny --manifest-path probes/lifecycle-stack-model/Cargo.toml --locked --config deny.toml check
    cargo deny --manifest-path tests/rtd-api-fixture/Cargo.toml --locked --config deny.toml check
    cargo deny --manifest-path tests/xlfn-e2e-fixture/Cargo.toml --locked --config deny.toml check

semver:
    # Derive compatibility requirements from each crate's actual version.
    # A workspace-wide major override would skip checks even for crates
    # whose compatibility line has not changed. Each implementation-only
    # crate omitted here lacks a 0.1.0 tag baseline; the facade snapshots also
    # track the definitions it re-exports from those crates.
    cargo semver-checks \
        --workspace \
        --exclude xlfn-kernel \
        --exclude xlfn-engine \
        --exclude xlfn-host \
        --exclude xlfn-services \
        --baseline-rev 0.1.0

# Assemble and build registry-ready archives without invoking publication.
# Cargo verifies the archives with local path dependencies removed.
package-check: release-metadata
    cargo package --workspace --exclude xlfn-windows-bindings-gen --all-features --locked

# Keep distributable files and internal dependency pins aligned with the source.
release-metadata:
    uv run --project tools --locked python -B -m unittest discover -s tools/tests -p test_check_release_metadata.py
    uv run --project tools --locked python -B -m xlfn_tools.check_release_metadata

# Retain the existing check name without calling `cargo publish`.
publish-check: package-check

quick: fmt python-check panic-boundaries typestate clippy test

# Validate the published guide and compile its examples against this checkout.
guide-check:
    uv run --project tools --locked python -B -m unittest discover -s guide -p 'test_*.py'
    mdbook build guide
    uv run --project tools --locked python -B guide/check.py --html
    uv run --project tools --locked python -B guide/check_examples.py

check: fmt python-check panic-boundaries typestate clippy features test test-features bench-check deny semver doc guide-check package-check

# --- Benchmark recipes ---

# Bencher continuous integration regression suite (representative production cases).
# Used by default for main branch history tracking and pull request regression gating.
bench: bench-ci

# Canonical CI regression suite: deterministic allocation probes plus
# representative hot-path Criterion cases.
bench-ci:
    just bench-one value_boundary_allocations "bench-internals async cache"
    just bench-one async_task_drain_allocations "bench-internals async"
    just bench-one async_pending_allocations "bench-internals async-builtin"
    just bench-one rtd_prepare_allocations "bench-internals rtd"
    just bench-one rtd_connection_allocations "bench-internals rtd"
    just bench-one rtd_publish_allocations "bench-internals rtd"
    just bench-one-filter sync_boundary "^sync_boundary/((admission|scalar_return/no_subscriber|domain_error/no_subscriber)/1|scalar_arguments/8/1)\z"
    just bench-one-filter argument_ingress "^argument_ingress/(native_abi/f64_8/plain/(q|native)|string_short/borrowed|string_japanese_4k/owned|matrix_f64_100k/plain|numeric_matrix_ref_100k/plain)\z"
    just bench-one-filter array_numeric_output "^array_numeric_output/(matrix_build_return|numeric_slice_convert_return)/100000\z"
    just bench-one-filter array_string_output "^(array_string_output/borrowed_str/16384|scalar_output/ascii_short/borrowed|scalar_output/japanese_4k/owned)\z"
    just bench-one-filter handle_prepare "^handle_prepare/(cold_miss_batch_100|warm_hit_batch_100)\z"
    just bench-one-filter handle_lookup "^handle_lookup/(warm_same_token|distinct_tokens)/1\z"
    just bench-one-filter cache_registry "^(cache_registry/distinct_endpoints/1|cache_borrowed_keys/registry_borrowed_warm_hit/1024)\z" "bench-internals cache"
    just bench-one-filter cache_miss_concurrency "^cache_miss_concurrency/(distinct_keys|same_key)/cheap_u64/cache/workers_4\z" "bench-internals cache"
    just bench-one-filter async_spawn "^(async_spawn/per_iteration/1|async_spawn/spawn_and_drain/workers_4/16)\z" "bench-internals async-builtin"
    just bench-one-filter rtd_publish "^rtd_publish/(number|string_8k)/changing\z" "bench-internals rtd"
    just bench-one-filter rtd_refresh "^rtd_refresh/(number|string_8k)/end_to_end/dense\z" "bench-internals rtd"

# Pull request benchmark gate (aliases bench-ci to ensure identical thresholds and history).
bench-pr: bench-ci

# Scaling curves across worker thread counts, batch sizes, and data dimensions.
# Run periodically, nightly, or on-demand to observe throughput scaling characteristics.
bench-scaling:
    just bench-one cache_miss_concurrency "bench-internals cache"
    just bench-one-filter async_spawn "^(async_spawn/(matrix_spawn|matrix_reschedule|spawn_and_drain)|cancellation_lifecycle/)" "bench-internals async-builtin"
    just bench-one-filter sync_boundary "^sync_boundary/(admission|scalar_return/no_subscriber)/(4|16)\z"
    just bench-one-filter handle_prepare "^handle_prepare/(distinct_key/(4|16)|cold_grow|drop|revision_churn)"
    just bench-one-filter handle_lookup "^handle_lookup/(warm_same_token|distinct_tokens|rotating_65536_tokens)/(4|16)\z"
    just bench-one-filter rtd_publish "^rtd_publish/string_8k/(changing|same_value)\z" "bench-internals rtd"
    just bench-one-filter rtd_refresh "^rtd_refresh/.+/end_to_end" "bench-internals rtd"

# Full unfiltered benchmark suite.
bench-full:
    just bench-one async_task_drain_allocations "bench-internals async"
    just bench-one async_pending_allocations "bench-internals async-builtin"
    just bench-one array_numeric_output
    just bench-one cache_miss_concurrency "bench-internals cache"
    just bench-one async_spawn "bench-internals async-builtin"
    just bench-one sync_boundary
    just bench-one handle_prepare
    just bench-one handle_lookup
    just bench-one argument_ingress
    just bench-one array_string_output
    just bench-one rtd_prepare_allocations "bench-internals rtd"
    just bench-one rtd_connection_allocations "bench-internals rtd"
    just bench-one rtd_publish_allocations "bench-internals rtd"
    just bench-one rtd_publish "bench-internals rtd"
    just bench-one rtd_refresh "bench-internals rtd"
    just bench-one cache_registry "bench-internals cache"
    just bench-one value_boundary_allocations "bench-internals async cache"

# Strict Q versus native B/A/J/D% admission, including handle input identity.
bench-native-abi:
    just bench-one-filter argument_ingress "^argument_ingress/native_abi/"

bench-one name features="bench-internals":
    cargo bench --package xlfn --bench {{name}} --features "{{features}}" --locked

bench-one-filter name filter features="bench-internals":
    cargo bench --package xlfn --bench {{name}} --features "{{features}}" --locked -- "{{filter}}"

bench-check:
    cargo clippy --package xlfn --benches --all-features --locked

# Full-cache residency and lease ownership; leak and alias checks remain enabled.
miri-cache-residency:
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="" cargo +{{miri-toolchain}} miri test -p xlfn-services --no-default-features --features cache --lib cache::residency_tests --locked
    CARGO_BUILD_WARNINGS=allow RUSTFLAGS="-A deprecated" MIRIFLAGS="-Zmiri-tree-borrows" cargo +{{miri-toolchain}} miri test -p xlfn-services --no-default-features --features cache --lib cache::residency_tests --locked

# Standalone lifetime model, including compile-fail escape and brand checks.
ghostcell-model:
    cargo test --manifest-path probes/ghostcell-call-model/Cargo.toml --locked

# Review additions and removals on portable and Windows surfaces, with fixed tooling.
public-api:
    uv run --project tools --locked python -B -m xlfn_tools.check_public_api

public-api-update:
    uv run --project tools --locked python -B -m xlfn_tools.check_public_api --update
