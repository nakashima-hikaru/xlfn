//! Lifecycle and orchestration for the formula-handle registry.
//!
//! `BindingTable` is the only mutable binding store. Each live slot owns its
//! `BindingRecord`, which holds an `ObjectBinding` capability; immutable
//! snapshots provide the call-scoped read capability. There is no second
//! record arena or resurrection path to keep in sync.

#[cfg(test)]
use super::binding::BindingState;
use super::binding::{BindingReservation, BindingTable};
use super::object::{ObjectArena, ObjectBinding, PendingObjectBinding};
use super::token::{HandleId, HandleToken, ObjectId, TokenCodec};
use super::{ExcelHandleObject, Handle};
use crate::panic_boundary::catch_no_unwind;
#[cfg(any(test, feature = "refinement"))]
use crate::sync::Mutex;
use crate::{XllError, XllResult};
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU8, Ordering};

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandleRegistryPhase {
    Open = 0,
    Closing = 1,
    Sealing = 2,
    Closed = 3,
}

impl HandleRegistryPhase {
    fn from_raw(raw: u8) -> Self {
        match raw {
            value if value == Self::Open as u8 => Self::Open,
            value if value == Self::Closing as u8 => Self::Closing,
            value if value == Self::Sealing as u8 => Self::Sealing,
            value if value == Self::Closed as u8 => Self::Closed,
            _ => Self::Closed,
        }
    }
}

/// A sealed view of one registry. The borrowed owner is the only registry
/// whose object arena this capability can finish draining.
pub(crate) struct HandleRegistrySealed<'registry> {
    registry: &'registry HandleRegistry,
}

impl std::fmt::Debug for HandleRegistrySealed<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HandleRegistrySealed")
            .finish_non_exhaustive()
    }
}

impl HandleRegistrySealed<'_> {
    pub(super) fn finish_quiescence(&self) -> XllResult<()> {
        self.registry.objects.finish_quiescence()
    }

    pub(super) fn quiescent(
        self,
        generation: Option<crate::generation::RuntimeGeneration>,
    ) -> XllResult<crate::shutdown::HandlesQuiescent> {
        super::runtime::HandlesQuiescent::finish_registry(self, generation)
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InsertBoundary {
    BeforeReserve,
    AfterReserve,
    BeforePublish,
    AfterPublish,
}

#[cfg(test)]
type InsertPause = (
    InsertBoundary,
    std::sync::mpsc::SyncSender<()>,
    std::sync::mpsc::Receiver<()>,
);

struct ValidatedBinding<'registry> {
    registry: &'registry HandleRegistry,
    object: ObjectBinding,
}

struct ReservedBinding<'registry> {
    // Roll back the slot and unlock the table before the object can run Drop.
    reservation: BindingReservation<'registry>,
    validated: ValidatedBinding<'registry>,
}

struct PublishedBinding {
    id: HandleId,
    object_id: ObjectId,
    reused: bool,
}

impl<'registry> ValidatedBinding<'registry> {
    fn reserve(self) -> XllResult<ReservedBinding<'registry>> {
        let registry = self.registry;
        #[cfg(test)]
        registry.pause_insert(InsertBoundary::BeforeReserve);
        let reservation = registry.bindings.reserve()?;
        let reserved = ReservedBinding {
            reservation,
            validated: self,
        };
        #[cfg(test)]
        registry.pause_insert(InsertBoundary::AfterReserve);
        Ok(reserved)
    }
}

impl ReservedBinding<'_> {
    fn publish(self) -> XllResult<PublishedBinding> {
        if !self.validated.registry.is_open() {
            return Err(XllError::CLOSING);
        }
        #[cfg(test)]
        self.validated
            .registry
            .pause_insert(InsertBoundary::BeforePublish);
        let object_id = self.validated.object.id();
        let Self {
            reservation,
            validated: ValidatedBinding { object, .. },
        } = self;
        let (id, reused) = reservation.publish(object);
        Ok(PublishedBinding {
            id,
            object_id,
            reused,
        })
    }
}

pub(crate) struct HandleRegistry {
    pub(super) codec: TokenCodec,
    pub(super) phase: AtomicU8,
    pub(super) bindings: BindingTable,
    pub(super) objects: xlfn_kernel::published_owner::PublishedOwner<ObjectArena>,
    #[cfg(test)]
    pub(super) after_lookup_live: Mutex<
        Option<(
            std::sync::mpsc::SyncSender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
    #[cfg(test)]
    pub(super) insert_pause: Mutex<Option<InsertPause>>,
    #[cfg(any(test, feature = "refinement"))]
    pub(super) trace: Mutex<Option<crate::shutdown_trace::ShutdownTraceHandle>>,
}

impl HandleRegistry {
    pub(crate) fn try_new(maximum_bindings: u32) -> XllResult<Self> {
        Self::try_new_with(maximum_bindings, |entropy| getrandom::fill(entropy), true)
    }

    pub(crate) fn try_new_with<E>(
        maximum_bindings: u32,
        fill: impl FnOnce(&mut [u8; 40]) -> Result<(), E>,
        report_failure: bool,
    ) -> XllResult<Self>
    where
        E: std::fmt::Debug,
    {
        let mut entropy = [0_u8; 40];
        if let Err(source) = fill(&mut entropy) {
            let error =
                crate::error::construct::internal(crate::diagnostics::catalog::HANDLE_ENTROPY);
            if report_failure {
                let _ = catch_no_unwind(AssertUnwindSafe(|| {
                    tracing::error!(
                        error = ?source,
                        diagnostic_id = crate::diagnostics::catalog::HANDLE_ENTROPY.as_u64(),
                        "OS CSPRNG failed while initializing Excel handle tokens"
                    );
                }));
                crate::diagnostics::report_no_unwind("handle_registry_init", &error);
            }
            return Err(error);
        }
        Ok(Self::from_entropy(maximum_bindings, entropy))
    }

    pub(crate) fn from_entropy(maximum_bindings: u32, entropy: [u8; 40]) -> Self {
        let session = u64::from_le_bytes(
            entropy[..8]
                .try_into()
                .expect("the session entropy slice has eight bytes"),
        );
        let secret = entropy[8..]
            .try_into()
            .expect("the handle MAC key slice has 32 bytes");
        Self {
            codec: TokenCodec::new(session, secret),
            phase: AtomicU8::new(HandleRegistryPhase::Open as u8),
            bindings: BindingTable::new(maximum_bindings),
            objects: xlfn_kernel::published_owner::PublishedOwner::new(ObjectArena::new()),
            #[cfg(test)]
            after_lookup_live: Mutex::new(None),
            #[cfg(test)]
            insert_pause: Mutex::new(None),
            #[cfg(any(test, feature = "refinement"))]
            trace: Mutex::new(None),
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.objects.set_trace_sink(triomphe::Arc::clone(&trace));
        *self.trace.lock() = Some(trace);
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_shutdown_event(&self, event: crate::shutdown_trace::ShutdownEvent) {
        if let Some(trace) = self.trace.lock().as_ref() {
            trace.record(event);
        }
    }

    #[cfg(all(target_os = "windows", any(test, feature = "refinement")))]
    pub(crate) fn trace_handle(&self) -> Option<crate::shutdown_trace::ShutdownTraceHandle> {
        self.trace.lock().clone()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn new(maximum_bindings: u32) -> Self {
        Self::try_new(maximum_bindings).expect("test host provides an OS CSPRNG")
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        usize::try_from(self.bindings.read_state().live_bindings)
            .expect("binding count fits in usize")
    }

    pub(crate) fn phase(&self) -> HandleRegistryPhase {
        HandleRegistryPhase::from_raw(self.phase.load(Ordering::Acquire))
    }

    fn is_open(&self) -> bool {
        self.phase() == HandleRegistryPhase::Open
    }

    pub(crate) fn new_object<'registry, T: Send + Sync + 'static>(
        &'registry self,
        value: T,
    ) -> XllResult<PendingObjectBinding<'registry, T>> {
        PendingObjectBinding::new(self, value)
    }

    #[inline]
    pub(crate) fn owns_object_arena(&self, arena: NonNull<ObjectArena>) -> bool {
        arena == NonNull::from(self.objects.as_ref())
    }

    #[cfg(test)]
    pub(crate) fn insert_pending<T>(&self, value: &mut Option<T>) -> XllResult<String>
    where
        T: Send + Sync + 'static,
    {
        let object = self.new_object(value.take().expect("pending handle value is armed"))?;
        object
            .publish()
            .map(|(token, _binding_id, _object_id, _reused)| token)
    }

    #[cfg(feature = "bench-internals")]
    pub(crate) fn publish_pending<T>(
        pending: PendingObjectBinding<'_, T>,
    ) -> XllResult<(String, HandleId, ObjectId, bool)>
    where
        T: Send + Sync + 'static,
    {
        pending.publish()
    }

    // Only the typed PendingObjectBinding publication path calls this final
    // erased storage boundary; the table owns heterogeneous object payloads.
    pub(super) fn publish_object_binding(
        &self,
        object: ObjectBinding,
    ) -> XllResult<(String, HandleId, ObjectId, bool)> {
        let PublishedBinding {
            id,
            object_id,
            reused,
        } = self.validate_binding(object)?.reserve()?.publish()?;
        #[cfg(test)]
        self.pause_insert(InsertBoundary::AfterPublish);
        #[cfg(any(test, feature = "refinement"))]
        self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::AddHandle);
        Ok((self.codec.format(id), id, object_id, reused))
    }

    fn validate_binding(&self, object: ObjectBinding) -> XllResult<ValidatedBinding<'_>> {
        if !self.is_open() {
            return Err(XllError::CLOSING);
        }
        if !self.owns_object_arena(object.arena()) {
            return Err(XllError::STALE_HANDLE);
        }
        Ok(ValidatedBinding {
            registry: self,
            object,
        })
    }

    #[cfg(test)]
    fn pause_insert(&self, boundary: InsertBoundary) {
        let pause = {
            let mut pause = self.insert_pause.lock();
            if pause.as_ref().is_some_and(|(at, _, _)| *at == boundary) {
                pause.take()
            } else {
                None
            }
        };
        if let Some((_, entered, resume)) = pause {
            entered.send(()).unwrap();
            resume
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
        }
    }

    #[cfg(test)]
    #[allow(
        unsafe_code,
        reason = "Test-only handle lookup clones value from lease"
    )]
    pub(crate) fn lookup<T>(&self, token: &str) -> XllResult<T>
    where
        T: Send + Sync + Clone + 'static,
    {
        let verified = self
            .codec
            .parse(std::ptr::from_ref(self).addr(), HandleToken::new(token))?;
        if !self.is_open() {
            return Err(XllError::CLOSING);
        }
        let lease = self.bindings.read_standalone(verified.id)?;
        let record = lease.record();
        if record.state() != BindingState::Live {
            return Err(XllError::STALE_HANDLE);
        }
        let value = record
            .object()
            .typed_projection::<T>()
            .ok_or(XllError::INVALID_HANDLE)?;
        // SAFETY: lease ensures the binding and object cell remain alive for this read.
        let object_ref = unsafe { value.as_ref_unchecked() };
        Ok(object_ref.clone())
    }

    pub(crate) fn lookup_handle<'call, T>(
        &'call self,
        context: &mut crate::value::DecodeCallContext<'_, 'call>,
        token: &str,
    ) -> XllResult<Handle<'call, T>>
    where
        T: ExcelHandleObject,
    {
        let verified = self
            .codec
            .parse(std::ptr::from_ref(self).addr(), HandleToken::new(token))?;
        if !self.is_open() {
            return Err(XllError::CLOSING);
        }
        let witness = context.enter_handle_domain(self.bindings.read_domain())?;
        let binding = self.bindings.read_scoped(verified.id, witness)?;
        #[cfg(test)]
        {
            // Test-only interleaving control; production retains the original
            // lookup body and has no callback wrapper or hook bookkeeping.
            let pause = self.after_lookup_live.lock().take();
            if let Some((observed, resume)) = pause {
                observed.send(()).unwrap();
                resume
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .unwrap();
            }
        }
        // read_scoped's Live observation is the lookup linearization point.
        // The call permit protects projection and the result across withdrawal.
        Handle::new(binding)
    }

    #[cfg(test)]
    pub(crate) fn remove<T: Send + Sync + 'static>(&self, token: &str) -> XllResult<()> {
        let verified = self
            .codec
            .parse(std::ptr::from_ref(self).addr(), HandleToken::new(token))?;
        let removal = self.bindings.begin_removal(verified.id)?;
        if removal.object().typed_projection::<T>().is_none() {
            return Err(XllError::INVALID_HANDLE);
        }
        removal.commit();
        #[cfg(any(test, feature = "refinement"))]
        self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::RemoveHandle);
        Ok(())
    }

    fn remove_with_kind(
        &self,
        token: &str,
        operation: &'static str,
        on_linearized: impl FnOnce(bool),
    ) -> XllResult<bool> {
        let verified = self
            .codec
            .parse(std::ptr::from_ref(self).addr(), HandleToken::new(token))?;
        if !self.is_open() {
            return Err(XllError::CLOSING);
        }
        let removal = self.bindings.begin_removal(verified.id)?;
        let reusable = removal.commit();
        on_linearized(reusable);
        let _ = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            tracing::trace!(operation, "handle binding retired");
        }));
        #[cfg(any(test, feature = "refinement"))]
        self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::RemoveHandle);
        Ok(reusable)
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.objects.cleanup_result()
    }

    #[cfg(test)]
    pub(crate) fn remove_and_drop(&self, token: &str, operation: &'static str) {
        let _ = self.remove_and_drop_with_observer(token, operation, |_| {});
    }

    pub(crate) fn remove_and_drop_with_observer(
        &self,
        token: &str,
        operation: &'static str,
        on_linearized: impl FnOnce(bool),
    ) -> Option<bool> {
        self.remove_with_kind(token, operation, on_linearized).ok()
    }

    pub(crate) fn retire_values_for_seal(&self) -> usize {
        let (live_bindings, retired) = self.bindings.retire_all();
        #[cfg(any(test, feature = "refinement"))]
        for _ in 0..live_bindings {
            self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::RemoveHandle);
        }
        // `retire_all` releases the binding-table write lock before returning;
        // dropping these records here keeps arbitrary user destructors out of
        // that lock's critical section.
        drop(retired);
        usize::try_from(live_bindings).expect("binding count fits in usize")
    }

    pub(crate) fn begin_close(&self) {
        let _ = self.phase.compare_exchange(
            HandleRegistryPhase::Open as u8,
            HandleRegistryPhase::Closing as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(super) fn seal(&self) -> XllResult<HandleRegistrySealed<'_>> {
        if self.bindings.read_domain().is_reclaiming_here() {
            return Err(XllError::CLOSING);
        }
        loop {
            let phase = self.phase();
            match phase {
                HandleRegistryPhase::Closed => {
                    return Ok(HandleRegistrySealed { registry: self });
                }
                HandleRegistryPhase::Sealing => return Err(XllError::CLOSING),
                HandleRegistryPhase::Open | HandleRegistryPhase::Closing => {
                    if self
                        .phase
                        .compare_exchange(
                            phase as u8,
                            HandleRegistryPhase::Sealing as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        break;
                    }
                }
            }
        }
        self.objects.seal();
        self.retire_values_for_seal();
        self.bindings.read_domain().seal();
        self.phase
            .store(HandleRegistryPhase::Closed as u8, Ordering::Release);
        Ok(HandleRegistrySealed { registry: self })
    }
}

impl Drop for HandleRegistry {
    fn drop(&mut self) {
        // Reclamation always borrows this registry. Reentering its destruction
        // from a raw capability would violate that capability's owner contract;
        // stop before recovering any published allocation in that case.
        if self.bindings.read_domain().is_reclaiming_here() {
            xlfn_kernel::invariant::fail_stop();
        }
        self.objects.seal();
        self.retire_values_for_seal();
        self.bindings.read_domain().seal();
        self.objects.assert_reclaimable();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::{Arc, Weak, mpsc};

    #[derive(Default)]
    struct DropObservation {
        count: AtomicUsize,
        writer_released: AtomicBool,
    }

    struct DropProbe {
        registry: Weak<HandleRegistry>,
        observation: Arc<DropObservation>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let writer_released = self
                .registry
                .upgrade()
                .is_some_and(|registry| registry.bindings.try_read_state().is_some());
            self.observation
                .writer_released
                .store(writer_released, Ordering::Release);
            self.observation.count.fetch_add(1, Ordering::Release);
        }
    }

    fn pending<'registry>(
        registry: &'registry Arc<HandleRegistry>,
        observation: &Arc<DropObservation>,
    ) -> PendingObjectBinding<'registry, DropProbe> {
        registry
            .new_object(DropProbe {
                registry: Arc::downgrade(registry),
                observation: Arc::clone(observation),
            })
            .unwrap()
    }

    fn assert_released(observation: &DropObservation) {
        assert_eq!(observation.count.load(Ordering::Acquire), 1);
        assert!(observation.writer_released.load(Ordering::Acquire));
    }

    #[test]
    fn miri_binding_publication_boundary_unwind_preserves_the_unique_owner() {
        for boundary in [
            InsertBoundary::BeforeReserve,
            InsertBoundary::AfterReserve,
            InsertBoundary::BeforePublish,
            InsertBoundary::AfterPublish,
        ] {
            let registry = Arc::new(HandleRegistry::from_entropy(1, [7; 40]));
            let observation = Arc::new(DropObservation::default());
            let pending = pending(&registry, &observation);
            let (entered, reached) = mpsc::sync_channel(1);
            let (resume, paused) = mpsc::channel();
            *registry.insert_pause.lock() = Some((boundary, entered, paused));
            drop(resume);

            let result = catch_no_unwind(AssertUnwindSafe(|| pending.publish()));
            assert!(result.is_err(), "{boundary:?}");
            assert!(reached.try_recv().is_ok(), "{boundary:?}");

            if boundary == InsertBoundary::AfterPublish {
                assert_eq!(registry.len(), 1);
                assert_eq!(observation.count.load(Ordering::Acquire), 0);
            } else {
                assert_eq!(registry.len(), 0);
                assert_released(&observation);
                // A new publication can reserve the slot abandoned by unwind.
                assert!(registry.new_object(42_u32).unwrap().publish().is_ok());
            }
            registry.seal().unwrap();
            assert_released(&observation);
        }
    }

    #[test]
    fn miri_binding_publication_errors_release_writer_before_object() {
        let registry = Arc::new(HandleRegistry::from_entropy(0, [7; 40]));
        let observation = Arc::new(DropObservation::default());
        assert!(matches!(
            pending(&registry, &observation).publish(),
            Err(__err) if __err.domain_code() == Some(crate::error::DomainErrorCode::Overflow)
        ));
        assert_released(&observation);

        let registry = Arc::new(HandleRegistry::from_entropy(1, [7; 40]));
        let observation = Arc::new(DropObservation::default());
        let pending = pending(&registry, &observation);
        let (entered, reached) = mpsc::sync_channel(1);
        let (resume, paused) = mpsc::channel();
        *registry.insert_pause.lock() = Some((InsertBoundary::AfterReserve, entered, paused));
        std::thread::scope(|scope| {
            let publishing = scope.spawn(|| pending.publish());
            reached.recv().unwrap();
            registry.begin_close();
            resume.send(()).unwrap();
            assert!(matches!(publishing.join().unwrap(), Err(__err) if __err.is_closing()));
        });
        assert_eq!(registry.len(), 0);
        assert_released(&observation);
    }
}
