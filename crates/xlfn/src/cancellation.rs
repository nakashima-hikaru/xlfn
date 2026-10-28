use crate::panic_boundary::catch_no_unwind;
use crate::sync::Mutex;
use crossbeam_utils::CachePadded;
use rustc_hash::FxHashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::task::{Context, Poll};
use xlfn_kernel::drain_gate::{DEFAULT_STRIPE_COUNT, current_thread_stripe};
use xlfn_kernel::published_owner::PublishedOwner;

#[cfg(feature = "bench-internals")]
pub(crate) mod benchmark;

const STATE_RUNNING: u8 = 0;
const STATE_CANCELED: u8 = 1;
const STATE_DELIVERING: u8 = 2;
#[cfg(feature = "async")]
const STATE_DONE: u8 = 3;

/// Scope whose termination can request cooperative cancellation of a task.
///
/// A token reports requests; it does not stop an application operation that
/// ignores the token or cannot be interrupted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationGuarantee {
    #[cfg(feature = "async")]
    /// Cancellation is available when observed, without a verified calculation boundary.
    BestEffort,
    /// Cancellation follows the associated Excel calculation scope.
    CalculationScoped,
    #[cfg(feature = "async")]
    /// Cancellation follows the associated subscription scope.
    SubscriptionScoped,
}

struct CancellationSlot {
    generation: AtomicU64,
    cancelled: AtomicBool,
    delivery_state: AtomicU8,
    // Slot reuse and every generation-dependent mutation share this lock. A
    // token's optimistic atomic check never authorizes a mutation by itself.
    waiters: Mutex<SlotWaiters>,
}

struct SlotWaiters {
    generation: u64,
    next_id: u64,
    entries: FxHashMap<u64, std::task::Waker>,
    // One framework task per source. Public cancellation waiters remain an
    // independent collection; task cancellation never allocates a map entry.
    #[cfg(feature = "async")]
    task_claimed: bool,
    #[cfg(feature = "async")]
    task_waker: Option<std::task::Waker>,
}

struct DetachedWaiters {
    entries: FxHashMap<u64, std::task::Waker>,
    #[cfg(feature = "async")]
    task_waker: Option<std::task::Waker>,
}

impl DetachedWaiters {
    fn wake(self) {
        // Preserve public cancellation notification before framework task
        // scheduling. No arbitrary callback runs under either registry lock.
        for (_, waker) in self.entries {
            let _ = catch_no_unwind(AssertUnwindSafe(|| waker.wake()));
        }
        #[cfg(feature = "async")]
        if let Some(waker) = self.task_waker {
            let _ = catch_no_unwind(AssertUnwindSafe(|| waker.wake()));
        }
    }
}

impl CancellationSlot {
    /// Both explicit cancellation and loss of the unique source terminate the
    /// same scope. The caller holds this slot's waiter lock through transition
    /// and detachment; arbitrary wakers run only after releasing all locks.
    fn cancel_locked(&self, waiters: &mut SlotWaiters) -> DetachedWaiters {
        let _ = self.delivery_state.compare_exchange(
            STATE_RUNNING,
            STATE_CANCELED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.cancelled.store(true, Ordering::Release);
        DetachedWaiters {
            entries: std::mem::take(&mut waiters.entries),
            #[cfg(feature = "async")]
            task_waker: waiters.task_waker.take(),
        }
    }
}

struct CancellationRegistryState {
    slots: Vec<PublishedOwner<CancellationSlot>>,
    free: Vec<u32>,
}

const CANCELLATION_SHARDS: usize = DEFAULT_STRIPE_COUNT;
const CANCELLATION_SHARD_BITS: u32 = CANCELLATION_SHARDS.trailing_zeros();
const _: () = assert!(CANCELLATION_SHARDS.is_power_of_two());

pub(crate) struct CancellationRegistry {
    // A source keeps its allocation shard in its slot ID. Completion may run
    // on another worker, but must return the slot to that same shard. Tokens
    // still point into process-stable, uniquely owned slot allocations.
    shards: [CachePadded<Mutex<CancellationRegistryState>>; CANCELLATION_SHARDS],
}

impl CancellationRegistry {
    pub(crate) const fn new() -> Self {
        Self {
            shards: [const {
                CachePadded::new(Mutex::new(CancellationRegistryState {
                    slots: Vec::new(),
                    free: Vec::new(),
                }))
            }; CANCELLATION_SHARDS],
        }
    }

    fn allocate(&self) -> (NonNull<CancellationSlot>, u64, u32) {
        self.allocate_in_shard(current_thread_stripe())
    }

    fn allocate_in_shard(&self, shard: usize) -> (NonNull<CancellationSlot>, u64, u32) {
        let mut state = self.shards[shard].lock();
        if let Some(index) = state.free.pop() {
            let slot = &state.slots[index as usize];
            let mut waiters = slot.waiters.lock();
            let generation = waiters
                .generation
                .checked_add(1)
                .expect("free slot generation");
            debug_assert!(waiters.entries.is_empty());
            #[cfg(feature = "async")]
            {
                debug_assert!(waiters.task_waker.is_none());
                waiters.task_claimed = false;
            }
            waiters.generation = generation;
            waiters.next_id = 1;
            slot.generation.store(generation, Ordering::Release);
            slot.cancelled.store(false, Ordering::Release);
            slot.delivery_state.store(STATE_RUNNING, Ordering::Release);
            (
                NonNull::from(&**slot),
                generation,
                Self::slot_id(shard, index),
            )
        } else {
            let index =
                u32::try_from(state.slots.len()).expect("cancellation slot index exhausted");
            let slot_id = Self::slot_id(shard, index);
            let slot = PublishedOwner::new(CancellationSlot {
                generation: AtomicU64::new(1),
                cancelled: AtomicBool::new(false),
                delivery_state: AtomicU8::new(STATE_RUNNING),
                waiters: Mutex::new(SlotWaiters {
                    generation: 1,
                    next_id: 1,
                    entries: FxHashMap::default(),
                    #[cfg(feature = "async")]
                    task_claimed: false,
                    #[cfg(feature = "async")]
                    task_waker: None,
                }),
            });
            let ptr = NonNull::from(&*slot);
            state.slots.push(slot);
            (ptr, 1, slot_id)
        }
    }

    fn slot_id(shard: usize, index: u32) -> u32 {
        assert!(
            index <= u32::MAX >> CANCELLATION_SHARD_BITS,
            "cancellation slot index exhausted"
        );
        (index << CANCELLATION_SHARD_BITS) | shard as u32
    }

    fn release(&self, slot_index: u32, expected_gen: u64) {
        let shard = (slot_index as usize) & (CANCELLATION_SHARDS - 1);
        let index = slot_index >> CANCELLATION_SHARD_BITS;
        let waiters = {
            let mut state = self.shards[shard].lock();
            let slot = &state.slots[index as usize];
            let mut waiters = slot.waiters.lock();
            if waiters.generation != expected_gen {
                return;
            }
            let detached = slot.cancel_locked(&mut waiters);
            drop(waiters);
            // A wrapped generation could make a process-live old token valid
            // again. Exhausted slots remain allocated, but are never reused.
            if expected_gen != u64::MAX {
                state.free.push(index);
            }
            detached
        };
        waiters.wake();
    }
}

static CANCELLATION_REGISTRY: CancellationRegistry = CancellationRegistry::new();

/// Non-owning observation of one cancellation scope.
///
/// Its unique source owns the scope's lifetime. Explicit cancellation or
/// dropping that source makes this token permanently cancelled. Copies retain
/// only a process-stable slot identity; they never extend the scope's lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancellationToken {
    slot: NonNull<CancellationSlot>,
    generation: u64,
    guarantee: CancellationGuarantee,
}

// SAFETY: CancellationSlot is heap-stable and internally synchronized.
unsafe impl Send for CancellationToken {}
// SAFETY: CancellationSlot is heap-stable and internally synchronized.
unsafe impl Sync for CancellationToken {}

pub(crate) struct CancellationSource {
    slot: NonNull<CancellationSlot>,
    generation: u64,
    slot_index: u32,
}

// SAFETY: CancellationSlot is heap-stable and internally synchronized.
unsafe impl Send for CancellationSource {}
// SAFETY: CancellationSlot is heap-stable and internally synchronized.
unsafe impl Sync for CancellationSource {}

impl CancellationSource {
    /// Claims the source's one framework task without allocating shared state.
    #[cfg(feature = "async")]
    pub(crate) fn task_registration(&self) -> TaskCancellationRegistration {
        // SAFETY: source slot allocations remain process-stable.
        let slot = unsafe { self.slot.as_ref() };
        let mut waiters = slot.waiters.lock();
        if waiters.generation != self.generation || waiters.task_claimed {
            xlfn_kernel::invariant::fail_stop();
        }
        waiters.task_claimed = true;
        TaskCancellationRegistration {
            slot: self.slot,
            generation: self.generation,
            registered_waker: None,
        }
    }

    pub(crate) fn new(guarantee: CancellationGuarantee) -> (Self, CancellationToken) {
        let (slot, generation, slot_index) = CANCELLATION_REGISTRY.allocate();
        (
            Self {
                slot,
                generation,
                slot_index,
            },
            CancellationToken {
                slot,
                generation,
                guarantee,
            },
        )
    }

    pub(crate) fn cancel(&self) {
        // SAFETY: self.slot is valid for the lifetime of this CancellationSource.
        let slot = unsafe { self.slot.as_ref() };
        let detached = {
            let mut waiters = slot.waiters.lock();
            if waiters.generation != self.generation {
                return;
            }
            slot.cancel_locked(&mut waiters)
        };
        detached.wake();
    }
}

/// Non-owning registration for the unique framework task in a cancellation
/// scope. Generation checks keep late task destruction away from reused slots.
#[cfg(feature = "async")]
pub(crate) struct TaskCancellationRegistration {
    slot: NonNull<CancellationSlot>,
    generation: u64,
    registered_waker: Option<WakerIdentity>,
}

#[cfg(feature = "async")]
// SAFETY: slots are process-stable and every mutation is synchronized. Cached
// waker pointers are identity comparisons only; neither is dereferenced.
unsafe impl Send for TaskCancellationRegistration {}

/// The identity of the source-owned waker, without owning another reference.
/// A cloned RawWaker may have a different identity from its input waker, so
/// this value is always captured from the clone actually stored in the slot.
struct WakerIdentity {
    // An address is sufficient for comparison and owns no pointee. Keeping a
    // raw pointer here would make the public Cancelled future lose Send/Sync.
    data: usize,
    vtable: &'static std::task::RawWakerVTable,
}

impl WakerIdentity {
    fn of(waker: &std::task::Waker) -> Self {
        Self {
            data: waker.data().addr(),
            vtable: waker.vtable(),
        }
    }

    fn matches(&self, waker: &std::task::Waker) -> bool {
        self.data == waker.data().addr() && std::ptr::eq(self.vtable, waker.vtable())
    }
}

#[cfg(feature = "async")]
impl TaskCancellationRegistration {
    pub(crate) fn is_cancelled(&self) -> bool {
        // SAFETY: slots are process-stable independently of source ownership.
        let slot = unsafe { self.slot.as_ref() };
        slot.generation.load(Ordering::Acquire) != self.generation
            || slot.cancelled.load(Ordering::Acquire)
            || slot.generation.load(Ordering::Acquire) != self.generation
    }

    /// Registers after a pending poll and closes the check/register race.
    pub(crate) fn poll_cancelled(&mut self, context: &Context<'_>) -> Poll<()> {
        self.poll_before_register(context, || {})
    }

    fn poll_before_register(
        &mut self,
        context: &Context<'_>,
        before_register: impl FnOnce(),
    ) -> Poll<()> {
        if self.is_cancelled() {
            self.registered_waker = None;
            return Poll::Ready(());
        }
        if self
            .registered_waker
            .as_ref()
            .is_some_and(|identity| identity.matches(context.waker()))
        {
            // The source still owns this waker unless cancel/release detached
            // it. Those paths publish cancellation before detachment, and slot
            // reuse changes generation, both checked above. A concurrent
            // cancellation must wake the registered waker, so no mutex is
            // needed to keep this unchanged registration pending.
            return Poll::Pending;
        }
        // SAFETY: slots remain allocated for the process lifetime.
        let slot = unsafe { self.slot.as_ref() };
        {
            let waiters = slot.waiters.lock();
            if waiters.generation != self.generation || self.is_cancelled() {
                self.registered_waker = None;
                return Poll::Ready(());
            }
            if waiters
                .task_waker
                .as_ref()
                .is_some_and(|waker| waker.will_wake(context.waker()))
            {
                self.registered_waker = waiters.task_waker.as_ref().map(WakerIdentity::of);
                return Poll::Pending;
            }
        }
        // Custom RawWaker clone/drop can reenter cancellation or recycle this
        // slot. The second lock therefore validates both generation and state.
        before_register();
        let mut replacement = Some(context.waker().clone());
        let detached = {
            let mut waiters = slot.waiters.lock();
            if waiters.generation != self.generation || self.is_cancelled() {
                self.registered_waker = None;
                None
            } else {
                let next = replacement.take().expect("new task waker");
                let identity = WakerIdentity::of(&next);
                let previous = waiters.task_waker.replace(next);
                // Publish the identity before releasing the lock and before
                // any old/unused waker destructor can reenter cancellation.
                self.registered_waker = Some(identity);
                previous
            }
        };
        drop(detached);
        drop(replacement);
        if self.is_cancelled() {
            self.registered_waker = None;
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[cfg(feature = "async")]
impl Drop for TaskCancellationRegistration {
    fn drop(&mut self) {
        // SAFETY: source destruction/reuse never frees the process-stable slot.
        let slot = unsafe { self.slot.as_ref() };
        let detached = {
            let mut waiters = slot.waiters.lock();
            if waiters.generation == self.generation {
                waiters.task_waker.take()
            } else {
                None
            }
        };
        // This field can be destroyed while unwinding a user-future Drop. A
        // second arbitrary destructor panic must not skip completion accounting.
        let _ = catch_no_unwind(AssertUnwindSafe(|| drop(detached)));
    }
}

impl Drop for CancellationSource {
    fn drop(&mut self) {
        CANCELLATION_REGISTRY.release(self.slot_index, self.generation);
    }
}

impl CancellationToken {
    /// Returns whether cancellation was requested or the owning scope ended.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        // SAFETY: slot memory is stable for the lifetime of the process.
        let slot = unsafe { self.slot.as_ref() };
        if slot.generation.load(Ordering::Acquire) != self.generation {
            return true;
        }
        slot.cancelled.load(Ordering::Acquire)
            || slot.delivery_state.load(Ordering::Acquire) == STATE_CANCELED
            // If the reads observed reset fields, the generation published
            // before those release stores must also be observed here.
            || slot.generation.load(Ordering::Acquire) != self.generation
    }

    /// Linearizes delivery vs cancellation using CAS on the delivery state machine.
    ///
    /// Transitions from RUNNING -> DELIVERING if cancellation has not claimed CANCELED.
    /// Returns true if this delivery caller won the right to deliver the result.
    #[cfg(feature = "async")]
    #[must_use]
    pub(crate) fn try_start_delivery(&self) -> bool {
        // SAFETY: slot memory is stable for the lifetime of the process.
        let slot = unsafe { self.slot.as_ref() };
        let waiters = slot.waiters.lock();
        if waiters.generation != self.generation {
            return false;
        }
        slot.delivery_state
            .compare_exchange(
                STATE_RUNNING,
                STATE_DELIVERING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[cfg(feature = "async")]
    pub(crate) fn finish_delivery(&self) {
        // SAFETY: slot memory is stable for the lifetime of the process.
        let slot = unsafe { self.slot.as_ref() };
        let waiters = slot.waiters.lock();
        if waiters.generation == self.generation {
            let _ = slot.delivery_state.compare_exchange(
                STATE_DELIVERING,
                STATE_DONE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    #[must_use]
    /// Returns the cancellation scope established when this token was created.
    pub const fn guarantee(&self) -> CancellationGuarantee {
        self.guarantee
    }

    /// Completes when [`Self::is_cancelled`] becomes true.
    pub fn cancelled(&self) -> Cancelled<'_> {
        Cancelled {
            token: *self,
            waiter_id: None,
            registered_waker: None,
            _marker: PhantomData,
        }
    }
}

/// Future returned by [`CancellationToken::cancelled`].
///
/// It wakes when the token's scope is cancelled or ends. Dropping the future
/// unregisters its waiter and does not cancel the scope.
pub struct Cancelled<'token> {
    token: CancellationToken,
    waiter_id: Option<u64>,
    registered_waker: Option<WakerIdentity>,
    _marker: PhantomData<&'token ()>,
}

impl Future for Cancelled<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.poll_before_lock(context, || {})
    }
}

impl Cancelled<'_> {
    // The hook fixes the optimistic-check / slot-reuse interleaving in tests;
    // the production call's no-op is eliminated during monomorphization.
    fn poll_before_lock(
        &mut self,
        context: &mut Context<'_>,
        before_lock: impl FnOnce(),
    ) -> Poll<()> {
        // SAFETY: slot memory is stable for the lifetime of the process.
        let slot = unsafe { self.token.slot.as_ref() };
        if self.token.is_cancelled() {
            self.unregister();
            return Poll::Ready(());
        }
        if self
            .registered_waker
            .as_ref()
            .is_some_and(|identity| identity.matches(context.waker()))
        {
            // Only this future removes its entry. Cancellation/source release
            // publishes the terminal state before detaching and waking it;
            // reuse changes the generation checked above. An unchanged entry
            // can therefore remain pending without locking or cloning a waker.
            return Poll::Pending;
        }

        // RawWaker clone/drop callbacks may reenter this slot. Keep the new
        // waker outside the guard's lifetime, including early returns/unwind.
        let mut replacement = Some(context.waker().clone());
        before_lock();
        let mut waiters = slot.waiters.lock();
        if waiters.generation != self.token.generation {
            self.waiter_id = None;
            self.registered_waker = None;
            return Poll::Ready(());
        }
        let detached;
        let result;
        if self.token.is_cancelled() {
            self.registered_waker = None;
            detached = self
                .waiter_id
                .take()
                .and_then(|id| waiters.entries.remove(&id));
            result = Poll::Ready(());
        } else {
            let waiter_id = *self.waiter_id.get_or_insert_with(|| {
                let id = waiters.next_id;
                waiters.next_id = id.checked_add(1).expect("cancellation waiter id exhausted");
                id
            });
            if waiters
                .entries
                .get(&waiter_id)
                .is_some_and(|waker| waker.will_wake(context.waker()))
            {
                self.registered_waker = waiters.entries.get(&waiter_id).map(WakerIdentity::of);
                detached = None;
            } else {
                let next = replacement.take().expect("new waker");
                let identity = WakerIdentity::of(&next);
                detached = waiters.entries.insert(waiter_id, next);
                self.registered_waker = Some(identity);
            }
            result = Poll::Pending;
        }
        drop(waiters);
        drop(detached);
        result
    }

    fn unregister(&mut self) {
        self.registered_waker = None;
        if let Some(waiter_id) = self.waiter_id.take() {
            // SAFETY: slot memory is stable for the lifetime of the process.
            let slot = unsafe { self.token.slot.as_ref() };
            let detached = {
                let mut waiters = slot.waiters.lock();
                if waiters.generation == self.token.generation {
                    waiters.entries.remove(&waiter_id)
                } else {
                    None
                }
            };
            drop(detached);
        }
    }
}

impl Drop for Cancelled<'_> {
    fn drop(&mut self) {
        self.unregister();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::task::{ArcWake, noop_waker, waker};
    use std::sync::Arc as StdArc;
    use std::sync::atomic::AtomicUsize;

    // Isolate slot-reuse tests from allocations in concurrently running tests.
    // Every token/future is dropped before this fixture's registry.
    struct LocalSource<'registry> {
        source: std::mem::ManuallyDrop<CancellationSource>,
        registry: &'registry CancellationRegistry,
    }

    impl std::ops::Deref for LocalSource<'_> {
        type Target = CancellationSource;

        fn deref(&self) -> &Self::Target {
            &self.source
        }
    }

    impl Drop for LocalSource<'_> {
        fn drop(&mut self) {
            self.registry
                .release(self.source.slot_index, self.source.generation);
        }
    }

    fn local_source(registry: &CancellationRegistry) -> (LocalSource<'_>, CancellationToken) {
        let (slot, generation, slot_index) = registry.allocate();
        (
            LocalSource {
                source: std::mem::ManuallyDrop::new(CancellationSource {
                    slot,
                    generation,
                    slot_index,
                }),
                registry,
            },
            CancellationToken {
                slot,
                generation,
                guarantee: CancellationGuarantee::CalculationScoped,
            },
        )
    }

    fn owned_waiter(token: CancellationToken) -> Cancelled<'static> {
        Cancelled {
            token,
            waiter_id: None,
            registered_waker: None,
            _marker: PhantomData,
        }
    }

    struct ReentrantDrop {
        nested: Option<Cancelled<'static>>,
        token: CancellationToken,
        dropped: StdArc<AtomicUsize>,
    }

    impl ArcWake for ReentrantDrop {
        fn wake_by_ref(_: &StdArc<Self>) {}
    }

    impl Drop for ReentrantDrop {
        fn drop(&mut self) {
            // SAFETY: the fixture's registry outlives all of its wakers.
            let slot = unsafe { self.token.slot.as_ref() };
            let unlocked = slot.waiters.try_lock().is_some();
            if !unlocked {
                // Fail promptly instead of hanging inside the nested Drop if
                // this regression returns. The registry still owns its waker.
                if let Some(nested) = &mut self.nested {
                    nested.waiter_id = None;
                }
            }
            assert!(unlocked, "Waker::drop ran while the slot mutex was held");
            drop(self.nested.take());
            self.dropped.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn register_reentrant_drop(
        token: CancellationToken,
        dropped: &StdArc<AtomicUsize>,
    ) -> Cancelled<'static> {
        let mut nested = owned_waiter(token);
        let noop = noop_waker();
        assert!(
            Pin::new(&mut nested)
                .poll(&mut Context::from_waker(&noop))
                .is_pending()
        );
        let waker = waker(StdArc::new(ReentrantDrop {
            nested: Some(nested),
            token,
            dropped: StdArc::clone(dropped),
        }));
        let mut future = owned_waiter(token);
        assert!(
            Pin::new(&mut future)
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        future
    }

    struct WakeCount(AtomicUsize);

    impl ArcWake for WakeCount {
        fn wake_by_ref(arc_self: &StdArc<Self>) {
            arc_self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn miri_public_waker_warm_poll_avoids_slot_lock_and_clone() {
        fn assert_send_sync_unpin<T: Send + Sync + Unpin>() {}
        assert_send_sync_unpin::<Cancelled<'static>>();

        let registry = CancellationRegistry::new();
        let (source, token) = local_source(&registry);
        let count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let waker = waker(StdArc::clone(&count));
        let mut future = token.cancelled();
        assert!(
            Pin::new(&mut future)
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        let references = StdArc::strong_count(&count);
        // SAFETY: the local source and registry outlive this future and guard.
        let slot = unsafe { token.slot.as_ref() };
        {
            let waiters = slot.waiters.lock();
            assert_eq!(waiters.entries.len(), 1);
            assert!(
                future
                    .poll_before_lock(&mut Context::from_waker(&waker), || {
                        panic!("unchanged waker must bypass registration")
                    })
                    .is_pending()
            );
            assert_eq!(StdArc::strong_count(&count), references);
        }
        source.cancel();
        assert_eq!(count.0.load(Ordering::Acquire), 1);
        assert!(
            Pin::new(&mut future)
                .poll(&mut Context::from_waker(&waker))
                .is_ready()
        );
    }

    #[test]
    fn miri_cached_public_waker_cannot_hide_slot_reuse() {
        let registry = CancellationRegistry::new();
        let (source, old_token) = local_source(&registry);
        let count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let waker = waker(StdArc::clone(&count));
        let mut old = old_token.cancelled();
        assert!(
            Pin::new(&mut old)
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        drop(source);
        let (source, token) = local_source(&registry);
        assert_eq!(token.slot, old_token.slot);
        let mut next = token.cancelled();
        assert!(
            Pin::new(&mut next)
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        assert!(
            Pin::new(&mut old)
                .poll(&mut Context::from_waker(&waker))
                .is_ready()
        );
        drop(old);
        // SAFETY: this local source retains the fixture slot through this check.
        let waiters = unsafe { token.slot.as_ref() }.waiters.lock().entries.len();
        assert_eq!(waiters, 1);
        source.cancel();
        assert_eq!(count.0.load(Ordering::Acquire), 2);
        assert!(
            Pin::new(&mut next)
                .poll(&mut Context::from_waker(&waker))
                .is_ready()
        );
    }

    struct PanicFirstWake(AtomicUsize);

    impl ArcWake for PanicFirstWake {
        fn wake_by_ref(arc_self: &StdArc<Self>) {
            if arc_self.0.fetch_add(1, Ordering::AcqRel) == 0 {
                panic!("injected cancellation waker panic");
            }
        }
    }

    #[cfg(feature = "async")]
    #[test]
    fn miri_task_registration_survives_slot_reuse_between_check_and_register() {
        let registry = CancellationRegistry::new();
        let (source, old_token) = local_source(&registry);
        let mut old = source.task_registration();
        let noop = noop_waker();
        let mut next_source = None;
        assert!(
            old.poll_before_register(&Context::from_waker(&noop), || {
                drop(source);
                next_source = Some(local_source(&registry));
            })
            .is_ready()
        );
        let (next_source, next_token) = next_source.unwrap();
        assert_eq!(old_token.slot, next_token.slot);
        let mut next = next_source.task_registration();
        let count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let waker = waker(StdArc::clone(&count));
        assert!(
            next.poll_cancelled(&Context::from_waker(&waker))
                .is_pending()
        );
        drop(old);
        assert!(!next.is_cancelled());
        next_source.cancel();
        assert_eq!(count.0.load(Ordering::Acquire), 1);
        assert!(next.poll_cancelled(&Context::from_waker(&noop)).is_ready());
    }

    #[cfg(feature = "async")]
    #[test]
    fn miri_cached_task_waker_cannot_hide_cancellation_or_slot_reuse() {
        let registry = CancellationRegistry::new();
        let (source, old_token) = local_source(&registry);
        let count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let waker = waker(StdArc::clone(&count));
        let context = Context::from_waker(&waker);
        let mut old = source.task_registration();
        assert!(old.poll_cancelled(&context).is_pending());
        source.cancel();
        assert_eq!(count.0.load(Ordering::Acquire), 1);
        assert!(old.poll_cancelled(&context).is_ready());
        drop(source);
        let (source, next_token) = local_source(&registry);
        assert_eq!(old_token.slot, next_token.slot);
        let mut next = source.task_registration();
        assert!(next.poll_cancelled(&context).is_pending());
        assert!(old.poll_cancelled(&context).is_ready());
        drop(old);
        source.cancel();
        assert_eq!(count.0.load(Ordering::Acquire), 2);
        assert!(next.poll_cancelled(&context).is_ready());
    }

    fn check_cache_tracks_the_registered_clone_identity(
        mut poll: impl FnMut(&std::task::Waker) -> Poll<()>,
        cancel: impl FnOnce(),
    ) {
        use std::task::{RawWaker, RawWakerVTable, Waker};
        struct State {
            clones: StdArc<AtomicUsize>,
            wakes: StdArc<AtomicUsize>,
        }
        unsafe fn clone(data: *const ()) -> RawWaker {
            // SAFETY: the input waker keeps this State alive for the callback.
            let state = unsafe { &*data.cast::<State>() };
            state.clones.fetch_add(1, Ordering::Relaxed);
            // RawWaker clones may use a different data pointer while retaining
            // the same wake target. Only this new allocation is source-owned.
            let cloned = StdArc::new(State {
                clones: StdArc::clone(&state.clones),
                wakes: StdArc::clone(&state.wakes),
            });
            RawWaker::new(StdArc::into_raw(cloned).cast(), &VTABLE)
        }
        unsafe fn release(data: *const ()) {
            // SAFETY: consumes one reference owned by this particular waker.
            drop(unsafe { StdArc::from_raw(data.cast::<State>()) });
        }
        unsafe fn wake_by_ref(data: *const ()) {
            // SAFETY: a borrowed waker retains the allocation during the call.
            unsafe { &*data.cast::<State>() }
                .wakes
                .fetch_add(1, Ordering::Relaxed);
        }
        unsafe fn wake(data: *const ()) {
            // SAFETY: wake consumes this waker's reference after using it.
            unsafe { wake_by_ref(data) };
            // SAFETY: wake consumes this raw waker's one owned reference.
            unsafe { release(data) };
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, release);
        let clones = StdArc::new(AtomicUsize::new(0));
        let wakes = StdArc::new(AtomicUsize::new(0));
        let state = StdArc::new(State {
            clones: StdArc::clone(&clones),
            wakes: StdArc::clone(&wakes),
        });
        let raw = RawWaker::new(StdArc::into_raw(state).cast(), &VTABLE);
        // SAFETY: every clone owns a separate State, balanced by the vtable.
        let waker = unsafe { Waker::from_raw(raw) };
        for _ in 0..2 {
            assert!(poll(&waker).is_pending());
        }
        // Caching the input identity would incorrectly suppress the second
        // registration, although that input allocation is not source-owned.
        assert_eq!(clones.load(Ordering::Relaxed), 2);
        cancel();
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert!(poll(&waker).is_ready());
    }

    #[test]
    fn miri_public_waker_cache_tracks_the_registered_clone_identity() {
        let registry = CancellationRegistry::new();
        let (source, token) = local_source(&registry);
        let mut future = token.cancelled();
        check_cache_tracks_the_registered_clone_identity(
            |waker| Pin::new(&mut future).poll(&mut Context::from_waker(waker)),
            || source.cancel(),
        );
    }

    #[cfg(feature = "async")]
    #[test]
    fn miri_task_waker_cache_tracks_the_registered_clone_identity() {
        let registry = CancellationRegistry::new();
        let (source, _) = local_source(&registry);
        let mut task = source.task_registration();
        check_cache_tracks_the_registered_clone_identity(
            |waker| task.poll_cancelled(&Context::from_waker(waker)),
            || source.cancel(),
        );
    }

    #[cfg(feature = "async")]
    #[test]
    fn miri_task_wake_follows_public_waiter_even_when_public_wake_panics() {
        struct OrderedWake {
            token: CancellationToken,
            order: StdArc<Mutex<Vec<u8>>>,
            marker: u8,
        }
        impl ArcWake for OrderedWake {
            fn wake_by_ref(this: &StdArc<Self>) {
                // SAFETY: the test retains its registry until both wakers drop.
                let slot = unsafe { this.token.slot.as_ref() };
                assert!(slot.waiters.try_lock().is_some());
                this.order.lock().push(this.marker);
                assert_ne!(this.marker, 1, "injected public waiter panic");
            }
        }
        let registry = CancellationRegistry::new();
        let (source, token) = local_source(&registry);
        let order = StdArc::new(Mutex::new(Vec::new()));
        let public_waker = waker(StdArc::new(OrderedWake {
            token,
            order: StdArc::clone(&order),
            marker: 1,
        }));
        let task_waker = waker(StdArc::new(OrderedWake {
            token,
            order: StdArc::clone(&order),
            marker: 2,
        }));
        let mut public = token.cancelled();
        assert!(
            Pin::new(&mut public)
                .poll(&mut Context::from_waker(&public_waker))
                .is_pending()
        );
        let mut task = source.task_registration();
        assert!(
            task.poll_cancelled(&Context::from_waker(&task_waker))
                .is_pending()
        );
        source.cancel();
        assert_eq!(order.lock().as_slice(), &[1, 2]);
        assert!(task.is_cancelled());
    }

    #[cfg(feature = "async")]
    #[test]
    fn miri_task_waker_clone_and_drop_can_cancel_their_source() {
        use std::task::{RawWaker, RawWakerVTable, Waker};
        struct State {
            source: CancellationSource,
            clones: AtomicUsize,
            drops: AtomicUsize,
        }
        unsafe fn clone(data: *const ()) -> RawWaker {
            // SAFETY: the source waker retains this State allocation.
            let state = unsafe { &*data.cast::<State>() };
            state.source.cancel();
            state.clones.fetch_add(1, Ordering::Relaxed);
            // SAFETY: this clone creates one reference for the returned waker.
            unsafe { StdArc::increment_strong_count(data.cast::<State>()) };
            RawWaker::new(data, &VTABLE)
        }
        unsafe fn release(data: *const ()) {
            // SAFETY: consumes exactly one reference owned by this raw waker.
            let state = unsafe { StdArc::from_raw(data.cast::<State>()) };
            state.source.cancel();
            state.drops.fetch_add(1, Ordering::Relaxed);
        }
        unsafe fn wake_by_ref(data: *const ()) {
            // SAFETY: the borrowed waker retains the state for this call.
            unsafe { &*data.cast::<State>() }.source.cancel();
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, release, wake_by_ref, release);
        let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
        let mut task = source.task_registration();
        let state = StdArc::new(State {
            source,
            clones: AtomicUsize::new(0),
            drops: AtomicUsize::new(0),
        });
        let raw = RawWaker::new(StdArc::into_raw(StdArc::clone(&state)).cast(), &VTABLE);
        // SAFETY: callbacks balance the reference and State is Send + Sync.
        let waker = unsafe { Waker::from_raw(raw) };
        assert!(task.poll_cancelled(&Context::from_waker(&waker)).is_ready());
        assert!(token.is_cancelled());
        assert_eq!(state.clones.load(Ordering::Relaxed), 1);
        assert_eq!(state.drops.load(Ordering::Relaxed), 1);
        drop(waker);
        assert_eq!(state.drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn cancellation_is_sticky_and_observable() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        assert!(!token.is_cancelled());
        source.cancel();
        assert!(token.is_cancelled());
        assert_eq!(token.guarantee(), CancellationGuarantee::CalculationScoped);

        let mut future = std::pin::pin!(token.cancelled());
        let waker = noop_waker();
        let mut context = Context::from_waker(&waker);
        assert_eq!(future.as_mut().poll(&mut context), Poll::Ready(()));
    }

    #[test]
    fn cancellation_wakes_every_registered_waiter() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let first_count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let second_count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let first_waker = waker(StdArc::clone(&first_count));
        let second_waker = waker(StdArc::clone(&second_count));
        let mut first = std::pin::pin!(token.cancelled());
        let second_token = token;
        let mut second = std::pin::pin!(second_token.cancelled());

        assert_eq!(
            first.as_mut().poll(&mut Context::from_waker(&first_waker)),
            Poll::Pending
        );
        assert_eq!(
            second
                .as_mut()
                .poll(&mut Context::from_waker(&second_waker)),
            Poll::Pending
        );

        source.cancel();

        assert_eq!(first_count.0.load(Ordering::Acquire), 1);
        assert_eq!(second_count.0.load(Ordering::Acquire), 1);
        assert_eq!(
            first.as_mut().poll(&mut Context::from_waker(&first_waker)),
            Poll::Ready(())
        );
        assert_eq!(
            second
                .as_mut()
                .poll(&mut Context::from_waker(&second_waker)),
            Poll::Ready(())
        );
    }

    #[test]
    fn panicking_waker_does_not_stop_later_cancellation_notifications() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let wake_state = StdArc::new(PanicFirstWake(AtomicUsize::new(0)));
        let panic_first_waker = waker(StdArc::clone(&wake_state));
        let mut waiters = [
            Box::pin(token.cancelled()),
            Box::pin(token.cancelled()),
            Box::pin(token.cancelled()),
        ];
        for waiter in &mut waiters {
            assert_eq!(
                waiter
                    .as_mut()
                    .poll(&mut Context::from_waker(&panic_first_waker)),
                Poll::Pending
            );
        }

        assert!(
            std::panic::catch_unwind(AssertUnwindSafe(|| source.cancel())).is_ok(),
            "CancellationSource::cancel must not propagate a user Waker panic"
        );
        assert_eq!(wake_state.0.load(Ordering::Acquire), waiters.len());
        for waiter in &mut waiters {
            assert_eq!(
                waiter
                    .as_mut()
                    .poll(&mut Context::from_waker(&panic_first_waker)),
                Poll::Ready(())
            );
        }
    }

    #[test]
    fn panicking_payload_drop_cannot_interrupt_cancel_or_release_notifications() {
        struct PayloadWake {
            wakes: AtomicUsize,
            payload_drops: StdArc<AtomicUsize>,
        }

        impl ArcWake for PayloadWake {
            fn wake_by_ref(state: &StdArc<Self>) {
                if state.wakes.fetch_add(1, Ordering::AcqRel) == 0 {
                    std::panic::panic_any(crate::panic_boundary::tests::PanickingPayload(
                        StdArc::clone(&state.payload_drops),
                    ));
                }
            }
        }

        for release in [false, true] {
            let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
            let payload_drops = StdArc::new(AtomicUsize::new(0));
            let wake_state = StdArc::new(PayloadWake {
                wakes: AtomicUsize::new(0),
                payload_drops: StdArc::clone(&payload_drops),
            });
            let waker = waker(StdArc::clone(&wake_state));
            let mut waiters = [token.cancelled(), token.cancelled(), token.cancelled()];
            for waiter in &mut waiters {
                assert!(
                    Pin::new(waiter)
                        .poll(&mut Context::from_waker(&waker))
                        .is_pending()
                );
            }
            if release {
                drop(source);
            } else {
                source.cancel();
            }
            assert_eq!(wake_state.wakes.load(Ordering::Acquire), waiters.len());
            assert_eq!(payload_drops.load(Ordering::Acquire), 0);
            for waiter in &mut waiters {
                assert!(
                    Pin::new(waiter)
                        .poll(&mut Context::from_waker(&waker))
                        .is_ready()
                );
            }
        }
    }

    #[test]
    fn cancellation_after_delivery_start_preserves_completion_authority() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        assert!(token.try_start_delivery());
        // Second delivery attempt fails
        assert!(!token.try_start_delivery());

        // Cancel after delivery started cannot transition state to CANCELED
        source.cancel();
        assert!(token.is_cancelled());
        // SAFETY: cancellation slots remain allocated for the process lifetime.
        let slot = unsafe { token.slot.as_ref() };
        assert_eq!(
            slot.delivery_state.load(Ordering::Acquire),
            STATE_DELIVERING
        );
        token.finish_delivery();
        assert_eq!(slot.delivery_state.load(Ordering::Acquire), STATE_DONE);
        assert!(!token.try_start_delivery());
    }

    #[test]
    fn cancellation_prevents_delivery_cas() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        source.cancel();
        // Delivery CAS fails because token is CANCELED
        assert!(!token.try_start_delivery());
    }

    #[test]
    fn dropping_waiter_unregisters_it() {
        let (_source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        {
            let mut future = std::pin::pin!(token.cancelled());
            let waker = noop_waker();
            assert_eq!(
                future.as_mut().poll(&mut Context::from_waker(&waker)),
                Poll::Pending
            );
            // SAFETY: slot memory is stable.
            let slot = unsafe { token.slot.as_ref() };
            assert_eq!(slot.waiters.lock().entries.len(), 1);
        }
        // SAFETY: slot memory is stable.
        let slot = unsafe { token.slot.as_ref() };
        assert!(slot.waiters.lock().entries.is_empty());
    }

    #[test]
    fn miri_source_drop_cancels_token_and_waiter_before_slot_reuse() {
        let registry = CancellationRegistry::new();
        let (source, token) = local_source(&registry);
        assert!(!token.is_cancelled());
        let mut future = std::pin::pin!(token.cancelled());
        let wake_count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let waker = waker(StdArc::clone(&wake_count));
        assert_eq!(
            future.as_mut().poll(&mut Context::from_waker(&waker)),
            Poll::Pending
        );

        drop(source);

        assert!(token.is_cancelled());
        assert_eq!(wake_count.0.load(Ordering::Acquire), 1);
        #[cfg(feature = "async")]
        assert!(!token.try_start_delivery());
        assert_eq!(
            future.as_mut().poll(&mut Context::from_waker(&waker)),
            Poll::Ready(())
        );
    }

    #[test]
    fn slot_reuse_advances_generation_and_leaves_old_token_stale() {
        let registry = CancellationRegistry::new();
        let (source1, token1) = local_source(&registry);
        let gen1 = token1.generation;
        let slot_ptr1 = token1.slot;
        drop(source1);
        assert!(token1.is_cancelled());

        let (source2, token2) = local_source(&registry);
        assert_eq!(token2.slot, slot_ptr1);
        assert_eq!(token2.generation, gen1 + 1);
        assert!(!token2.is_cancelled());
        assert!(token1.is_cancelled());
        drop(source2);
    }

    #[test]
    fn unrelated_pool_shard_does_not_block_source_lifecycle() {
        let registry = CancellationRegistry::new();
        let shard = current_thread_stripe();
        let _busy = registry.shards[(shard + 1) % CANCELLATION_SHARDS].lock();
        let (source, token) = local_source(&registry);
        assert!(!token.is_cancelled());
        drop(source);
        assert!(token.is_cancelled());
    }

    #[test]
    fn miri_cross_thread_source_drop_returns_slot_to_allocation_shard() {
        let registry = CancellationRegistry::new();
        let (source, old_token) = local_source(&registry);
        let slot_id = source.slot_index;
        let count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let waker = waker(StdArc::clone(&count));
        let mut waiter = owned_waiter(old_token);
        assert!(
            Pin::new(&mut waiter)
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        std::thread::scope(|scope| {
            scope.spawn(move || drop(source)).join().unwrap();
        });
        assert_eq!(count.0.load(Ordering::Acquire), 1);
        assert!(old_token.is_cancelled());
        let (next_source, next_token) = local_source(&registry);
        assert_eq!(next_source.slot_index, slot_id);
        assert_eq!(next_token.slot, old_token.slot);
        assert_eq!(next_token.generation, old_token.generation + 1);
        assert!(!next_token.is_cancelled());
        assert!(
            Pin::new(&mut waiter)
                .poll(&mut Context::from_waker(&waker))
                .is_ready()
        );
        assert!(!next_token.is_cancelled());
    }

    #[test]
    fn slot_ids_distinguish_equal_local_indices_in_different_shards() {
        let registry = CancellationRegistry::new();
        let (first, first_generation, first_id) = registry.allocate_in_shard(0);
        let (second, second_generation, second_id) =
            registry.allocate_in_shard(CANCELLATION_SHARDS - 1);
        assert_ne!(first_id, second_id);
        assert_ne!(first, second);
        registry.release(second_id, second_generation);
        // SAFETY: the local registry retains both slot allocations.
        assert!(!unsafe { first.as_ref() }.cancelled.load(Ordering::Acquire));
        let (reused, generation, id) = registry.allocate_in_shard(CANCELLATION_SHARDS - 1);
        assert_eq!(reused, second);
        assert_eq!(id, second_id);
        assert_eq!(generation, second_generation + 1);
        registry.release(first_id, first_generation);
        registry.release(id, generation);
    }

    #[test]
    fn miri_repoll_across_slot_reuse_preserves_new_generation_waiter() {
        // Cover both a previously registered waiter (the ID collision) and a
        // first poll that reaches registration only after the slot is reused.
        for already_registered in [false, true] {
            let registry = CancellationRegistry::new();
            let (source, old_token) = local_source(&registry);
            let mut old_waiter = old_token.cancelled();
            let noop = noop_waker();
            if already_registered {
                assert!(
                    Pin::new(&mut old_waiter)
                        .poll(&mut Context::from_waker(&noop))
                        .is_pending()
                );
                assert_eq!(old_waiter.waiter_id, Some(1));
            }
            let paused = std::sync::Barrier::new(2);
            let resume = std::sync::Barrier::new(2);
            // A different waker forces the registration path even when the
            // first poll populated the unchanged-waker cache.
            let replacement = waker(StdArc::new(WakeCount(AtomicUsize::new(0))));
            std::thread::scope(|scope| {
                let old_poll = scope.spawn(|| {
                    old_waiter.poll_before_lock(&mut Context::from_waker(&replacement), || {
                        paused.wait();
                        resume.wait();
                    })
                });
                paused.wait();
                drop(source);
                let (next_source, next_token) = local_source(&registry);
                assert_eq!(next_token.slot, old_token.slot);
                let count = StdArc::new(WakeCount(AtomicUsize::new(0)));
                let waker = waker(StdArc::clone(&count));
                let mut next_waiter = next_token.cancelled();
                assert!(
                    Pin::new(&mut next_waiter)
                        .poll(&mut Context::from_waker(&waker))
                        .is_pending()
                );
                assert_eq!(next_waiter.waiter_id, Some(1));
                resume.wait();
                assert_eq!(old_poll.join().unwrap(), Poll::Ready(()));
                // SAFETY: next_source keeps its fixture slot live.
                let slot = unsafe { next_token.slot.as_ref() };
                assert_eq!(slot.waiters.lock().entries.len(), 1);
                next_source.cancel();
                assert_eq!(count.0.load(Ordering::Acquire), 1);
                assert!(
                    Pin::new(&mut next_waiter)
                        .poll(&mut Context::from_waker(&waker))
                        .is_ready()
                );
            });
        }
    }

    #[test]
    fn dropping_stale_waiter_preserves_new_generation_waiter() {
        let registry = CancellationRegistry::new();
        let (source, token) = local_source(&registry);
        let mut stale = token.cancelled();
        let noop = noop_waker();
        assert!(
            Pin::new(&mut stale)
                .poll(&mut Context::from_waker(&noop))
                .is_pending()
        );
        drop(source);
        let (next_source, next_token) = local_source(&registry);
        let count = StdArc::new(WakeCount(AtomicUsize::new(0)));
        let waker = waker(StdArc::clone(&count));
        let mut next = next_token.cancelled();
        assert!(
            Pin::new(&mut next)
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        drop(stale);
        next_source.cancel();
        assert_eq!(count.0.load(Ordering::Acquire), 1);
    }

    #[test]
    fn replacing_waker_can_reenter_waiter_unregistration() {
        let registry = CancellationRegistry::new();
        let (_source, token) = local_source(&registry);
        let dropped = StdArc::new(AtomicUsize::new(0));
        let mut future = register_reentrant_drop(token, &dropped);
        let noop = noop_waker();
        assert!(
            Pin::new(&mut future)
                .poll(&mut Context::from_waker(&noop))
                .is_pending()
        );
        assert_eq!(dropped.load(Ordering::Acquire), 1);
        // SAFETY: the registry outlives the future.
        let slot = unsafe { token.slot.as_ref() };
        assert_eq!(slot.waiters.lock().entries.len(), 1);
    }

    #[test]
    fn dropping_waker_can_reenter_waiter_unregistration() {
        let registry = CancellationRegistry::new();
        let (_source, token) = local_source(&registry);
        let dropped = StdArc::new(AtomicUsize::new(0));
        drop(register_reentrant_drop(token, &dropped));
        assert_eq!(dropped.load(Ordering::Acquire), 1);
        // SAFETY: the registry outlives its tokens.
        let slot = unsafe { token.slot.as_ref() };
        assert!(slot.waiters.lock().entries.is_empty());
    }

    #[test]
    fn terminal_repoll_drops_waker_outside_waiter_lock() {
        let registry = CancellationRegistry::new();
        let (_source, token) = local_source(&registry);
        let dropped = StdArc::new(AtomicUsize::new(0));
        let mut future = register_reentrant_drop(token, &dropped);
        let noop = noop_waker();
        // SAFETY: the registry outlives its tokens.
        let slot = unsafe { token.slot.as_ref() };
        assert!(
            future
                .poll_before_lock(&mut Context::from_waker(&noop), || {
                    slot.cancelled.store(true, Ordering::Release);
                })
                .is_ready()
        );
        assert_eq!(dropped.load(Ordering::Acquire), 1);
        assert!(slot.waiters.lock().entries.is_empty());
    }

    #[test]
    fn miri_raw_waker_clone_and_unused_clone_drop_run_outside_slot_lock() {
        use std::task::{RawWaker, RawWakerVTable, Waker};

        struct RawState {
            token: CancellationToken,
            nested: Mutex<Option<Cancelled<'static>>>,
            clones: AtomicUsize,
            drops: AtomicUsize,
        }

        impl RawState {
            fn reenter(&self) {
                // SAFETY: the fixture registry outlives every raw waker.
                let slot = unsafe { self.token.slot.as_ref() };
                let unlocked = slot.waiters.try_lock().is_some();
                let mut nested = self.nested.lock().take();
                if !unlocked && let Some(nested) = &mut nested {
                    nested.waiter_id = None;
                }
                assert!(
                    unlocked,
                    "RawWaker callback ran while the slot mutex was held"
                );
                drop(nested);
            }
        }

        unsafe fn clone(data: *const ()) -> RawWaker {
            // SAFETY: data is an Arc<RawState> pointer retained by the waker.
            let state = unsafe { &*data.cast::<RawState>() };
            state.reenter();
            state.clones.fetch_add(1, Ordering::AcqRel);
            // SAFETY: the source waker still owns a strong reference.
            unsafe { StdArc::increment_strong_count(data.cast::<RawState>()) };
            RawWaker::new(data, &VTABLE)
        }

        unsafe fn release(data: *const ()) {
            // SAFETY: this callback consumes exactly one raw strong reference.
            let state = unsafe { StdArc::from_raw(data.cast::<RawState>()) };
            state.reenter();
            state.drops.fetch_add(1, Ordering::AcqRel);
        }

        unsafe fn wake_by_ref(_: *const ()) {}

        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, release, wake_by_ref, release);
        static OTHER_VTABLE: RawWakerVTable =
            RawWakerVTable::new(clone, release, wake_by_ref, release);

        let registry = CancellationRegistry::new();
        let (source, token) = local_source(&registry);
        let noop = noop_waker();
        let mut nested = owned_waiter(token);
        assert!(
            Pin::new(&mut nested)
                .poll(&mut Context::from_waker(&noop))
                .is_pending()
        );
        let state = StdArc::new(RawState {
            token,
            nested: Mutex::new(Some(nested)),
            clones: AtomicUsize::new(0),
            drops: AtomicUsize::new(0),
        });
        let raw = RawWaker::new(StdArc::into_raw(StdArc::clone(&state)).cast(), &VTABLE);
        // SAFETY: the vtable balances Arc references and is thread-safe.
        let waker = unsafe { Waker::from_raw(raw) };
        let mut future = token.cancelled();
        for _ in 0..2 {
            assert!(
                Pin::new(&mut future)
                    .poll(&mut Context::from_waker(&waker))
                    .is_pending()
            );
        }
        assert_eq!(state.clones.load(Ordering::Acquire), 1);
        assert_eq!(state.drops.load(Ordering::Acquire), 0);
        assert!(state.nested.lock().is_none());

        let raw = RawWaker::new(
            StdArc::into_raw(StdArc::clone(&state)).cast(),
            &OTHER_VTABLE,
        );
        // SAFETY: the alternate vtable balances this new raw Arc reference.
        let changed_waker = unsafe { Waker::from_raw(raw) };
        let mut next_source = None;
        assert!(
            future
                .poll_before_lock(&mut Context::from_waker(&changed_waker), || {
                    drop(source);
                    next_source = Some(local_source(&registry).0);
                })
                .is_ready()
        );
        assert_eq!(state.clones.load(Ordering::Acquire), 2);
        assert_eq!(state.drops.load(Ordering::Acquire), 2);
        drop(next_source);

        #[cfg(feature = "async")]
        {
            let (source, token) = local_source(&registry);
            let state = StdArc::new(RawState {
                token,
                nested: Mutex::new(Some(owned_waiter(token))),
                clones: AtomicUsize::new(0),
                drops: AtomicUsize::new(0),
            });
            let raw = RawWaker::new(StdArc::into_raw(StdArc::clone(&state)).cast(), &VTABLE);
            // SAFETY: this waker owns one reference balanced by the same vtable.
            let waker = unsafe { Waker::from_raw(raw) };
            let mut task = source.task_registration();
            for _ in 0..2 {
                assert!(
                    task.poll_cancelled(&Context::from_waker(&waker))
                        .is_pending()
                );
            }
            assert_eq!(state.clones.load(Ordering::Acquire), 1);
            assert_eq!(state.drops.load(Ordering::Acquire), 0);
            drop(waker);
            assert!(
                task.poll_cancelled(&Context::from_waker(&noop))
                    .is_pending()
            );
            assert_eq!(state.drops.load(Ordering::Acquire), 2);
            assert!(state.nested.lock().is_none());
        }
    }

    #[test]
    fn released_and_stale_tokens_cannot_mutate_delivery_state() {
        let registry = CancellationRegistry::new();
        let (source, old_token) = local_source(&registry);
        drop(source);
        assert!(!old_token.try_start_delivery());
        let (_source, token) = local_source(&registry);
        assert!(!old_token.try_start_delivery());
        old_token.finish_delivery();
        assert!(token.try_start_delivery());
        old_token.finish_delivery();
        // SAFETY: the registry outlives its tokens.
        let slot = unsafe { token.slot.as_ref() };
        assert_eq!(
            slot.delivery_state.load(Ordering::Acquire),
            STATE_DELIVERING
        );
        token.finish_delivery();
        assert_eq!(slot.delivery_state.load(Ordering::Acquire), STATE_DONE);
    }

    #[test]
    fn exhausted_slot_generation_is_never_reused() {
        let registry = CancellationRegistry::new();
        let (slot_ptr, _, index) = registry.allocate();
        // SAFETY: the registry owns this slot for the duration of the test.
        let slot = unsafe { slot_ptr.as_ref() };
        slot.generation.store(u64::MAX, Ordering::Release);
        slot.waiters.lock().generation = u64::MAX;
        registry.release(index, u64::MAX);
        let (next, generation, _) = registry.allocate();
        assert_ne!(next, slot_ptr);
        assert_eq!(generation, 1);
    }

    #[test]
    fn wake_from_release_can_reenter_cancellation_registry_without_deadlock() {
        struct ReentrantWake;
        impl ArcWake for ReentrantWake {
            fn wake_by_ref(_arc_self: &StdArc<Self>) {
                let (source, token) =
                    CancellationSource::new(CancellationGuarantee::CalculationScoped);
                assert!(!token.is_cancelled());
                drop(source);
            }
        }

        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let mut future = std::pin::pin!(token.cancelled());
        let reentrant_waker = waker(StdArc::new(ReentrantWake));
        assert_eq!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(&reentrant_waker)),
            Poll::Pending
        );

        drop(source);
    }
}
