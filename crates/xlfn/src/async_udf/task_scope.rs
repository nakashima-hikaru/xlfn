use crate::generation::RuntimeGeneration;
use crate::handle::GenerationLeaseBrand;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;

/// Narrow lifetime brand for the generated async handle path.
///
/// The scope carries no runtime borrow. Its lifetime is a compile-time token
/// that the runtime validates at the one point where the scoped task future
/// is erased to the runtime's opaque `'static` task type.
#[cfg(feature = "handles")]
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct AsyncTaskScope<'generation> {
    generation: RuntimeGeneration,
    _brand: PhantomData<&'generation GenerationLeaseBrand>,
}

/// Generated-user-code builder for the narrow async handle path.
///
/// A trait method, rather than a general borrowed-future callback, gives the
/// generated implementation an explicit late-bound lifetime for each task
/// scope while keeping the rest of the executor `'static`-task based.
#[cfg(feature = "handles")]
#[doc(hidden)]
pub trait HandleScopedBuilder<T> {
    fn build<'generation>(
        self,
        scope: AsyncTaskScope<'generation>,
    ) -> Pin<Box<dyn Future<Output = crate::XllResult<T>> + Send + 'generation>>;
}

#[cfg(feature = "handles")]
// Only the generated inner future carries the scope brand. Delivery state
// remains owned and 'static, so its concrete future can live directly in the
// task allocation after the inner future's single lifetime erasure.
pub(crate) trait HandleScopedTaskBuilder {
    type Output: Send + 'static;
    type Delivery: Send + 'static;

    fn build_task<'generation>(
        self,
        scope: AsyncTaskScope<'generation>,
    ) -> (ScopedTaskFuture<'generation, Self::Output>, Self::Delivery);

    fn deliver(
        delivery: Self::Delivery,
        future: ScopedTaskFuture<'static, Self::Output>,
    ) -> impl Future<Output = ()> + Send + 'static;
}

#[cfg(feature = "handles")]
impl<'generation> AsyncTaskScope<'generation> {
    pub(crate) fn new(generation: RuntimeGeneration, _: &'generation GenerationLeaseBrand) -> Self {
        Self {
            generation,
            _brand: PhantomData,
        }
    }

    pub(crate) const fn generation(self) -> RuntimeGeneration {
        self.generation
    }
}

#[cfg(feature = "handles")]
pub(crate) type ScopedTaskFuture<'generation, Output = ()> =
    Pin<Box<dyn Future<Output = Output> + Send + 'generation>>;

/// Erases the compile-time handle-generation brand after the generated
/// future has been constrained to contain only task-owned data.
///
/// # Safety
///
/// The caller must ensure that the future contains no borrow from the caller's
/// stack whose validity depends on the scope brand. Generated async contexts
/// may borrow the `ExecutionLease` and cancellation token moved into the same
/// future; those references are therefore task-self-contained. The only
/// external lifetime marker is the zero-sized `AsyncTaskScope`/`HandleLease`
/// brand, while the object lifetime is protected independently by
/// `RawObjectLeaseGuard`. The opaque `AsyncTask` owns a registry completion guard and calculation
/// pin. Registry `active_tasks` retains all framework state until future
/// destruction; `AsyncStopped` is issued only after that count reaches zero.
/// Generation services cannot be sealed before that certificate, so this proof
/// applies equally to builtin and external executors without poller assumptions.
#[cfg(feature = "handles")]
pub(crate) unsafe fn erase_scoped_task_future<'generation, Output: Send + 'static>(
    future: ScopedTaskFuture<'generation, Output>,
) -> ScopedTaskFuture<'static, Output> {
    // SAFETY: upheld by `HandleScopedBuilder`'s late-bound method and the
    // `'static` builder bound, plus the shutdown ordering documented above.
    unsafe {
        std::mem::transmute::<
            ScopedTaskFuture<'generation, Output>,
            ScopedTaskFuture<'static, Output>,
        >(future)
    }
}
