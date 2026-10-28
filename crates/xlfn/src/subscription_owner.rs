//! Owned subscription protocols shared by the public add-in contract and RTD.

#![allow(
    unsafe_code,
    reason = "subscription barriers and ownership-preserving conversions carry explicit unsafe contracts"
)]

use crate::XllResult;

/// A subscription whose cancellation and disconnection protocol is explicit.
///
/// The subscription value has one owner: its server, or the generation's
/// bounded cleanup queue after COM disconnection revokes the connection.
/// Deferred COM cleanup and shutdown invoke `request_cancel`, then consume
/// the value through `disconnect_and_wait`. Shutdown drains this work before
/// reclaiming its server or source. No detached shared cancellation object
/// participates in the ownership graph. The framework may move this value;
/// it provides no pinning guarantee for the subscription itself.
///
/// # Safety
///
/// `disconnect_and_wait` must stop every callback and worker that can use any
/// RTD sink issued to this subscription before control leaves the method,
/// whether it returns `Ok`, returns `Err`, or unwinds. No sink clone may be
/// used after any of those exits. Preserve this guarantee during unwinding,
/// for example with a cleanup guard that joins every sink user. This barrier
/// also applies when no prior `request_cancel` call was made.
///
/// Cancellation and disconnection must not synchronously initiate this
/// add-in's own removal: removal waits for this subscription's cleanup.
/// These methods may run on different framework-owned threads, including a
/// Windows MTA cleanup worker. They must not rely on the subscribing thread's
/// affinity or pass an apartment-bound COM interface without marshaling.
///
/// The framework contains cleanup panics and may reclaim the publish core
/// afterward. Neither an error nor a panic extends the lifetime of a sink.
pub unsafe trait RtdSubscription: Sized + Send + 'static {
    /// Requests that subscription work stop without transferring ownership.
    fn request_cancel(&self);

    /// Consumes the subscription and stops all sink users before returning.
    fn disconnect_and_wait(self) -> XllResult<()>;
}

// SAFETY: the empty owner never receives a sink or starts work.
unsafe impl RtdSubscription for () {
    fn request_cancel(&self) {}

    fn disconnect_and_wait(self) -> XllResult<()> {
        Ok(())
    }
}

/// Transfers a concrete subscription into an add-in's common subscription owner.
///
/// A single subscription type already implements this conversion to itself.
/// Use [`crate::rtd_subscriptions!`] to declare a closed enum of several types
/// and generate their ownership-preserving conversions and barrier delegation.
/// Ordinary `From` or `Into` implementations do not establish this contract.
///
/// # Safety
///
/// On return, the resulting `E` must own the cancellation and disconnection
/// obligations for every sink user owned by `self`. No such obligation may
/// escape into another owner, background task, global, or forgotten value.
/// If conversion unwinds, all of those sink users must have stopped before
/// control leaves the conversion. Merely dropping `self` is insufficient
/// unless its destructor establishes that same barrier.
pub unsafe trait IntoRtdSubscription<E: RtdSubscription>: RtdSubscription {
    /// Moves every subscription obligation into the returned common owner.
    fn into_rtd_subscription(self) -> E;
}

// SAFETY: identity moves the exact owner without invoking any user code.
unsafe impl<T: RtdSubscription> IntoRtdSubscription<T> for T {
    fn into_rtd_subscription(self) -> T {
        self
    }
}

/// Declares the closed set of subscription owners used by an add-in.
///
/// Each variant contains exactly one concrete `RtdSubscription`. The generated
/// implementation delegates cancellation and the consuming barrier to that
/// value, and converts each concrete type by directly constructing its variant.
/// Every concrete type must occur only once. Gate the macro invocation itself
/// when declaring a feature-dependent subscription set.
///
/// ```
/// xlfn::rtd_subscriptions! {
///     /// Subscription owners accepted by this add-in.
///     pub enum Subscriptions {
///         Empty(()),
///     }
/// }
/// ```
#[macro_export]
macro_rules! rtd_subscriptions {
    (
        $(#[$attribute:meta])*
        $visibility:vis enum $name:ident {
            $($variant:ident($subscription:ty)),+ $(,)?
        }
    ) => {
        $(#[$attribute])*
        $visibility enum $name {
            $(
                #[doc = concat!("Owns a `", stringify!($subscription), "` subscription.")]
                $variant($subscription),
            )+
        }

        // SAFETY: every operation delegates to the exact uniquely owned variant.
        #[allow(unsafe_code, reason = "generated subscription enum delegates each barrier to its concrete owner")]
        unsafe impl $crate::RtdSubscription for $name {
            fn request_cancel(&self) {
                match self {
                    $(Self::$variant(value) => <$subscription as $crate::RtdSubscription>::request_cancel(value),)+
                }
            }

            fn disconnect_and_wait(self) -> $crate::XllResult<()> {
                match self {
                    $(Self::$variant(value) => <$subscription as $crate::RtdSubscription>::disconnect_and_wait(value),)+
                }
            }
        }

        $(
            // SAFETY: constructing the variant only moves its owner; it cannot
            // panic, retain an independent obligation, or invoke application code.
            #[allow(unsafe_code, reason = "generated conversion only moves a subscription into its enum variant")]
            unsafe impl $crate::IntoRtdSubscription<$name> for $subscription {
                fn into_rtd_subscription(self) -> $name {
                    $name::$variant(self)
                }
            }
        )+
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panic_boundary::catch_no_unwind;
    use std::cell::Cell;
    use std::panic::AssertUnwindSafe;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Counts {
        cancelled: AtomicUsize,
        disconnected: AtomicUsize,
        dropped: AtomicUsize,
    }

    struct Probe {
        counts: Arc<Counts>,
        local_cancellations: Cell<usize>,
        cancel_panics: bool,
        disconnect_panics: bool,
    }

    impl Probe {
        fn new(counts: &Arc<Counts>) -> Self {
            Self {
                counts: Arc::clone(counts),
                local_cancellations: Cell::new(0),
                cancel_panics: false,
                disconnect_panics: false,
            }
        }
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            self.counts.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: the probe never receives a sink and starts no worker.
    unsafe impl RtdSubscription for Probe {
        fn request_cancel(&self) {
            self.local_cancellations
                .set(self.local_cancellations.get() + 1);
            self.counts.cancelled.fetch_add(1, Ordering::Relaxed);
            assert!(
                !self.cancel_panics,
                "injected common-owner cancellation panic"
            );
        }

        fn disconnect_and_wait(self) -> XllResult<()> {
            self.counts.disconnected.fetch_add(1, Ordering::Relaxed);
            assert!(
                !self.disconnect_panics,
                "injected common-owner disconnect panic"
            );
            Ok(())
        }
    }

    crate::rtd_subscriptions! {
        enum Subscriptions {
            Tracked(Probe),
            Empty(()),
        }
    }

    #[test]
    fn miri_subscription_enum_moves_non_sync_owner_and_delegates_barriers() {
        static_assertions::assert_impl_all!(Subscriptions: Send);
        static_assertions::assert_not_impl_any!(Subscriptions: Sync, Clone, Copy, From<Probe>);
        let counts = Arc::new(Counts::default());
        let subscription: Subscriptions = Probe::new(&counts).into_rtd_subscription();
        subscription.request_cancel();
        let Subscriptions::Tracked(probe) = &subscription else {
            panic!("conversion selected the wrong subscription variant");
        };
        assert_eq!(probe.local_cancellations.get(), 1);
        subscription.disconnect_and_wait().unwrap();
        assert_eq!(counts.cancelled.load(Ordering::Relaxed), 1);
        assert_eq!(counts.disconnected.load(Ordering::Relaxed), 1);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 1);

        let empty: Subscriptions = ().into_rtd_subscription();
        assert!(matches!(empty, Subscriptions::Empty(())));
        empty.request_cancel();
        empty.disconnect_and_wait().unwrap();
    }

    #[test]
    fn miri_subscription_enum_preserves_owner_on_cancel_panic() {
        let counts = Arc::new(Counts::default());
        let mut probe = Probe::new(&counts);
        probe.cancel_panics = true;
        let subscription: Subscriptions = probe.into_rtd_subscription();
        assert!(catch_no_unwind(AssertUnwindSafe(|| subscription.request_cancel())).is_err());
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 0);
        subscription.disconnect_and_wait().unwrap();
        assert_eq!(counts.cancelled.load(Ordering::Relaxed), 1);
        assert_eq!(counts.disconnected.load(Ordering::Relaxed), 1);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miri_subscription_enum_consuming_panic_drops_concrete_owner_once() {
        let counts = Arc::new(Counts::default());
        let mut probe = Probe::new(&counts);
        probe.disconnect_panics = true;
        let subscription: Subscriptions = probe.into_rtd_subscription();
        assert!(catch_no_unwind(AssertUnwindSafe(|| subscription.disconnect_and_wait())).is_err());
        assert_eq!(counts.disconnected.load(Ordering::Relaxed), 1);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn identity_conversion_keeps_concrete_subscription_type() {
        let counts = Arc::new(Counts::default());
        let subscription: Probe = Probe::new(&counts).into_rtd_subscription();
        subscription.disconnect_and_wait().unwrap();
        assert_eq!(counts.disconnected.load(Ordering::Relaxed), 1);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 1);
    }
}
