# Local and CI benchmark entry points. Keep the command used by Bencher the
# same as the command developers run locally.

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
