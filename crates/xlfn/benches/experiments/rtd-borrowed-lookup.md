# RTD borrowed lookup and own-on-miss candidate

The candidate meets all requested **median-based qualification gates** in this
local experiment. Existing reuse falls to **11.36 µs for one part** and
**22.46 µs for ten parts**, including validation and hash computation, per
256-subscription batch. Existing borrowed allocation, reallocation, and free
counts are all zero. New/churn also improve versus the pristine arena's owned
end-to-end baseline. Canonical topics remain owned only by `SubscriptionEntry`.

The 25 µs result is a median gate, not an every-run bound: ten-part existing
measured **26.15, 22.18, and 22.46 µs**. The first run exceeded the cutoff. The
candidate is retained as an experiment; the public API and the default checkout
are not promoted by this result. A later change can decide `subscribe_parts`
and convergence of the owned and borrowed lookup implementations.

## End-to-end results

Apple M1, macOS 26.6.2, Rust 1.98.0 (`88d9e12ae`, 2026-08-18), System allocator.
Times are microseconds per 256 subscriptions, using the median of three
5-second / 50-sample Criterion slope estimates.

| Operation | Parts | Arena owned baseline | Borrowed candidate | Change | Gate | Result |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| new | 1 | 59.13 | 42.96 | -27.4% | ≤ +5% | Pass |
| existing | 1 | 40.73 | 11.36 | -72.1% | ≤ baseline | Pass |
| churn | 1 | 78.49 | 61.58 | -21.5% | ≤ +5% | Pass |
| new | 10 | 155.81 | 99.54 | -36.1% | ≤ +5% | Pass |
| existing | 10 | 176.85 | 22.46 | -87.3% | ≤25 µs | Pass |
| churn | 10 | 207.12 | 143.76 | -30.6% | ≤ +5% | Pass |

The same-binary **owned control** using the candidate's shared validator measured
36.39 µs (one part) and 144.08 µs (ten parts) for existing reuse. Thus the result
is not solely the faster validator: borrowed reuse still avoids the owned
control's temporary topic allocation and destruction. All six owned-control
results and every raw estimate are in `rtd-borrowed-lookup-results.json`.

The earlier 55 µs arena measurement excluded topic construction. It is not the
baseline for this qualification: the 176.85 µs end-to-end baseline includes
copying the caller strings, validation/hash, lookup, commit, and input teardown.

A first complete screening triplet retained unconditional UTF-16 scanning;
borrowed ten-part existing was 51.83 µs and failed the target despite zero
allocations. Adding the shared UTF-8 byte-bound proof described below brought
it into the final range. That preliminary triplet is preserved separately in
the JSON and is not included in the final medians.

## Allocation gate

Counts below come from the candidate's separate System-allocator probe,
per 256 existing subscriptions. These counts are not inferred from timing.

| Route | Parts | Allocations | Reallocations | Deallocations |
| --- | ---: | ---: | ---: | ---: |
| Owned control | 1 | 512 | 256 | 512 |
| Owned control | 10 | 2,816 | 768 | 2,816 |
| Borrowed | 1 | **0** | **0** | **0** |
| Borrowed | 10 | **0** | **0** | **0** |

The probe asserts all three borrowed counts are zero and fails on any violation.
Caller-owned string creation is excluded; there are no retained framework topic
copies on hits, and misses materialize only the canonical entry's topic.

## Implementation and ownership

The experimental `prepare_parts(&source, [&str; N])` validates borrowed input,
computes the existing canonical topic hash, and scans the source/hash bucket.
Each candidate slot is generation-checked and compared against the canonical
entry using hash, part count, and every string's bytes. Lookup returns only a
slot, logical ID, and connected-state flag before reservation mutation. Slots
and catalog borrows never escape the mutex.

An existing input remains references on the stack. A miss materializes exactly
one `Box<[String]>` plus its string payloads and moves already validated metadata
into `RtdTopic`; it neither revalidates nor rehashes. The identity buckets retain
only slots, and `SubscriptionEntry` remains the sole canonical topic owner.
No interning, `Arc<str>`, contiguous-topic storage, or new public trait is added.
Quota rejection after a miss may allocate and immediately destroy this owned
topic, matching the existing admission contract.

The ordinary owned `prepare(source, RtdTopic)` body is byte-for-byte unchanged.
The borrowed runtime methods are available only in tests or with
`bench-internals` plus `rtd`. Both owning and borrowed construction use the same
`validate_topic_parts` function and `TopicMetadata`. The public `Into<String>`
constructor retains its bounded conversion loop and original error precedence;
arbitrary `Into<String>` values must be converted before their text is available.
The arena catalog is byte-for-byte unchanged from the stable-slot candidate.
The default checkout remains the preceding non-owning map implementation;
these changes are delivered as experiment patches, without `subscribe_parts`.

For UTF-16 validation, `str::len() <= 32767` proves the code-unit limit is met:
each Unicode scalar consumes no more UTF-16 units than UTF-8 bytes. Longer UTF-8
inputs still run the exact `encode_utf16().count()` validation, so multibyte
strings can be accepted even when their byte length exceeds 32767. Limits,
error precedence, total UTF-8 accounting, and the FxHasher recipe remain intact.

## Measurement scope

Each batch contains 256 caller-owned arrays of one or ten strings. Part text is
`market-{index:06}-field-{part:02}` (22 UTF-8 bytes per part). Caller string
creation and fixture setup/teardown are excluded. Existing fixtures seed
committed pending subscriptions before timing. These measurements do not make
Excel/COM calls, and existing is a pending-reuse benchmark, not an active-server
latency measurement.

- Owned: construct `RtdTopic` from the caller strings, validate/hash, prepare,
  and commit, including allocation and destruction of the temporary topic.
- Borrowed: assemble `[&str; N]`, validate/hash, look up, materialize on miss,
  prepare, and commit. There is no precomputed topic hash in the timed input.
- Churn: prepare the entire new batch into a reservation vector, then roll back
  every reservation. Vector allocation/destruction is timed for both variants.

The baseline executable uses the pristine stable-slot constructor and owned
prepare. A second owned control uses the candidate's shared validator; this
prevents attributing the validator improvement entirely to borrowed ownership.
Baseline and candidate use the same fixture logic (only formatting differs)
and separate build outputs. The baseline-only harness shim is never selected in a baseline timing.
The System counting allocator is linked only into a separate allocation probe.
Caller input creation, existing seeding, and fixture destruction are outside
that probe's tracking interval.

Final runs use release optimization, 1 s warm-up, 5 s measurement, 50 samples,
and three repetitions. The qualifying pair order is baseline/borrowed,
borrowed/baseline, baseline/borrowed; each pair is followed by the candidate
owned control. Only one timing process runs at once, after builds/tests finish.
Results are medians of Criterion slope point estimates per 256 subscriptions.
Percent changes compare those medians, not paired statistical intervals.
Per-run estimates and their intervals are preserved in the JSON artifact.
This local macOS/M1 experiment does not establish Windows/Excel production
latency or performance for longer, arbitrary Unicode topics.

## Correctness and validation

`just quick` passed formatting, panic-boundary auditing, all-workspace/all-target
Clippy, and **801 nextest tests**; the profile skipped seven tests. Seven added
tests cover:

- Owning/borrowed acceptance and error agreement at 0/253/254 parts, empty-part
  precedence, 1 MiB byte limit, UTF-16 limits, and embedded NUL bytes.
- An independent full UTF-16-count oracle for ASCII, two- and three-byte BMP
  scalars, and non-BMP scalars on both sides of the byte-bound shortcut.
- The pre-experiment canonical hash recipe and forced-collision equality,
  including part-boundary differences; materialization preserves forced metadata.
- Caller data dropping before the reservation, canonical storage stability on
  hits, pending rollback, connecting/active reuse without extra reservations.
- Source isolation, colliding identities after removal and slot reuse, old slot
  invalidation, owned/borrowed interoperability, and quota/closing/stale errors.

Both supplemental patches apply cleanly to the pristine arena snapshot. Applying
the candidate patch reproduces the tested source exactly, including the unchanged
arena catalog. No production public API change is part of this experiment.

## Reproduce

Use two checkouts of the preceding non-owning map change, including the experiment
`.rs` files beside this report. Apply `rtd-slot-arena.patch` to both. Then apply
**one** of these alternative supplemental patches to each checkout:

```sh
# Baseline checkout, after the arena patch:
git apply crates/xlfn/benches/experiments/rtd-borrowed-baseline-harness.patch

# Candidate checkout, after the arena patch:
git apply crates/xlfn/benches/experiments/rtd-borrowed-lookup.patch
CARGO_TARGET_DIR=/absolute/path/to/candidate-build just quick
```

Do not apply the earlier existing-breakdown diagnostic patch. Use a distinct
build directory per checkout. The common benchmark file includes both groups;
select only `rtd_owned_e2e` in the baseline, whose borrowed shim intentionally
just delegates to owning construction. Run `rtd_borrowed` and the additional
`rtd_owned_e2e` control in the candidate:

```sh
XLFN_BENCH_MEASUREMENT_MS=5000 CARGO_TARGET_DIR=/absolute/path/to/build \
  cargo bench -p xlfn --bench rtd_borrowed --features 'bench-internals rtd' \
  --locked -- --warm-up-time 1 --sample-size 50 --save-baseline run-1 '^rtd_owned_e2e/'

# Candidate checkout only:
CARGO_TARGET_DIR=/absolute/path/to/candidate-build cargo bench -p xlfn \
  --bench rtd_borrowed_allocations --features 'bench-internals rtd' --locked
```

Repeat with distinct save-baseline names and the group filter `^rtd_borrowed/`
for the candidate borrowed timings. The allocation probe exits unsuccessfully
if either existing borrowed case makes any allocation, reallocation, or free.
