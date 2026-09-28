# Choose a calculation pattern

Start with a normal worksheet function. Add state or background work when a
specific requirement calls for it. These facilities can be combined, but each
has a different owner and update trigger.

## Match the feature to the task

| Requirement | Use | What controls its lifetime or updates? | Cargo feature |
| --- | --- | --- | --- |
| Compute a result from the current arguments | [Worksheet function](worksheet-functions.md) | Excel calls the function during recalculation | None |
| Share configuration or a service across functions | [Application state](lifecycle.md) | The add-in's open generation | None |
| Keep a typed Rust object that other formulas use | [Formula-owned handle](handles.md) | The producing formula and its input revision | `handles` |
| Return one result after asynchronous work finishes | [Async function](async-functions.md) | The current calculation and cancellation | `async` |
| Push repeated updates into a cell | [RTD source](rtd.md) | The active subscription | `rtd` |
| Reuse computed values across calls and bound resident cache weight | [Calculation cache](caching.md) | Your keys, weight budget, eviction, and invalidation | `cache` |

The crate has no default features. Enable only the facilities your add-in requires
in `Cargo.toml`.

## One result or a stream?

An async function completes one invocation. It suits a request that waits for
data and then produces a result. A new calculation creates a new invocation;
cancellation makes work from an obsolete calculation stop being useful.

RTD keeps a subscription alive and publishes new values as a source changes.
Use it for a live quote, sensor reading, or progress value that should update
without the user editing a formula.

Neither feature makes blocking work asynchronous. If an operation blocks, run it
on a dedicated thread pool so it does not block the async runtime or Excel threads.
See [Do not block the async executor](async-functions.md#do-not-block-the-async-executor).

## A worksheet object or an internal cache?

A handle is a worksheet-visible reference to a typed Rust object. For example,
one formula creates a parsed data set and several other formulas query it.
For an unchanged formula revision, xlfn reuses the object. Changes to external
data must be represented by an explicit input or another deliberate update
design; recalculation alone does not promise to rebuild it.

A cache reuses a calculation's result without introducing a worksheet-visible
object. A miss recomputes the value. Choose a key and clear or invalidate
entries when the data they depend on changes. Live leases can keep evicted
values in memory beyond the resident-weight budget.

Store an application's cache or service client in shared state.

## Pick the execution mode separately

Execution mode answers which Excel operations a function may perform and where
it may run. It is separate from the decision to own state or cache a result.

- Pure calculations can use `thread_safe` when the entire call path supports
  concurrent execution.
- Creating a new handle object uses main-thread return semantics.
- Reading coordinates or using reference coercion requires macro-sheet mode.
- An `async fn` uses the asynchronous mode and its own cancellation rules.

The [execution-mode table](execution-modes.md#compatibility-table) lists the
supported combinations. Once you choose a pattern, follow its linked guide
for a minimal example and the lifecycle rules that accompany it.
