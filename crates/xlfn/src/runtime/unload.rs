//! Static selection of the add-in's quiescence and physical residency policy.

use crate::addin::{Addin, PhysicallyUnloadableAddin};

mod sealed {
    pub trait Sealed<A: crate::addin::Addin> {}
}

/// The quiescence contract selected by the generated add-in runtime.
///
/// Only framework policies implement this trait. In particular, selecting
/// [`PhysicalUnload`] requires the add-in's unsafe physical-unload contract;
/// callers cannot supply a policy that asserts physical quiescence themselves.
#[doc(hidden)]
pub trait UnloadPolicy<A: Addin>: sealed::Sealed<A> + Send + Sync + 'static {
    const PHYSICAL: bool;

    fn quiesce(
        shared: &mut A::SharedState,
        lifecycle: &mut A::LifecycleState,
    ) -> Result<(), A::Error>;
}

/// Retains the DLL's residency lease after logical close.
#[doc(hidden)]
pub struct LogicalUnload;

impl<A: Addin> sealed::Sealed<A> for LogicalUnload {}

impl<A: Addin> UnloadPolicy<A> for LogicalUnload {
    const PHYSICAL: bool = false;

    fn quiesce(
        shared: &mut A::SharedState,
        lifecycle: &mut A::LifecycleState,
    ) -> Result<(), A::Error> {
        A::quiesce(shared, lifecycle)
    }
}

/// Permits residency release only after the stronger quiescence contract.
#[doc(hidden)]
pub struct PhysicalUnload;

impl<A: PhysicallyUnloadableAddin> sealed::Sealed<A> for PhysicalUnload {}

impl<A: PhysicallyUnloadableAddin> UnloadPolicy<A> for PhysicalUnload {
    const PHYSICAL: bool = true;

    fn quiesce(
        shared: &mut A::SharedState,
        lifecycle: &mut A::LifecycleState,
    ) -> Result<(), A::Error> {
        A::quiesce_for_physical_unload(shared, lifecycle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addin::{OpenContext, OpenResult, Opened};
    use crate::runtime::Runtime;

    struct DistinctQuiesceAddin;

    impl Addin for DistinctQuiesceAddin {
        type RtdSubscription = ();
        type RtdSources = crate::DynamicRtdSources<Self::RtdSubscription>;
        type SharedState = usize;
        type LifecycleState = usize;
        type Error = crate::XllError;
        type Layers = ();
        #[cfg(feature = "async")]
        type AsyncExecutor = crate::NoAsyncExecutor;

        fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
            Ok(Opened::new(0).with_lifecycle(0))
        }

        fn quiesce(shared: &mut usize, lifecycle: &mut usize) -> Result<(), Self::Error> {
            *shared += 1;
            *lifecycle += 1;
            Err(crate::XllError::Closing)
        }
    }

    #[allow(
        unsafe_code,
        reason = "The counter-only probe owns no execution sources"
    )]
    // SAFETY: the test probe consists only of counters and owns no executable
    // source, worker, callback, timer, or external resource.
    unsafe impl PhysicallyUnloadableAddin for DistinctQuiesceAddin {
        fn quiesce_for_physical_unload(
            shared: &mut usize,
            lifecycle: &mut usize,
        ) -> Result<(), Self::Error> {
            *shared += 10;
            *lifecycle += 10;
            Ok(())
        }
    }

    #[test]
    fn runtime_policy_selects_its_quiesce_contract_and_preserves_the_result() {
        let logical = Runtime::<DistinctQuiesceAddin>::new();
        let physical: Runtime<DistinctQuiesceAddin, PhysicalUnload> =
            Runtime::new_with_physical_unload();
        assert!(!logical.physical_unload_enabled());
        assert!(physical.physical_unload_enabled());

        let (mut shared, mut lifecycle) = (0, 0);
        assert!(matches!(
            logical.quiesce_addin(&mut shared, &mut lifecycle),
            Err(crate::XllError::Closing)
        ));
        assert_eq!((shared, lifecycle), (1, 1));

        let (mut shared, mut lifecycle) = (0, 0);
        physical.quiesce_addin(&mut shared, &mut lifecycle).unwrap();
        assert_eq!((shared, lifecycle), (10, 10));
    }
}
