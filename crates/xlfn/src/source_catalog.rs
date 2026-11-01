//! Concrete source collections selected by an add-in.
use crate::RtdSubscription;
use std::marker::PhantomData;

/// Source storage for one add-in generation.
///
/// Use a tuple of one to sixteen named source types for static dispatch, or
/// [`DynamicRtdSources`] when sources are registered individually. This trait
/// is sealed: only framework-provided catalogs can issue source identities.
pub trait RtdSourceCatalog<E: RtdSubscription>: sealed::Catalog<E> + Send + Sync + 'static {
    /// Typed handles returned when the complete catalog is registered.
    type Handles;
}

/// A catalog for incremental registration, including capturing closures.
///
/// Each source has one boxed adapter. Static tuple catalogs instead retain
/// concrete sources together in one allocation without a source vtable.
pub struct DynamicRtdSources<E: RtdSubscription = ()> {
    #[cfg(any(feature = "rtd", feature = "handles"))]
    pub(crate) sources: Vec<Box<dyn crate::subscription::source::ErasedRtdSource<E>>>,
    marker: PhantomData<fn() -> E>,
}
impl<E: RtdSubscription> AsRef<Self> for DynamicRtdSources<E> {
    fn as_ref(&self) -> &Self {
        self
    }
}
impl<E: RtdSubscription> Default for DynamicRtdSources<E> {
    fn default() -> Self {
        Self {
            #[cfg(any(feature = "rtd", feature = "handles"))]
            sources: Vec::new(),
            marker: PhantomData,
        }
    }
}
impl<E: RtdSubscription> RtdSourceCatalog<E> for DynamicRtdSources<E> {
    type Handles = ();
}
impl<E: RtdSubscription> RtdSourceCatalog<E> for () {
    type Handles = ();
}

/// An index checked against and retained with its exact immutable catalog.
/// Raw catalog dispatch accepts this owner instead of independently supplied
/// receivers and indices. Its fields are private to this trusted boundary.
#[cfg(any(feature = "rtd", feature = "handles"))]
pub(crate) struct CatalogEntry<'a, E: RtdSubscription, C: RtdSourceCatalog<E>> {
    catalog: &'a C,
    index: usize,
    marker: PhantomData<fn() -> E>,
}

#[cfg(any(feature = "rtd", feature = "handles"))]
impl<'a, E: RtdSubscription, C: RtdSourceCatalog<E>> CatalogEntry<'a, E, C> {
    pub(crate) fn acquire(catalog: &'a C, index: usize) -> Option<Self> {
        (index < catalog.len()).then_some(Self {
            catalog,
            index,
            marker: PhantomData,
        })
    }
    pub(crate) fn all(catalog: &'a C) -> impl Iterator<Item = Self> {
        (0..catalog.len()).map(move |index| Self {
            catalog,
            index,
            marker: PhantomData,
        })
    }
    pub(crate) fn subscribe(
        &self,
        topic: &crate::subscription::RtdTopic,
        sink: crate::subscription::PublishSink,
    ) -> crate::XllResult<E> {
        C::subscribe(self, topic, sink)
    }
    pub(crate) fn request_stop(&self) {
        C::request_stop(self);
    }
    pub(crate) fn shutdown_and_wait(&self) -> crate::XllResult<()> {
        C::shutdown_and_wait(self)
    }
}

// The sealing module is private. Its methods cannot be called downstream to
// forge handles or transfer a sink outside the runtime's admission barrier.
#[allow(
    private_interfaces,
    reason = "sealed implementation protocol is crate-internal"
)]
mod sealed {
    use super::*;
    #[cfg(any(feature = "rtd", feature = "handles"))]
    use crate::{
        XllResult,
        generation::RuntimeGeneration,
        subscription::{PublishSink, RtdTopic},
    };

    #[allow(
        private_interfaces,
        reason = "sealed implementation protocol is crate-internal"
    )]
    pub trait Catalog<E: RtdSubscription>: Sized {
        type Stored: AsRef<Self> + Send + Sync + 'static;
        fn store(self) -> Self::Stored;
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn len(&self) -> usize;
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn handles(
            &self,
            generation: RuntimeGeneration,
            registration: u64,
        ) -> <Self as RtdSourceCatalog<E>>::Handles
        where
            Self: RtdSourceCatalog<E>;
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn subscribe(
            entry: &CatalogEntry<'_, E, Self>,
            topic: &RtdTopic,
            sink: PublishSink,
        ) -> XllResult<E>
        where
            Self: RtdSourceCatalog<E>;
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn request_stop(entry: &CatalogEntry<'_, E, Self>)
        where
            Self: RtdSourceCatalog<E>;
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn shutdown_and_wait(entry: &CatalogEntry<'_, E, Self>) -> XllResult<()>
        where
            Self: RtdSourceCatalog<E>;
    }
    impl<E: RtdSubscription> Catalog<E> for () {
        type Stored = xlfn_kernel::published_owner::PublishedOwner<Self>;
        fn store(self) -> Self::Stored {
            xlfn_kernel::published_owner::PublishedOwner::new(self)
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn len(&self) -> usize {
            0
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn handles(&self, _: RuntimeGeneration, _: u64) -> <Self as RtdSourceCatalog<E>>::Handles {}
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn subscribe(_: &CatalogEntry<'_, E, Self>, _: &RtdTopic, _: PublishSink) -> XllResult<E> {
            unreachable!("empty catalog")
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn request_stop(_: &CatalogEntry<'_, E, Self>) {
            unreachable!("empty catalog")
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn shutdown_and_wait(_: &CatalogEntry<'_, E, Self>) -> XllResult<()> {
            unreachable!("empty catalog")
        }
    }
    impl<E: RtdSubscription> Catalog<E> for DynamicRtdSources<E> {
        type Stored = Self;
        fn store(self) -> Self::Stored {
            #[cfg(any(feature = "rtd", feature = "handles"))]
            {
                let mut catalog = self;
                catalog.sources.shrink_to_fit();
                catalog
            }
            #[cfg(not(any(feature = "rtd", feature = "handles")))]
            {
                self
            }
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn len(&self) -> usize {
            self.sources.len()
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn handles(&self, _: RuntimeGeneration, _: u64) -> <Self as RtdSourceCatalog<E>>::Handles {}
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn subscribe(
            entry: &CatalogEntry<'_, E, Self>,
            topic: &RtdTopic,
            sink: PublishSink,
        ) -> XllResult<E> {
            entry.catalog.sources[entry.index].subscribe(topic, sink)
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn request_stop(entry: &CatalogEntry<'_, E, Self>) {
            entry.catalog.sources[entry.index].request_stop();
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn shutdown_and_wait(entry: &CatalogEntry<'_, E, Self>) -> XllResult<()> {
            entry.catalog.sources[entry.index].shutdown_and_wait()
        }
    }
}

#[cfg(any(feature = "rtd", feature = "handles"))]
mod tuples {
    use super::*;
    use crate::{
        IntoRtdSubscription, XllResult,
        generation::RuntimeGeneration,
        subscription::{
            PublishSink, RtdTopic,
            source::{RtdSource, RtdSourceHandle, SourceHandleId, subscribe_typed},
        },
    };
    macro_rules! tuple_catalog {
        ($($S:ident:$index:tt),+) => {
            impl<E, $($S),+> RtdSourceCatalog<E> for ($($S,)+)
            where E: RtdSubscription, $($S: RtdSource, $S::Subscription: IntoRtdSubscription<E>,)+ {
                type Handles = ($(RtdSourceHandle<$S>,)+);
            }
            #[allow(private_interfaces, reason = "sealed implementation protocol is crate-internal")]
            impl<E, $($S),+> sealed::Catalog<E> for ($($S,)+)
            where E: RtdSubscription, $($S: RtdSource, $S::Subscription: IntoRtdSubscription<E>,)+ {
                type Stored = xlfn_kernel::published_owner::PublishedOwner<Self>;
                fn store(self) -> Self::Stored { xlfn_kernel::published_owner::PublishedOwner::new(self) }
                fn len(&self) -> usize { [$(stringify!($S)),+].len() }
                fn handles(&self, generation: RuntimeGeneration, registration: u64) -> <Self as RtdSourceCatalog<E>>::Handles {
                    ($(RtdSourceHandle::from_id(SourceHandleId { generation, registration, sequence: $index + 1 }),)+)
                }
                fn subscribe(entry:&CatalogEntry<'_,E,Self>,topic:&RtdTopic,sink:PublishSink)->XllResult<E> {
                    match entry.index { $($index => subscribe_typed(&entry.catalog.$index, topic, sink),)+ _ => unreachable!("validated catalog index") }
                }
                fn request_stop(entry:&CatalogEntry<'_,E,Self>) {
                    match entry.index { $($index => entry.catalog.$index.request_stop(),)+ _ => unreachable!("validated catalog index") }
                }
                fn shutdown_and_wait(entry:&CatalogEntry<'_,E,Self>)->XllResult<()> {
                    match entry.index { $($index => entry.catalog.$index.shutdown_and_wait(),)+ _ => unreachable!("validated catalog index") }
                }
            }
        }
    }
    tuple_catalog!(S0:0);
    tuple_catalog!(S0:0, S1:1);
    tuple_catalog!(S0:0, S1:1, S2:2);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4, S5:5);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4, S5:5, S6:6);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4, S5:5, S6:6, S7:7);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4, S5:5, S6:6, S7:7, S8:8);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4, S5:5, S6:6, S7:7, S8:8, S9:9);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4, S5:5, S6:6, S7:7, S8:8, S9:9, S10:10);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4, S5:5, S6:6, S7:7, S8:8, S9:9, S10:10, S11:11);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4, S5:5, S6:6, S7:7, S8:8, S9:9, S10:10, S11:11, S12:12);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4, S5:5, S6:6, S7:7, S8:8, S9:9, S10:10, S11:11, S12:12, S13:13);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4, S5:5, S6:6, S7:7, S8:8, S9:9, S10:10, S11:11, S12:12, S13:13, S14:14);
    tuple_catalog!(S0:0, S1:1, S2:2, S3:3, S4:4, S5:5, S6:6, S7:7, S8:8, S9:9, S10:10, S11:11, S12:12, S13:13, S14:14, S15:15);
}

#[cfg(all(test, feature = "rtd"))]
mod tests {
    #![allow(unsafe_code, reason = "test sources never retain a supplied sink")]
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountedSource(Arc<AtomicUsize>);
    // SAFETY: no sink is retained and no worker or callback escapes.
    unsafe impl crate::subscription::RtdSource for CountedSource {
        type Value = i32;
        type Subscription = ();
        fn subscribe(
            &self,
            _: &crate::subscription::RtdTopic,
            _: crate::subscription::RtdSink<i32>,
        ) -> crate::XllResult<()> {
            Ok(())
        }
        fn request_stop(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn shutdown_and_wait(&self) -> crate::XllResult<()> {
            self.0.fetch_add(10, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn miri_catalog_entry_retains_its_issuer_and_rejects_invalid_indices() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let a = (CountedSource(Arc::clone(&first)),);
        let b = (CountedSource(Arc::clone(&second)),);
        assert!(CatalogEntry::<(), _>::acquire(&a, 1).is_none());
        assert!(CatalogEntry::<(), _>::acquire(&a, usize::MAX).is_none());
        assert!(CatalogEntry::<(), _>::acquire(&(), 0).is_none());
        assert_eq!(CatalogEntry::<(), _>::all(&()).count(), 0);
        let entry = CatalogEntry::<(), _>::acquire(&a, 0).unwrap();
        entry.request_stop();
        entry.shutdown_and_wait().unwrap();
        assert_eq!(first.load(Ordering::Relaxed), 11);
        assert_eq!(second.load(Ordering::Relaxed), 0);
        for entry in CatalogEntry::<(), _>::all(&b) {
            entry.request_stop();
            entry.shutdown_and_wait().unwrap();
        }
        assert_eq!(second.load(Ordering::Relaxed), 11);
    }
}
