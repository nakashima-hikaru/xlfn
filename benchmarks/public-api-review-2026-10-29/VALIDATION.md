# Public API change validation

Local verification uses Rust 1.99.1 on aarch64-apple-darwin. Source and
regression fixtures are included in the same commit as this record.

| Check | Result |
|---|---|
| `cargo nextest run --workspace --all-features --locked --no-fail-fast` | 1,527 passed, 9 skipped |
| `cargo nextest run -p xlfn --no-default-features --locked` | 484 passed, 7 skipped |
| Public all-feature trybuild consumers | 86 expected pass/fail cases matched |
| Public `async`-only trybuild consumers | 63 expected pass/fail cases matched |
| `cargo clippy --workspace --all-targets --all-features --locked` | Passed, warnings denied |
| `cargo test --doc --workspace --all-features --locked` | Passed |
| `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps --locked` | Passed |
| `just guide-check` | 17 chapters and standalone RTD project passed |
| `python3 -B tools/check_typestate.py` | 149 passed; 26 Windows-only cases skipped |
| Windows typestate checker, x64, `--extra-features blake3/pure` | All 175 cases passed |
| Full-feature Windows x64 `cargo check`, `--features blake3/pure` | Passed |
| RTD-only Windows x64 and x86 `cargo check` | Passed |
| `python3 -B tools/check_panic_boundaries.py` | 52 reviewed direct references; passed |
| `python3 -B tests/rtd-api-fixture/check.py` | 20 independently featured consumer outcomes matched |
| Miri, `rtd`, `miri_boxed_subscription` | 3 ownership/panic tests passed |
| Miri, `handles`, `fresh_handle_revisions_run_factory` | Fresh revision, failed creation and old-lease retirement test passed |
| Production feature powerset (`cargo hack check`, excluding default/bench-internals/refinement) | All 192 configurations passed |
| Formatting and `git diff --check` | Passed |

Miri used the repository's nightly-2026-09-02 toolchain and existing
`CARGO_BUILD_WARNINGS=allow RUSTFLAGS='-A deprecated'` compatibility settings.
The raw local execution logs are retained in `.cache/public-api-review/`.
The array measurements and RTD consumer logs/checksums are also tracked here
and under `tests/rtd-api-fixture/evidence/` respectively.

The initial native Windows full-feature cross-check could not find `lib.exe`
for BLAKE3's assembly backend. The successful full-feature cross-check and
Windows typestate run therefore use the pure Rust backend. RTD-only checks
need no such substitution. None of these checks executes Windows, Excel,
COM, or a worksheet. Native Windows CI and real Excel behavior remain
separate qualification evidence. The array ABI measurements have the narrower
scope and noise limitations stated in README.md.
