//! Module lifetime: residency and persistent native calculation callbacks.

use crate::sync::Mutex;
use std::mem::ManuallyDrop;

use crate::module_residency::ModuleResidencyLease;
use crate::registration::{RegistrationHost, RegistrationMutation};
use crate::{XllError, XllResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventSlot {
    Absent,
    /// Reserved before entering Excel; a panic also leaves this non-retryable.
    Registering,
    Registered(i32),
    Indeterminate,
}

struct ModuleState {
    // Dropping a runtime is not evidence that Excel detached its exports.
    // Only release(), with no retained event slot, may release this reference.
    lease: Option<ManuallyDrop<ModuleResidencyLease>>,
    ended: EventSlot,
    #[cfg(feature = "async")]
    canceled: EventSlot,
}

impl ModuleState {
    fn resident_event_count(&self) -> usize {
        let count = usize::from(self.ended != EventSlot::Absent);
        #[cfg(feature = "async")]
        let count = count + usize::from(self.canceled != EventSlot::Absent);
        count
    }

    fn event(&mut self, event: CalculationEvent) -> &mut EventSlot {
        match event {
            CalculationEvent::Ended => &mut self.ended,
            #[cfg(feature = "async")]
            CalculationEvent::Canceled => &mut self.canceled,
        }
    }
}

#[derive(Clone, Copy)]
enum CalculationEvent {
    Ended,
    #[cfg(feature = "async")]
    Canceled,
}

impl CalculationEvent {
    fn procedure(self) -> &'static str {
        match self {
            Self::Ended => "__xlfn_calculation_ended",
            #[cfg(feature = "async")]
            Self::Canceled => "__xlfn_calculation_canceled",
        }
    }

    fn selector(self) -> i32 {
        match self {
            Self::Ended => xlfn_sys::XLEVENT_CALCULATION_ENDED,
            #[cfg(feature = "async")]
            Self::Canceled => xlfn_sys::XLEVENT_CALCULATION_CANCELED,
        }
    }
}

// This code is linked into the same XLL as its generated event exports. Taking
// its address lets a direct runtime open acquire residency before entering Excel.
#[inline(never)]
fn event_residency_anchor() {}

/// Physical DLL residency is deliberately separate from logical lifecycle
/// state. Native event exports belong to the same static runtime across opens,
/// so their registrations survive generation teardown. Excel need not support
/// removing them: ingress and generation admission reject late notifications.
///
/// The lease and event slots share one lock so release cannot race with a new
/// registration reservation. No lock is held while invoking Excel.
pub(crate) struct ModuleResidency {
    state: Mutex<ModuleState>,
}

impl ModuleResidency {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(ModuleState {
                lease: None,
                ended: EventSlot::Absent,
                #[cfg(feature = "async")]
                canceled: EventSlot::Absent,
            }),
        }
    }

    pub(crate) fn ensure(&self, anchor: *const ()) -> crate::XllResult<bool> {
        let mut state = self.state.lock();
        if state.lease.is_some() {
            return Ok(false);
        }
        state.lease = Some(ManuallyDrop::new(ModuleResidencyLease::acquire(anchor)?));
        Ok(true)
    }

    /// Returns false when native callbacks still require module residency.
    pub(crate) fn release(&self) -> XllResult<bool> {
        let mut state = self.state.lock();
        if state.resident_event_count() != 0 {
            return Ok(false);
        }
        let Some(residency) = state.lease.as_mut() else {
            return Ok(true);
        };
        residency.try_release()?;
        state.lease = None;
        Ok(true)
    }

    pub(crate) fn ensure_calculation_events(
        &self,
        host: &RegistrationHost<'_>,
        has_async_functions: bool,
    ) -> XllResult<()> {
        if has_async_functions {
            #[cfg(feature = "async")]
            self.ensure_event(CalculationEvent::Canceled, |procedure, selector| {
                host.register_event(procedure, selector)
            })?;
            #[cfg(not(feature = "async"))]
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::ASYNC_FEATURE,
            });
        }
        self.ensure_event(CalculationEvent::Ended, |procedure, selector| {
            host.register_event(procedure, selector)
        })
    }

    fn ensure_event(
        &self,
        event: CalculationEvent,
        register: impl FnOnce(&'static str, i32) -> RegistrationMutation<i32>,
    ) -> XllResult<()> {
        {
            let mut state = self.state.lock();
            match *state.event(event) {
                EventSlot::Registered(_) => return Ok(()),
                EventSlot::Registering | EventSlot::Indeterminate => {
                    return Err(XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::REGISTRATION_UNKNOWN,
                    });
                }
                EventSlot::Absent => {}
            }
            if state.lease.is_none() {
                state.lease = Some(ManuallyDrop::new(ModuleResidencyLease::acquire(
                    event_residency_anchor as *const (),
                )?));
            }
            *state.event(event) = EventSlot::Registering;
        }
        let result = register(event.procedure(), event.selector());
        let mut state = self.state.lock();
        match result {
            RegistrationMutation::Applied { value, cleanup } => {
                // Even failed result cleanup cannot justify a second native
                // registration. Persist the token before returning that error.
                *state.event(event) = EventSlot::Registered(value);
                cleanup
            }
            RegistrationMutation::Rejected { error } => {
                *state.event(event) = EventSlot::Absent;
                Err(error)
            }
            RegistrationMutation::Indeterminate { error, .. } => {
                *state.event(event) = EventSlot::Indeterminate;
                Err(error)
            }
        }
    }

    /// Includes uncertain and in-flight registrations, which also require a lease.
    #[cfg(any(test, feature = "refinement", feature = "lifecycle-diagnostics"))]
    pub(crate) fn resident_event_count(&self) -> usize {
        self.state.lock().resident_event_count()
    }

    /// Persistent callbacks may outlive generation reclamation only while
    /// their code remains mapped. Admission is certified by the close protocol.
    pub(crate) fn callbacks_retained_safely(&self) -> bool {
        let state = self.state.lock();
        state.resident_event_count() == 0 || state.lease.is_some()
    }

    #[cfg(any(
        test,
        feature = "refinement",
        feature = "rtd",
        feature = "handles",
        feature = "lifecycle-diagnostics"
    ))]
    pub(crate) fn is_held(&self) -> bool {
        self.state.lock().lease.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::return_abi::ExcelCallbackStatus;

    #[test]
    fn registration_reservation_retains_code_and_reentry_cannot_duplicate_it() {
        let module = ModuleResidency::new();
        module
            .ensure_event(CalculationEvent::Ended, |procedure, selector| {
                assert_eq!(procedure, "__xlfn_calculation_ended");
                assert_eq!(selector, xlfn_sys::XLEVENT_CALCULATION_ENDED);
                assert!(module.is_held());
                assert!(!module.release().unwrap());
                assert!(
                    module
                        .ensure_event(CalculationEvent::Ended, |_, _| {
                            panic!("reentrant registration must not enter Excel twice")
                        })
                        .is_err()
                );
                RegistrationMutation::Applied {
                    value: -1_657_274_367,
                    cleanup: Ok(()),
                }
            })
            .unwrap();

        module
            .ensure_event(CalculationEvent::Ended, |_, _| {
                panic!("a later open must reuse the installed native handler")
            })
            .unwrap();
        assert_eq!(
            module.state.lock().ended,
            EventSlot::Registered(-1_657_274_367)
        );
        assert_eq!(module.resident_event_count(), 1);
        assert!(!module.release().unwrap());
    }

    #[test]
    fn applied_registration_survives_failed_result_cleanup() {
        let module = ModuleResidency::new();
        assert!(
            module
                .ensure_event(CalculationEvent::Ended, |_, _| {
                    RegistrationMutation::Applied {
                        value: 19,
                        cleanup: Err(XllError::Closing),
                    }
                })
                .is_err()
        );
        module
            .ensure_event(CalculationEvent::Ended, |_, _| {
                panic!("cleanup failure must not replay an applied registration")
            })
            .unwrap();
        assert_eq!(module.resident_event_count(), 1);
        assert!(!module.release().unwrap());
    }

    #[test]
    fn rejected_registration_can_retry_but_uncertain_registration_cannot() {
        let module = ModuleResidency::new();
        assert!(
            module
                .ensure_event(CalculationEvent::Ended, |_, _| {
                    RegistrationMutation::Rejected {
                        error: XllError::Closing,
                    }
                })
                .is_err()
        );
        assert_eq!(module.resident_event_count(), 0);
        assert!(module.release().unwrap());
        assert!(!module.is_held());

        for status in [ExcelCallbackStatus::Success, ExcelCallbackStatus::Abort] {
            let module = ModuleResidency::new();
            assert!(
                module
                    .ensure_event(CalculationEvent::Ended, |_, _| {
                        RegistrationMutation::Indeterminate {
                            status,
                            error: XllError::Closing,
                        }
                    })
                    .is_err()
            );
            assert!(
                module
                    .ensure_event(CalculationEvent::Ended, |_, _| {
                        panic!("an uncertain host mutation must never be duplicated")
                    })
                    .is_err()
            );
            assert_eq!(module.resident_event_count(), 1);
            assert!(!module.release().unwrap());
            assert!(module.callbacks_retained_safely());
        }

        module
            .ensure_event(CalculationEvent::Ended, |_, _| {
                RegistrationMutation::Applied {
                    value: 3,
                    cleanup: Ok(()),
                }
            })
            .unwrap();
        assert!(module.is_held());
        assert_eq!(module.resident_event_count(), 1);
    }

    #[test]
    fn panic_during_registration_leaves_a_non_retryable_resident_slot() {
        let module = ModuleResidency::new();
        assert!(
            crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                module.ensure_event(CalculationEvent::Ended, |_, _| panic!("registration panic"))
            }))
            .is_err()
        );
        assert!(
            module
                .ensure_event(CalculationEvent::Ended, |_, _| {
                    panic!("panic must not reset an in-flight host mutation")
                })
                .is_err()
        );
        assert_eq!(module.resident_event_count(), 1);
        assert!(!module.release().unwrap());
    }

    #[test]
    #[cfg(feature = "async")]
    fn partial_event_failure_retries_only_the_absent_selector() {
        use crate::host_callback::HostCallbackSession;
        use crate::registration::host::test_support::{CallbackScript, Reply};
        use xlfn_sys::{XL_EVENT_REGISTER, XLOPER12, XLRET_FAILED};

        let module = ModuleResidency::new();
        let script = CallbackScript::install([
            Reply::success(XL_EVENT_REGISTER, XLOPER12::integer(11)),
            Reply::success(XL_EVENT_REGISTER, XLOPER12::nil()).status(XLRET_FAILED),
            Reply::success(XL_EVENT_REGISTER, XLOPER12::integer(12)),
        ]);
        let first_call = HostCallbackSession::new();
        assert!(
            module
                .ensure_calculation_events(&RegistrationHost::new(&first_call), true)
                .is_err()
        );
        assert_eq!(module.resident_event_count(), 1);
        assert!(!module.release().unwrap());

        let retry = HostCallbackSession::new();
        module
            .ensure_calculation_events(&RegistrationHost::new(&retry), true)
            .unwrap();
        module
            .ensure_calculation_events(&RegistrationHost::new(&retry), true)
            .unwrap();
        assert_eq!(module.resident_event_count(), 2);
        let state = module.state.lock();
        assert_eq!(state.canceled, EventSlot::Registered(11));
        assert_eq!(state.ended, EventSlot::Registered(12));
        drop(state);
        script.assert_calls(&[XL_EVENT_REGISTER, XL_EVENT_REGISTER, XL_EVENT_REGISTER]);
    }
}
