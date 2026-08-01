# Native objects as handles

A common add-in API creates an opaque native object in one formula and consumes it in other functions. Combine formula-owned `Handle<T>` values with worker-owned native storage; do not put a raw native pointer directly in the formula-owned Rust object unless its thread and destruction contract genuinely permit arbitrary-thread drop.

## Recommended representation

For a pool of thread-affine contexts, store native objects inside the owning worker and expose a logical identifier:

```rust
#[derive(Clone, Copy)]
struct NativeObjectId(u64);

struct NativeContext {
    next_id: u64,
    objects: HashMap<u64, NonNull<native_object>>,
    api: ConcurrentLibrary<NativeApi>,
    raw: NonNull<native_context>,
}

#[derive(ExcelHandleObject)]
pub struct Dataset {
    inner: Arc<NativeResource<DatasetSpec>>,
}

trait ResourceSpec: Send + Sync + 'static {
    type Id: Copy + Send + Sync + 'static;
    fn destroy(context: &mut NativeContext, id: Self::Id) -> Result<(), Error>;
}

struct NativeResource<S: ResourceSpec> {
    id: S::Id,
    context: ContextIdentity,
    engine: Arc<EngineInner>,
    release: Arc<ReleaseState>,
    marker: PhantomData<fn() -> S>,
}
```

The formula-owned object is `Send + Sync` because it contains only an immutable
logical ID, an opaque context identity, shared engine lifecycle state, and synchronized
release state. The raw pointer remains in the worker-local map. Using a typed
resource specification binds each ID kind to its destructor.

## Creation

Create independent top-level resources directly through the engine. Worker
selection and native creation are one executor operation:

```rust
#[excel_function(name = "DATASET.CREATE")]
fn dataset_create(
    #[excel_context(main_thread)] context: MainThreadContext<'_, State>,
    rate: f64,
) -> Result<Dataset, Error> {
    context.state().engine.create_dataset(rate)
}
```

Create a context-pinned session when resources may reference one another, then
create all of them through that session:

```rust
fn create_session(&self) -> Result<EngineSession, Error> {
    let _lease = self.inner.admission.enter()?;
    let (worker, ()) = self.inner.workers.call_with_worker(|_| Ok(()))?;
    Ok(EngineSession {
        engine: Arc::clone(&self.inner),
        context: ContextIdentity::new(worker),
    })
}

fn create_dataset(&self, rate: f64) -> Result<Dataset, Error> {
    let id = self.engine.workers.call_on(self.context.worker(), move |context| {
        context.create_dataset(rate, Vec::new())
    })?;
    Ok(Dataset {
        inner: Arc::new(NativeResource {
            id,
            context: self.context,
            engine: Arc::clone(&self.engine),
            release: Arc::new(ReleaseState::new()),
            marker: PhantomData,
        }),
    })
}

```

The worksheet producer is main-thread because it returns a new handle object. The native creation itself may execute on the owner worker.

## Use from an async UDF

```rust
#[excel_function(name = "DATASET.COMPUTE")]
async fn dataset_fetch_data(
    #[excel_context(asynchronous)] context: AsyncContext<State>,
    dataset: Handle<Dataset>,
    factor: f64,
    beta: f64,
) -> Result<f64, Error> {
    let engine = &context.state().engine;
    dataset.inner.validate_engine(engine)?;
    let _lease = engine.inner.admission.enter()?;
    dataset.inner.release.ensure_release_not_requested()?;
    let id = dataset.inner.id;
    match engine
        .inner
        .workers
        .call_on_async(dataset.inner.context.worker(), move |native| {
            native.fetch_data(id, alpha, beta)
        })
        .await
    {
        Ok(value) => Ok(value),
        Err(WorkerCallError::User(error)) => Err(error),
        Err(WorkerCallError::Infrastructure(error)) => Err(Error::Worker(error)),
    }
}
```

Check same engine, same context, and open state before dispatch. Do not expose
`PoolWorkerId` as domain identity: it is an executor detail, while
`ContextIdentity` states the native pointer-compatibility contract.

## Destruction

The formula-owned wrapper should enqueue destruction on the same worker:

```rust
impl Dataset {
    fn close(&self) -> Result<(), Error> {
        self.inner.close()
    }
}

impl<S: ResourceSpec> NativeResource<S> {
    fn close(&self) -> Result<(), Error> {
        let _lease = self.engine.admission.enter()?;
        if !self.release.begin_or_wait()? {
            return Ok(());
        }
        let id = self.id;
        let result = self.engine.workers.call_on(self.context.worker(), move |context| {
            S::destroy(context, id)
        });
        self.release.complete_from(&result);
        result?
    }
}

impl<S: ResourceSpec> Drop for NativeResource<S> {
    fn drop(&mut self) {
        let Ok(_lease) = self.engine.admission.enter() else {
            return;
        };
        if !self.release.claim_for_drop() {
            return;
        }
        let id = self.id;
        let release = self.release.clone();
        if let Err(error) = self.engine.workers.enqueue_release_on(
            self.context.worker(),
            move |context| release.complete(S::destroy(context, id)),
        ) {
            self.release.complete_indeterminate(error);
        }
    }
}
```

`ReleaseState` uses `Mutex<ReleasePhase>` plus `Condvar`, where the phases are
`Open`, `Releasing`, `Released`, `Failed`, and `Indeterminate`. A second
`close` waits while the first is `Releasing` and then returns the same outcome.
An atomic boolean is insufficient because it cannot distinguish “release was
requested” from “release completed successfully.”

`enqueue_release_on` is non-blocking, bypasses normal operation capacity, and
shares FIFO order with accepted operations. It can still fail after worker
shutdown or panic, so that failure is recorded as indeterminate. The design
also requires all of the following:

- the owner remains alive until all formula-owned objects have been released;
- a failed native destructor leaves the object registered for final cleanup;
- the worker-side context performs fallible finalization before `Drop`, then
  destroys any residual objects in `Drop` as a best-effort fallback;
- explicit close and queued cleanup cannot destroy the same object twice.

Worker-side destruction must use `lookup → native destroy → remove`; removing
the pointer before the native call can leak it if call admission fails.
Destructors cannot return errors to the worksheet, so important cleanup
failures must be diagnosed.

## Dependencies between native resources

When one native object retains or refers to another, register that edge at
creation. Each registry entry stores dependency keys, dependent count, and a
monotonic creation sequence. Explicit close returns `ResourceInUse` while any
dependent remains. Shutdown repeatedly destroys zero-dependent entries; among
equally eligible entries it selects the newest creation first. Because a new
entry can depend only on already-open entries in the same session, the graph
remains acyclic and this is a deterministic reverse-topological order.
The dependency list is a set. Repeating the same resource key is rejected
before native creation, so each edge increments the parent's dependent count
exactly once.

Keeping only an `Arc` to a parent Rust handle is insufficient: explicit close
affects the shared native object even while Rust clones remain. The dependency
edge must therefore live in the worker registry that owns native destruction.

## Shutdown ordering

The framework closes handle topics before application state is finally released, but application architecture should not rely on an undocumented incidental drop order. Make the shutdown protocol explicit:

1. stop new worksheet/application submissions;
2. let framework-managed formula handles release or mark the engine closing;
3. flush logical object-release jobs;
4. destroy remaining native objects defensively on their owner workers;
5. shut down and join every owner;
6. release the loaded DLL.

A worker context's destructor should regard its object map as the final safety net and destroy each remaining object exactly once.

## Re-evaluation and replacement

A handle producer runs on every Excel evaluation. For the same formula identity, xlfn replaces the Rust `Dataset` behind the stable token after successful observation. The old `Dataset` is then dropped and should release its previous native object.

This means a producer such as `DATASET.CREATE(value)` may create and destroy native objects on each recalculation. When that cost is undesirable, design inputs and workbook volatility carefully, or add an application cache keyed by immutable dataset content. Do not depend on the handle system to skip the producer body.

## Avoid raw-pointer handle objects

This is usually unsound or operationally fragile:

```rust,ignore
#[derive(ExcelHandleObject)]
struct BadDataset {
    raw: NonNull<native_dataset>,
}
```

The derive requires `Send + Sync`, and the object may be dropped on a lifecycle or formula-management thread rather than the native creation thread. Adding unsafe `Send`/`Sync` impls merely to satisfy the derive hides the native contract instead of enforcing it.

Use a logical ID and owner-worker submission handle unless the native explicitly guarantees arbitrary-thread movement, shared access, and destruction.
