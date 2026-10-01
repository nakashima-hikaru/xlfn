# Rust 1.99 pin projection audit

Audited against Rust 1.99.0 (`b940084d7`, 2026-09-28), using the installed
`library/core/src/pin.rs` source. The two workspace calls to
`Pin::new_unchecked` require no behavioral change.

The [constructor contract](https://doc.rust-lang.org/1.99.0/core/pin/struct.Pin.html#method.new_unchecked)
requires the pointee to remain at its address, with valid storage, until it is
dropped. The obligation persists after the temporary pinned borrow ends. The
pointer's safe trait implementations must also obey the
[`PinSafePointer` contract](https://doc.rust-lang.org/1.99.0/core/pin/trait.PinSafePointer.html):
dereferencing preserves the object and concrete type, mutation and pointer drop
do not move the pointee, and cloning or shared access cannot defeat the pin.
The standard library provides the `&mut T` implementation. The trait itself
remains unstable; this audit uses its documented contract and does not add an
implementation or require a feature gate.

- `async_udf/future.rs`: `NoUnwindFuture::poll` projects a pinned wrapper to its
  private `ManuallyDrop<F>` field and constructs `Pin<&mut F>`. `ManuallyDrop`
  is dereferenced before the pointer is pinned; it is not the `Pin` pointer
  type. No method extracts or replaces `F`. `finish` and `Drop` both use
  `destroy`, which marks destruction before user code and drops `F` once in
  place, including when its destructor panics.
- `async_udf/task.rs`: `TrackedFuture::poll` constructs
  `Pin<&mut NoUnwindFuture<Abortable<F>>>` from the pinned wrapper. The private
  future field is never moved after pinning. Its field destructor runs in
  place before `CompletionGuard`, preserving the task's reclamation ownership
  until user cleanup has completed. Mutating completion observation does not
  move the future field.

The existing `miri_future_destruction_preserves_pin_and_runs_exactly_once` test
checks a `!Unpin` future's address through poll and in-place destruction, with
both normal and panicking destructors. Async task tests separately cover
cancellation before poll and reclamation ordering. Passing those checks is
supporting execution evidence; the structural pinning and destruction
arguments above are the safety justification.

## Miri execution evidence

With `nightly-2026-09-02` (Rust 1.100.0 nightly, `5db7f4be8`), Linux-targeted
Miri passed all 23 kernel and 29 async-selection tests on 2026-10-01. Both
runs used `--target x86_64-unknown-linux-gnu --lib --locked -- miri_
--test-threads=1`; the async run used `--no-default-features --features async`.
These were interpreted Linux-target executions from the macOS host, not native
Linux or Windows runs. This nightly includes the Rust 1.99 Box ownership APIs,
the updated pin contract, and `raw_borrows_via_references`.

`nightly-2026-09-28` could not complete either Linux selection: contended waits
reached `parking_lot_core` 0.9.12's incorrectly typed variadic futex argument.
The upstream [issue](https://github.com/Amanieu/parking_lot/issues/542) and
[pending two-line fix](https://github.com/Amanieu/parking_lot/pull/539) describe
the same failure. The 2026-09-02 pin preserves Linux Miri coverage while keeping
the released dependency unchanged until its upstream repair is available.

These full selections used Miri's default provenance mode. Strict provenance
is separately blocked by `parking_lot_core`'s integer-to-pointer conversion in
`word_lock.rs`, also tracked [upstream](https://github.com/Amanieu/parking_lot/issues/541).
The passing async run additionally warned about `crossbeam-epoch`'s
integer-to-pointer conversion. Passing the default-mode selections does not
establish strict-provenance compatibility for those dependencies.
