# Thread-affine application adapters

Some external implementations require a context and every derived object to be created, used, and destroyed on one OS thread. xlfn does not provide a worker, executor, or thread-affinity abstraction for this case. The add-in application must implement or select one that matches the external contract.

## Owner and client split

A common design separates:

- an **owner**, which creates the thread-affine state, runs its event loop, performs final destruction, and joins the worker;
- a cloneable **client**, which contains only thread-safe submission state and may be stored in the add-in `State` or in formula-owned objects.

An application-owned shape may look like this:

```rust,ignore
use std::sync::mpsc::SyncSender;
use std::thread::JoinHandle;

pub struct EngineClient {
    tx: SyncSender<Command>,
}

pub struct EngineRuntime {
    client: EngineClient,
    worker: Option<JoinHandle<()>>,
}

enum Command {
    Price(PriceRequest),
    Release(ObjectId),
    Shutdown,
}
```

These are illustrative application types. xlfn does not export `EngineClient`, `EngineRuntime`, `Command`, or an equivalent worker API.

The worker should create and destroy the thread-affine context on the same thread. A worksheet call sends owned data to the client and receives an owned result; raw pointers and borrowed Excel values must not cross that queue.

## Define dispatch semantics explicitly

The adapter contract should state:

- whether submission blocks, fails immediately, or returns a future;
- queue capacity and overload behavior;
- whether queued work can be cancelled before it starts;
- whether running work can observe cancellation;
- reentry and callback rules;
- which thread destroys each object;
- how worker panic or process failure is reported;
- whether shutdown drains or rejects queued work.

Keep application/domain failures distinct from adapter-infrastructure failures. This allows stable mapping to worksheet errors without parsing diagnostic strings.

## Synchronous and asynchronous worksheet calls

A synchronous UDF must not wait without a documented upper bound. For a saturated adapter, choose an explicit policy such as immediate overload, bounded waiting, or an asynchronous UDF.

For an async UDF, the application adapter may expose a future or bridge a response channel into one. Dropping the Excel-side future does not automatically interrupt a running external call. Cancellation remains an application-adapter property; xlfn only coordinates cancellation and result delivery at the Excel boundary.

## Concurrency

Creating several workers improves throughput only when the external implementation permits independent concurrent contexts. The application must decide whether calls are:

- globally serialized;
- serialized per context or object;
- independently concurrent;
- externally rate-limited.

Do not infer concurrency from the existence of multiple Excel calculation threads. A `thread_safe` UDF allows Excel to call the Rust boundary concurrently; it does not make the downstream implementation thread-safe.

## Shutdown

`Addin::quiesce` must establish application-level quiescence before returning:

1. reject new adapter submissions;
2. signal cancellation or shutdown;
3. resolve or reject queued requests according to the documented policy;
4. release external objects on their required owner;
5. join every worker or coordinator thread;
6. release any library, process, connection, or other adapter root.

A timeout that abandons in-process code is not a safe unload strategy. If a running operation cannot be bounded or cooperatively stopped, isolate it behind a process boundary whose failure cannot leave code executing in the XLL after unload.

The framework does not verify these properties. They are part of the application's adapter design, tests, and release qualification.
