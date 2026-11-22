use crate::generation::RuntimeGeneration;
use crate::handle::TaskPins;
use std::future::Future;

/// Narrow lifetime brand for the generated async handle path.
///
/// The scope borrows the raw-pin owner from the enclosing executor task, not
/// from the runtime. The generated future retains the brand's lifetime until
/// it is destroyed inside the same task.
#[cfg(feature = "handles")]
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct AsyncTaskScope<'generation> {
    generation: RuntimeGeneration,
    pins: &'generation TaskPins,
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
    ) -> impl Future<Output = crate::XllResult<T>> + Send + 'generation;
}

#[cfg(feature = "handles")]
// The enclosing executor task owns the brand, generated future, and delivery.
// Each future stays concrete and keeps its original scope lifetime throughout
// delivery; only the enclosing, self-contained task must be 'static.
pub(crate) trait HandleScopedTaskBuilder {
    type Output: Send + 'static;
    type Delivery: Send + 'static;

    fn build_task<'generation>(
        self,
        scope: AsyncTaskScope<'generation>,
    ) -> (
        impl Future<Output = Self::Output> + Send + 'generation,
        Self::Delivery,
    );

    fn deliver<'generation>(
        delivery: Self::Delivery,
        future: impl Future<Output = Self::Output> + Send + 'generation,
    ) -> impl Future<Output = ()> + Send + 'generation;
}

#[cfg(feature = "handles")]
impl<'generation> AsyncTaskScope<'generation> {
    pub(crate) fn new(generation: RuntimeGeneration, pins: &'generation TaskPins) -> Self {
        Self { generation, pins }
    }

    pub(crate) const fn pins(self) -> &'generation TaskPins {
        self.pins
    }

    pub(crate) const fn generation(self) -> RuntimeGeneration {
        self.generation
    }
}

/// Wraps an evaluation at its single ownership boundary. Keeps the concrete
/// future type hidden before returning it to the generic task runner.
///
/// # Safety
/// The returned future and its captures must be the only remaining users of
/// the scope, apart from the pin owner's own cleanup guard.
#[cfg(feature = "handles")]
#[allow(
    unsafe_code,
    reason = "The owning launch invokes this only once per evaluation"
)]
pub(super) unsafe fn with_pin_cleanup<'generation, F: Future + Send + 'generation>(
    scope: AsyncTaskScope<'generation>,
    future: F,
) -> impl Future<Output = F::Output> + Send + 'generation
where
    F::Output: 'static,
{
    ScopedEvaluation {
        future,
        _end: ScopeEnd(scope.pins()),
    }
}

#[cfg(feature = "handles")]
struct ScopedEvaluation<'generation, F> {
    // Destruction order matters: the contained future may use its leases
    // during Drop. Drop glue runs _end after future, including on unwind.
    future: F,
    _end: ScopeEnd<'generation>,
}

#[cfg(feature = "handles")]
struct ScopeEnd<'generation>(&'generation TaskPins);

#[cfg(feature = "handles")]
impl Drop for ScopeEnd<'_> {
    #[allow(
        unsafe_code,
        reason = "The evaluator ends before the pin cleanup guard"
    )]
    fn drop(&mut self) {
        // SAFETY: with_pin_cleanup requires the wrapped future to be the
        // only remaining user of the scope. This guard is dropped after it.
        unsafe { self.0.release_after_evaluation() };
    }
}

#[cfg(feature = "handles")]
impl<F: Future> Future for ScopedEvaluation<'_, F> {
    type Output = F::Output;

    #[allow(unsafe_code, reason = "Structural pin projection of the future field")]
    fn poll(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // SAFETY: no code replaces or moves future after pinning. The
        // wrapper has no custom Drop and drops the future in place.
        unsafe { self.map_unchecked_mut(|this| &mut this.future) }.poll(context)
    }
}
