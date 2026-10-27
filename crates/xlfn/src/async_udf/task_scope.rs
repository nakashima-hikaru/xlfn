use crate::generation::RuntimeGeneration;
use crate::handle::GenerationLeaseBrand;
use std::future::Future;
use std::marker::PhantomData;

/// Narrow lifetime brand for the generated async handle path.
///
/// The scope carries no runtime borrow. Its lifetime is a compile-time token
/// borrowed from a brand owned by the enclosing executor task. The generated
/// future keeps that lifetime until it is destroyed inside the same task.
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
