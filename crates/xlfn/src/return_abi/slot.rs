//! Local TLS-slot capabilities. The TLS storage itself owns no Drop value.
//!
//! Only an initialized owner can publish a pointer. Free authority is minted
//! at the unsafe Excel boundary after matching the current thread's slot;
//! destruction must finish before a reusable capability can release it.

use super::{ReturnBlock, ReturnBlockBacking};
use std::any::Any;
use std::cell::{Cell, UnsafeCell};
use std::mem::{ManuallyDrop, MaybeUninit};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;

type DropPanic = Box<dyn Any + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    Vacant,
    Reserved,
    Initialized,
    Published,
    Freeing,
    Poisoned,
}

pub(super) struct ReturnBlockSlot {
    state: Cell<SlotState>,
    block: UnsafeCell<MaybeUninit<ReturnBlock>>,
}

impl ReturnBlockSlot {
    const fn new() -> Self {
        Self {
            state: Cell::new(SlotState::Vacant),
            block: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    #[inline]
    fn pointer(&self) -> NonNull<ReturnBlock> {
        // SAFETY: UnsafeCell::get points to this nonnull inline storage. Only
        // phase owners may initialize/destroy it; this creates no reference.
        unsafe { NonNull::new_unchecked(self.block.get().cast()) }
    }

    #[inline]
    pub(super) fn try_reserve(&self) -> Option<ReservedSlot<'_>> {
        if self.state.get() != SlotState::Vacant {
            return None;
        }
        self.state.set(SlotState::Reserved);
        Some(ReservedSlot { slot: self })
    }

    /// # Safety
    /// `pointer` is the exact live return being freed once by Excel, whose
    /// reads have ended. An address match alone does not grant that authority.
    #[inline]
    pub(super) unsafe fn claim_free(&self, pointer: *mut ReturnBlock) -> Option<FreeingSlot<'_>> {
        if self.pointer().as_ptr() != pointer {
            return None;
        }
        if self.state.get() != SlotState::Published {
            xlfn_kernel::invariant::fail_stop();
        }
        self.state.set(SlotState::Freeing);
        Some(FreeingSlot { slot: self })
    }

    #[cfg(test)]
    pub(super) fn is_poisoned(&self) -> bool {
        self.state.get() == SlotState::Poisoned
    }
}

thread_local! {
    pub(super) static RETURN_BLOCK_SLOT: ReturnBlockSlot = const { ReturnBlockSlot::new() };
}

// A forgotten reservation blocks reuse without running arbitrary TLS
// destruction after a DLL unload. All actual owners below are stack-local.
const _: () = assert!(!std::mem::needs_drop::<ReturnBlockSlot>());

#[must_use]
pub(super) struct ReservedSlot<'slot> {
    slot: &'slot ReturnBlockSlot,
}
impl<'slot> ReservedSlot<'slot> {
    #[inline]
    pub(super) fn initialize(self, block: ReturnBlock) -> InitializedSlot<'slot> {
        let reserved = ManuallyDrop::new(self);
        let pointer = reserved.slot.pointer().as_ptr();
        // SAFETY: this sole reservation owns uninitialized storage; the write
        // is infallible and invokes no user code between the two phase changes.
        unsafe { pointer.write(block) };
        // SAFETY: the block is initialized but not published, and this sole
        // reservation still owns it. Set the backing in place after the move.
        unsafe { (*pointer).backing = ReturnBlockBacking::ThreadLocal };
        reserved.slot.state.set(SlotState::Initialized);
        InitializedSlot {
            slot: reserved.slot,
        }
    }
}
impl Drop for ReservedSlot<'_> {
    fn drop(&mut self) {
        debug_assert_eq!(self.slot.state.get(), SlotState::Reserved);
        self.slot.state.set(SlotState::Vacant);
    }
}

#[must_use]
pub(super) struct InitializedSlot<'slot> {
    slot: &'slot ReturnBlockSlot,
}
impl<'slot> InitializedSlot<'slot> {
    #[inline]
    pub(super) fn publish(self) -> PublishedSlot<'slot> {
        let initialized = ManuallyDrop::new(self);
        initialized.slot.state.set(SlotState::Published);
        PublishedSlot {
            slot: initialized.slot,
        }
    }
}
impl Drop for InitializedSlot<'_> {
    fn drop(&mut self) {
        // No pointer escaped. Keep the slot blocked throughout observer and
        // payload destruction, including reentry and unwinding field drops.
        self.slot.state.set(SlotState::Freeing);
        let freeing = FreeingSlot { slot: self.slot };
        // SAFETY: initialize wrote this exact owning ReturnBlock, no pointer
        // was published, and this Drop consumes its only initialized owner.
        let operation =
            unsafe { super::enter_return_free_operation(self.slot.pointer().cast().as_ptr()) };
        if let Some(operation) = &operation {
            operation.obligation.observe_release_block();
        }
        let result = freeing.destroy();
        drop(operation);
        match result {
            Ok(reusable) => reusable.release(),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }
}

#[must_use]
pub(super) struct PublishedSlot<'slot> {
    slot: &'slot ReturnBlockSlot,
}
impl PublishedSlot<'_> {
    #[inline]
    pub(super) fn into_pointer(self) -> *mut xlfn_sys::XLOPER12 {
        // The TLS slot remains stable until the host's free callback. Dropping
        // or forgetting this local capability never destroys the publication.
        self.slot.pointer().cast().as_ptr()
    }
}
impl Drop for PublishedSlot<'_> {
    fn drop(&mut self) {
        // Publication has crossed the local ownership boundary. Only the
        // host's later free claim may destroy this retained TLS payload.
    }
}

#[must_use]
pub(super) struct FreeingSlot<'slot> {
    slot: &'slot ReturnBlockSlot,
}
impl<'slot> FreeingSlot<'slot> {
    #[inline]
    pub(super) fn destroy(self) -> Result<ReusableSlot<'slot>, DropPanic> {
        let freeing = ManuallyDrop::new(self);
        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: this capability comes from either the exact host-free
            // claim or the unpublished initialized owner. It is consumed once.
            unsafe { std::ptr::drop_in_place(freeing.slot.pointer().as_ptr()) };
        }));
        match result {
            Ok(()) => Ok(ReusableSlot { slot: freeing.slot }),
            Err(payload) => {
                freeing.slot.state.set(SlotState::Poisoned);
                Err(payload)
            }
        }
    }
}
impl Drop for FreeingSlot<'_> {
    fn drop(&mut self) {
        // Abandonment is not proof that destruction completed. Retain the
        // initialized payload and permanently prohibit reuse of this storage.
        self.slot.state.set(SlotState::Poisoned);
    }
}

#[must_use]
pub(super) struct ReusableSlot<'slot> {
    slot: &'slot ReturnBlockSlot,
}
impl ReusableSlot<'_> {
    #[inline]
    pub(super) fn release(self) {
        drop(self);
    }
}
impl Drop for ReusableSlot<'_> {
    fn drop(&mut self) {
        // Only successful in-place destruction can construct this owner.
        debug_assert_eq!(self.slot.state.get(), SlotState::Freeing);
        self.slot.state.set(SlotState::Vacant);
    }
}

/// The free callback arrived on a thread which does not own this TLS slot.
/// Its producer's state must remain untouched, including on destructor panic.
///
/// # Safety
/// The pointer is a live foreign-thread ReturnBlock, freed once after all reads.
pub(super) unsafe fn destroy_foreign(pointer: *mut ReturnBlock) -> Result<(), DropPanic> {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: forwarded from the defensive cross-thread free boundary.
        unsafe { std::ptr::drop_in_place(pointer) };
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::return_abi::{PreparedReturn, ReturnTracker, ownership::ReturnPublicationTxn};
    use std::sync::atomic::Ordering;

    #[test]
    fn local_slot_capabilities_add_no_stored_owner_or_pointer_words() {
        assert!(!std::mem::needs_drop::<ReturnBlockSlot>());
        assert_eq!(std::mem::size_of::<SlotState>(), std::mem::size_of::<u8>());
        let pointer = std::mem::size_of::<&ReturnBlockSlot>();
        assert_eq!(std::mem::size_of::<ReservedSlot<'_>>(), pointer);
        assert_eq!(std::mem::size_of::<InitializedSlot<'_>>(), pointer);
        assert_eq!(std::mem::size_of::<PublishedSlot<'_>>(), pointer);
        assert_eq!(std::mem::size_of::<FreeingSlot<'_>>(), pointer);
        assert_eq!(std::mem::size_of::<ReusableSlot<'_>>(), pointer);
    }

    fn with_block(operation: impl FnOnce(ReturnBlock, &'static ReturnTracker)) {
        crate::return_abi::tests::with_locked_return_test(|| {
            static TRACKER: ReturnTracker = ReturnTracker::new_closed();
            TRACKER.reopen_admission().unwrap();
            let transaction = ReturnPublicationTxn::new(TRACKER.try_enter_producer().unwrap());
            let block = transaction
                .prepare(PreparedReturn::error(&crate::XllError::Panic))
                .into_block();
            operation(block, &TRACKER);
            assert_eq!(TRACKER.outstanding_obligations(), 0);
            TRACKER.close_admission();
            TRACKER.wait_for_quiescence();
        });
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "a forgotten reservation must block reuse"
    )]
    fn miri_reservation_drop_reuses_but_forget_retains_slot() {
        let slot = ReturnBlockSlot::new();
        drop(slot.try_reserve().unwrap());
        let reserved = slot.try_reserve().unwrap();
        std::mem::forget(reserved);
        assert!(slot.try_reserve().is_none());
        assert_eq!(slot.state.get(), SlotState::Reserved);
    }

    #[test]
    fn miri_initialized_abandonment_destroys_before_reusing() {
        with_block(|block, tracker| {
            let slot = ReturnBlockSlot::new();
            let initialized = slot.try_reserve().unwrap().initialize(block);
            assert_eq!(tracker.outstanding_obligations(), 1);
            assert!(slot.try_reserve().is_none());
            drop(initialized);
            assert_eq!(tracker.outstanding_obligations(), 0);
            assert!(slot.try_reserve().is_some());
        });
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "exercise forgotten initialized ownership without losing test cleanup"
    )]
    fn miri_forgotten_initialized_owner_retains_payload_and_slot() {
        with_block(|block, tracker| {
            let slot = ReturnBlockSlot::new();
            let initialized = slot.try_reserve().unwrap().initialize(block);
            std::mem::forget(initialized);
            assert!(slot.try_reserve().is_none());
            assert_eq!(tracker.outstanding_obligations(), 1);
            assert_eq!(slot.state.get(), SlotState::Initialized);
            // Only this trusted child test can recover the deliberately
            // forgotten, unpublished owner for cleanup. No pointer escaped.
            drop(InitializedSlot { slot: &slot });
        });
    }

    #[test]
    fn miri_initialized_destructor_panic_poison_precedes_recovery() {
        with_block(|block, tracker| {
            let slot = ReturnBlockSlot::new();
            let initialized = slot.try_reserve().unwrap().initialize(block);
            crate::return_abi::PANIC_ON_RETURN_BLOCK_DROP.store(true, Ordering::SeqCst);
            let failure =
                crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| drop(initialized)));
            assert!(failure.is_err());
            assert!(slot.is_poisoned());
            assert!(slot.try_reserve().is_none());
            assert_eq!(tracker.outstanding_obligations(), 0);
        });
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "published capability abandonment must retain its allocation"
    )]
    fn miri_published_drop_or_forget_retains_payload_until_free() {
        for forget in [false, true] {
            with_block(|block, tracker| {
                let slot = ReturnBlockSlot::new();
                let published = slot.try_reserve().unwrap().initialize(block).publish();
                if forget {
                    std::mem::forget(published);
                } else {
                    drop(published);
                }
                assert_eq!(slot.state.get(), SlotState::Published);
                assert!(slot.try_reserve().is_none());
                assert_eq!(tracker.outstanding_obligations(), 1);
                // Privileged test-only recovery of the retained publication;
                // this test published no external read and owns the sole free.
                let pointer = slot.pointer().cast().as_ptr();
                // SAFETY: exact initialized publication, no remaining reads.
                let operation =
                    unsafe { crate::return_abi::enter_return_free_operation(pointer) }.unwrap();
                operation.obligation.observe_release_block();
                // SAFETY: the same exact-once free authority.
                let freeing = unsafe { slot.claim_free(pointer.cast()) }.unwrap();
                freeing
                    .destroy()
                    .unwrap_or_else(|_| panic!("fixture destruction succeeds"))
                    .release();
                drop(operation);
                assert_eq!(tracker.outstanding_obligations(), 0);
                assert!(slot.try_reserve().is_some());
            });
        }
    }

    #[test]
    fn miri_foreign_free_leaves_producer_tls_ineligible_for_reuse() {
        struct ForeignReturn(NonNull<xlfn_sys::XLOPER12>);
        // SAFETY: the sender keeps its TLS storage alive until the receiver
        // finishes the exact-once free. Sending transfers all payload access.
        unsafe impl Send for ForeignReturn {}
        impl ForeignReturn {
            fn free(self) {
                // SAFETY: this receiver owns the sole free, and the sender
                // waits for completion before touching its TLS state again.
                drop(unsafe { crate::return_abi::free_return_boundary(self.0.as_ptr()) });
            }
        }
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let (finished, wait) = std::sync::mpsc::sync_channel(1);
        let producer = std::thread::spawn(move || {
            with_block(|block, tracker| {
                let pointer = RETURN_BLOCK_SLOT.with(|slot| {
                    slot.try_reserve()
                        .unwrap()
                        .initialize(block)
                        .publish()
                        .into_pointer()
                });
                send.send(ForeignReturn(NonNull::new(pointer).unwrap()))
                    .unwrap();
                wait.recv().unwrap();
                assert_eq!(tracker.outstanding_obligations(), 0);
                RETURN_BLOCK_SLOT.with(|slot| {
                    assert_eq!(slot.state.get(), SlotState::Published);
                    assert!(slot.try_reserve().is_none());
                });
            });
        });
        receive.recv().unwrap().free();
        finished.send(()).unwrap();
        producer.join().unwrap();
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "forgotten freeing claims must not certify reuse"
    )]
    fn miri_freeing_drop_or_forget_never_certifies_reuse() {
        for forget in [false, true] {
            with_block(|block, tracker| {
                let slot = ReturnBlockSlot::new();
                let pointer = slot
                    .try_reserve()
                    .unwrap()
                    .initialize(block)
                    .publish()
                    .into_pointer();
                // SAFETY: this test acts as the exact-once free callback for its
                // local publication, with all payload reads already finished.
                let operation =
                    unsafe { crate::return_abi::enter_return_free_operation(pointer) }.unwrap();
                // SAFETY: the same local publication and callback authority.
                let freeing = unsafe { slot.claim_free(pointer.cast()) }.unwrap();
                if forget {
                    std::mem::forget(freeing);
                } else {
                    drop(freeing);
                }
                assert_eq!(
                    slot.state.get(),
                    if forget {
                        SlotState::Freeing
                    } else {
                        SlotState::Poisoned
                    }
                );
                assert!(slot.try_reserve().is_none());
                assert_eq!(tracker.outstanding_obligations(), 1);
                operation.obligation.observe_release_block();
                // SAFETY: test-only cleanup of the retained initialized payload;
                // the sole free claim was dropped without destroying it.
                unsafe { std::ptr::drop_in_place(pointer.cast::<ReturnBlock>()) };
                drop(operation);
                assert!(slot.try_reserve().is_none());
            });
        }
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "exercise forgotten post-destruction permission"
    )]
    fn miri_forgotten_reusable_owner_keeps_slot_blocked() {
        with_block(|block, tracker| {
            let slot = ReturnBlockSlot::new();
            let pointer = slot
                .try_reserve()
                .unwrap()
                .initialize(block)
                .publish()
                .into_pointer();
            // SAFETY: exact-once local free with no remaining reads.
            let operation =
                unsafe { crate::return_abi::enter_return_free_operation(pointer) }.unwrap();
            operation.obligation.observe_release_block();
            // SAFETY: same exact callback authority as above.
            let freeing = unsafe { slot.claim_free(pointer.cast()) }.unwrap();
            let reusable = freeing
                .destroy()
                .unwrap_or_else(|_| panic!("fixture destruction succeeds"));
            std::mem::forget(reusable);
            assert!(slot.try_reserve().is_none());
            assert_eq!(slot.state.get(), SlotState::Freeing);
            drop(operation);
            assert_eq!(tracker.outstanding_obligations(), 0);
        });
    }
}
