# Thread-affine native state

Many native APIs require a context and every derived object to be created, used, and destroyed on one thread. A thread-bound worker architecture encodes that operational model without declaring `T: Send`.

## Owner and handle

Use a channel or dedicated worker thread model (such as a worker thread receiving channel commands):

```rust
use std::sync::mpsc;
use std::thread;

pub struct ThreadBoundWorker {
    tx: mpsc::Sender<Box<dyn FnOnce(&mut NativeContext) + Send>>,
}
```

The owner contains the join responsibility and is deliberately not the object to place in formula state or capture in a job. The handle is cloneable and may be shared between worksheet calls.

The initializer and the eventual destructor of `T` run on the worker. Calls receive `&mut T`:

```rust
let value = handle.call(|context| context.fetch_data(query))?;
```

The closure, its result, and its user error must be `Send + 'static`; the native `T` itself need not be.

## Error separation

Call methods return `WorkerCallError<E>`:

```rust
match handle.call(|context| context.fetch_data(query)) {
    Ok(value) => Ok(value),
    Err(WorkerCallError::User(error)) => Err(error),
    Err(WorkerCallError::Infrastructure(error)) => Err(Error::Worker(error)),
}
```

`User(E)` comes from the submitted operation. `Infrastructure(WorkerError)` describes queue, lifecycle, reentry, worker panic, or ownership failure. Keeping them distinct prevents infrastructure code from executing a user-defined error conversion on the worker.

Common infrastructure states include queue full, closed/closing worker, reentrant call, panic, wrong pool, and invalid worker count. Map them to a stable operational error policy rather than string matching.

## Blocking, non-blocking, and async submission

```rust
handle.call(operation);      // wait for capacity and completion
handle.try_call(operation);  // fail immediately when not accepted
handle.call_async(operation).await;
handle.enqueue(cleanup);     // non-blocking fire-and-forget
```

`call_async` is cancellation-aware while a job is queued. Dropping the returned future races with the worker through a `Queued -> Running` or `Queued -> Canceled` state transition. Once the operation is running, dropping the future cannot interrupt arbitrary native code.
Same-worker async reentry is rejected, while an async call from one worker to a
different worker is allowed. Synchronous calls from any worker to another
worker remain rejected because they block the caller and can form a cycle.

`enqueue` reports a full or stopped queue immediately and never waits for
completion. Use it only when worker-owned state performs a final defensive
cleanup during shutdown.

Do not make a synchronous worksheet function wait indefinitely on a saturated worker. Choose between:

- a bounded `try_call` failure;
- an asynchronous UDF that awaits `call_async`;
- a deliberately bounded synchronous operation with measured latency.

## Worker pools

Use a pool when the native supports several independent thread-affine contexts:

```rust
let pool = ThreadBoundPool::spawn(
    PoolOptions::new(4)
        .named("data processing")
        .queue_capacity(64),
    |_| NativeContext::create(),
    NativeContext::shutdown,
)?;
let handle = pool.handle();
```

If a later worker fails to initialize, the pool runs the rollback function on
every context already created and reports initialization, rollback, and worker
shutdown failures separately. The registered finalizer is also used by normal
shutdown and Drop fallback. The pool selects the worker with the fewest pending
calls, using a rotating tie break.

```rust
let value = handle.call(|context| context.fetch_data(query))?;
let (worker, object) = handle.call_with_worker(|context| context.create_object())?;
let value = handle.call_on(worker, move |context| context.use_object(object))?;
```

`PoolWorkerId` binds an opaque native object to the worker and pool that owns it. Passing an ID from another pool returns `WorkerError::WrongPool`.

Async equivalents are available:

```rust
let value = handle.call_async(operation).await?;
let (worker, object) = handle.call_with_worker_async(create).await?;
let value = handle.call_on_async(worker, operation).await?;
```

A pool improves concurrency only when contexts and the loaded DLL can actually execute
concurrently. `SerializedLibrary<A>` keeps all workers behind one canonical-path gate. Select
`ConcurrentLibrary<A>` through `VerifiedLibrary::assume_concurrent` only under a separately
verified native guarantee.

## Shutdown policies

The coordinator-backed pool cancels queued calls, preserves accepted release
commands, runs one finalizer per worker, joins workers, and finally drops its
lifecycle root:

```rust
pool.shutdown()?;
```

`ThreadBoundPool::shutdown` may be called concurrently and every caller
observes the same result. It rejects a blocking call made by one of its own
workers. Dropping the pool only requests shutdown; the coordinator completes
cleanup without blocking the dropping thread.

The lower-level single-worker owner retains explicit canceling and graceful
shutdown policies:

```rust
owner.shutdown()?;
owner.shutdown_graceful()?;
```

Use graceful shutdown only when completing every queued operation is required and bounded. For an XLL unload, cancellation of work that has not started is normally preferable.

Neither method can forcibly stop a running foreign function. Running operations must cooperate with cancellation or have a documented upper bound. The owner must be joined before XLL unload.

## Coordinator-owned pool lifecycle

`ThreadBoundOwner<T>` remains `!Send + !Sync`. A pool never exposes its
collection of these owners: private `LocalPoolRuntime` state is created and
dropped on a coordinator thread. Shared state can own `ThreadBoundPool<T, F>`
directly and calculation paths clone only its handle:

```text
State / EngineInner: ThreadBoundPool<T, F>
calculation paths: ThreadBoundPoolHandle<T>
session: engine handle + opaque ContextIdentity
handle objects: engine handle + ContextIdentity + logical native object ID
```

Use `spawn_with_root` when DLL load and final unload must occur on that
coordinator. The root is created before worker initialization and dropped only
after every context has been finalized and every worker joined.

## Lower-level wrappers

The worker module also supplies narrower assertions and synchronization tools:

### `Serialized<T>`

Use when `T: Send` may move to shared state but only one mutable call may execute at a time. `with` waits; `try_with` reports immediate unavailability. Same-thread reentry is rejected.

### `ContextPool<T>`

Use when independent `T: Send` contexts can be leased on arbitrary caller threads. `with` waits for one context; `try_with` fails immediately. The pool must be non-empty and rejects same-wrapper reentry.

### `ThreadMobile<T>`

An unsafe assertion that a value, including its destruction, may move between threads. It grants `Send` without granting concurrent access.

### `Reentrant<T>`

An unsafe assertion that all shared operations, movement, and destruction are safe under concurrency. It grants both `Send` and `Sync`. This is the strongest contract and should be the rarest choice.

Prefer owners and safe synchronization types. Unsafe wrappers document native guarantees; they do not discover them.
