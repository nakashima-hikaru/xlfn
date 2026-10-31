# Verus verification

This directory contains implementation-oriented protocol and ownership proofs.
Shared transition and control expressions connect parts of these proofs to
production Rust. The remaining native representation obligations are listed in
[the worklist](REFINEMENT_WORKLIST.md); passing model proofs does not establish
an end-to-end proof of the native implementation.

The verification layers have distinct scopes:

| Layer | Scope |
| --- | --- |
| [Lean](../../formal/README.md) | Lifecycle, shutdown and temporal ownership protocols |
| Verus | Shared arithmetic/control expressions and resource-backed protocol models |
| Loom | Exhaustive or explicitly bounded modeled memory-order interleavings |
| Miri | Executed pointer, aliasing and ownership paths under both borrow models |

## Maintained proof targets

| Target | Shared or modeled behavior | Remaining native boundary |
| --- | --- | --- |
| `sealable_counter` | Checked 32/64-bit admission, release, seal and reopen transitions | Actual atomic location and concurrent history |
| `drain_gate` | Release/notify/unlock, wait reobservation, striped drain, idle seal and rollback | Native atomics, guards, wait/notify and stripe identity |
| `published_owner` | Initialized memory, provenance and allocator permission ownership | Native Box conversion, unsized payloads and Drop |
| `operation_gate` | Admission closure and permit conservation | Native guard and owner-lifetime correspondence |
| `rotating_read_domain` | Reader retry, seal/publish/reopen, pending/idle recovery and certified queue withdrawal | Native atomic/lock/queue identity and weak memory |
| `service_slot` | Publication, withdrawal and fault-state ownership | Native pointer publication and destruction |
| `cache_lease` | Pin/observation resources, shared lookup, retirement and heap-permission recovery | Native pin field, resident index, queue cutoff and allocation recovery |
| `handle_domain` | Retained observations, publication, batch recovery and post-destructor debt accounting | Native binding/debt fields, queue history and Drop |

Cache pins use `AtomicU32` in production; admission counters remain `usize`.
Both-width arithmetic proofs alone do not establish this mixed-width native
composition. Verus objects and vstd atomics are separate from the production
fields unless a specific source connection is stated.

Invariant identifiers link corresponding Lean, Verus and Rust obligations.
They do not constitute a mechanically checked cross-prover theorem. See
[TCB.md](TCB.md) for trusted primitives, audit policy and claim limits, and
[NATIVE_ADAPTER_PLAN.md](NATIVE_ADAPTER_PLAN.md) for remaining implementation work.

## Running verification

The installer needs `bash`, `curl`, `unzip`, Python 3 and `rustup`:

```bash
./tools/install_verus.sh
just verus-audit
just verus
just cache-release-ordering
```

The installer checks the pinned release against
`tools/verus_release_sha256.json`, reads the required Rust toolchain from the
release's `version.json`, and checks that Verus starts. It refuses an existing
destination. The default destination is
`~/.local/share/xlfn/verus/<version>/<platform>`; add it to `PATH` or supply a new
dedicated directory as the first argument. Updating the verifier requires
reviewing and committing the corresponding platform hashes.

`just verus` runs the proof targets, negative mutation suites, and Verus/native
borrow-rejection checks. Mutations check sensitivity of the maintained proofs;
they do not establish completeness. Parser errors, timeouts and unrelated
compiler errors never count as proof rejection. The explicitly named ownership
case requires its expected Rust diagnostic.

The runner shares a baseline only when its entry point and copied input bytes
match. Each mutation starts from an immutable temporary snapshot. Results are
not cached between invocations or machines.

```bash
# Inspect exact inputs and rejection criteria without invoking Verus.
python3 -B tools/check_verus_refinement.py --list

# Save timings locally when diagnosing verification throughput.
python3 -B tools/check_verus_refinement.py --jobs 2 --report /tmp/verus-timings.json
```

CI shards mutation inputs and checks that reports cover every current case
exactly once, with matching baseline inputs and verifier version. Timing reports
are run artifacts; they are not checked-in performance baselines.
