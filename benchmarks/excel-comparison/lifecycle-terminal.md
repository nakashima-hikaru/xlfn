# Lifecycle terminal-status correction, 2026-09-30

Microsoft's [Excel4/Excel12 contract](https://learn.microsoft.com/ja-jp/office/client-developer/excel/excel4-excel12)
requires returning control to Excel after `xlretAbort` or `xlretUncalced`, with
only `xlFree` permitted during cleanup. Completing removal of a generation
does not imply that further callbacks are permitted in the current invocation.

Previously, a controlled `xlAutoOpen` reload could unregister its sole UDF,
encounter a terminal status while deleting its registered name, and complete
logical removal with metadata debt. Its replacement open created a new callback
session before returning to Excel. Pending-open rollback had the same reset.
A successful final deletion followed by a terminal `xlFree` also needed to
prevent same-invocation reopening.

The host boundary now creates one `HostCallbackSession`. Removal borrows it;
pending rollback uses it; the open transaction takes ownership of it, including
metadata retry and failure rollback. Internal transactions do not create fresh
callback authority. A terminal result prevents starting a replacement open or
resetting the module callback gate during the current Excel invocation.

When a terminal result stops a controlled reload after successful removal, the
runtime remains `Closed`, retaining `ExplicitRemovalComplete` and module
residency for a subsequent close or a new Excel open invocation. The
later invocation gets a fresh session and may retry metadata debt. Successfully
deleted metadata is not resurrected when its result's `xlFree` fails. Recovery
reports terminal status even for the last committed deletion or an already
terminal session with an empty debt ledger. Existing quarantine and physical
unload opt-in policies remain in force.

## Regression evidence

The four tests in `crates/xlfn/src/boundary/host_terminal_tests.rs` exercise one
synchronous UDF with no async event registrations. They cover both terminal
statuses during `xlfEvaluate`, its result's `xlFree`, pending rollback, and the
final successful `xlfSetName` result's `xlFree`. They verify exact callback
sequences, one free per callback result, no replacement `Addin::open`, retained
or cleared metadata debt as appropriate, later retry, residency, and normal
reload/close behavior. Recovery and event tests add separate status/mutation
checks.

In an isolated `c87ed9fcf44f572ead9e9a170ce3d69db48a0340` checkout, adding only
the regression fixture and test visibility/accessors produced one passing
normal-reload control and three failing terminal tests. Those old paths
attempted replacement work and ended in `Quarantined` under the fixture,
rather than the required `Closed` return. All four tests pass with the fix,
including their Abort/Uncalced variants.

The local validation record is [lifecycle-terminal.json](lifecycle-terminal.json):
1,045 process-isolated workspace tests passed, 768 serialized xlfn libtests
passed, all 48 feature combinations compiled, Clippy and rustdoc (`-D warnings`)
passed, and the four boundary regressions passed strict-provenance Miri
under both Stacked and Tree Borrows. Minimal-feature boundary tests also passed.
x86_64 and i686 MSVC target type checks passed with `blake3/pure`.

The final candidate suite used a separate target directory after the baseline
experiment contaminated a shared test executable. Shared-target reruns were
discarded, and the default xlfn cache was rebuilt and passed all 31 related
tests. The isolated full run marked one unrelated Cargo-output unit test as
leaky; its isolated single-test rerun passed without that marker.

These are mock callback, local macOS, interpreter and cross-target checks. They
are not native Windows or real Excel observations. Existing formal certificates
cover lifecycle/resource protocols; they do not establish this Excel callback
contract. No live Excel version/build/bitness is claimed by this record.

## Event lifetime update, 2026-10-25

The September validation above predates the current event policy. Historical
registration evidence included a signed-negative, nonzero `xltypeInt` token;
registration now preserves and accepts nonzero integer tokens. A later native
interactive probe observed both handlers still firing after nil-procedure
removal, with re-registration accumulating handler generations. That failure
is preserved in the [event qualification record](event-qualification/README.md).

Production now gives calculation-event registrations loaded-module lifetime.
Each static runtime reuses its confirmed registrations across logical close
and reopen. Closed generation admission prevents late native notifications
from accessing reclaimed state, while a retained DLL reference keeps the
native entry points executable. No event-removal call or successful-removal
certificate is needed for logical close. In-flight or indeterminate event
registration also retains residency and prevents blind re-registration.

This changes neither the terminal `HostCallbackSession` rule above nor the
function/metadata cleanup obligations. `PhysicallyUnloadableAddin` can permit
lease release only with zero retained native event slots; successful logical
close with retained events is not physical-unload evidence. Matching-build
interactive close/reopen and cancellation qualification of this implementation
remains pending; the earlier observations are not validation of the new code.
