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
        fn subscribe(&self, index: usize, topic: &RtdTopic, sink: PublishSink) -> XllResult<E>;
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn request_stop(&self, index: usize);
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn shutdown_and_wait(&self, index: usize) -> XllResult<()>;
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
        fn subscribe(&self, _: usize, _: &RtdTopic, _: PublishSink) -> XllResult<E> {
            unreachable!("empty catalog")
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn request_stop(&self, _: usize) {
            unreachable!("empty catalog")
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn shutdown_and_wait(&self, _: usize) -> XllResult<()> {
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
        fn subscribe(&self, index: usize, topic: &RtdTopic, sink: PublishSink) -> XllResult<E> {
            self.sources[index].subscribe(topic, sink)
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn request_stop(&self, index: usize) {
            self.sources[index].request_stop();
        }
        #[cfg(any(feature = "rtd", feature = "handles"))]
        fn shutdown_and_wait(&self, index: usize) -> XllResult<()> {
            self.sources[index].shutdown_and_wait()
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
                fn subscribe(&self,index:usize,topic:&RtdTopic,sink:PublishSink)->XllResult<E> {
                    match index { $($index => subscribe_typed(&self.$index, topic, sink),)+ _ => unreachable!("validated catalog index") }
                }
                fn request_stop(&self,index:usize) {
                    match index { $($index => self.$index.request_stop(),)+ _ => unreachable!("validated catalog index") }
                }
                fn shutdown_and_wait(&self,index:usize)->XllResult<()> {
                    match index { $($index => self.$index.shutdown_and_wait(),)+ _ => unreachable!("validated catalog index") }
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
