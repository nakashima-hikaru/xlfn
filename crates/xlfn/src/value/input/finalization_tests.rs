//! Behavior at the consuming input-recording boundary.
#![cfg_attr(
    not(feature = "handles"),
    allow(
        clippy::drop_non_drop,
        reason = "Finalized owners have no handle-permit Drop fields in feature-off tests"
    )
)]
use super::*;
use std::cell::Cell;

#[test]
fn miri_typed_finalization_preserves_identity_and_prepared_borrow() {
    let mut text: Vec<u16> = "A\u{1f642}Z".encode_utf16().collect();
    text.insert(0, u16::try_from(text.len()).unwrap());
    let raw = XLOPER12 {
        value: xlfn_sys::XLOPER12Value {
            string: text.as_mut_ptr(),
        },
        xltype: xlfn_sys::XLTYPE_STR,
    };
    crate::call::with_excel_call_scope_and_state(&raw, |raw, scope| {
        let value = XlValueRef::from_array_cell(raw).unwrap();
        let mut eager = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
        assert_eq!(
            eager.decode::<&str>(0, "name", value).unwrap(),
            "A\u{1f642}Z"
        );
        let expected = eager.finish().unwrap().unwrap();
        let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
        let prepared = arguments.prepare::<&str>(0, "name", value).unwrap();
        let finished = arguments.finish_typed().unwrap();
        assert_eq!(finished.fingerprint(), &expected);
        // The recorder has gone, while the prepared value still borrows the
        // call's input/scratch lifetime and may be materialized afterward.
        assert_eq!(prepared.materialize().unwrap(), "A\u{1f642}Z");
        drop(finished);
    });
}

#[test]
fn miri_typed_finalization_rejects_missing_or_failed_arguments() {
    let inputs = [
        XLOPER12::number(7.0),
        XLOPER12::error(crate::ExcelError::Value.code()),
    ];
    crate::call::with_excel_call_scope_and_state(&inputs, |inputs, scope| {
        let mut incomplete = ArgumentContext::<FormulaInputMode>::from_scope(scope, 2);
        incomplete
            .decode::<f64>(0, "first", XlValueRef::from_array_cell(&inputs[0]).unwrap())
            .unwrap();
        assert!(matches!(
            incomplete.finish_typed(),
            Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
            })
        ));

        let mut failed = ArgumentContext::<FormulaInputMode>::from_scope(scope, 2);
        let prepared = failed
            .prepare::<f64>(0, "first", XlValueRef::from_array_cell(&inputs[0]).unwrap())
            .unwrap();
        let error = failed.prepare::<f64>(
            1,
            "second",
            XlValueRef::from_array_cell(&inputs[1]).unwrap(),
        );
        assert!(matches!(
            error,
            Err(XllError::ExcelValue(crate::ExcelError::Value))
        ));
        assert!(matches!(
            failed.finish_typed(),
            Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
            })
        ));
        assert_eq!(prepared.materialize().unwrap(), 7.0);
    });
}

#[test]
#[cfg(feature = "handles")]
fn miri_scope_only_formula_return_rejects_missing_publication_access() {
    let callbacks = crate::host_callback::HostCallbackSession::new();
    crate::call::with_excel_call_scope_and_state(&callbacks, |callbacks, scope| {
        let arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 0);
        let finished = arguments.finish_typed().unwrap();
        let result = finished.return_context::<(), crate::DynamicRtdSources<()>>(
            crate::call_return::ReturnFrameAccess {
                udf_id: "scope-only-formula",
                callbacks,
            },
        );
        assert!(matches!(
            result,
            Err(XllError::Input {
                reason: crate::error::InputError::Malformed(
                    "formula return requires admitted handle access"
                ),
                ..
            })
        ));
    });
}

thread_local! {
    static PREPARED_COUNTS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}
struct Observed(f64);
struct Retained(f64);
impl Drop for Retained {
    fn drop(&mut self) {
        PREPARED_COUNTS.with(|counts| {
            let (materialized, dropped) = counts.get();
            counts.set((materialized, dropped + 1));
        });
    }
}
impl<'call> super::FromExcel<'call> for Observed {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        f64::from_excel(value, argument).map(Self)
    }
}
impl ExcelInputIdentity for Observed {
    fn encode_input_identity(&self, identity: &mut InputIdentityEncoder) {
        identity.f64(self.0);
    }
}
impl<'call> PrepareExcel<'call> for Observed {
    type Prepared = Retained;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Retained> {
        let value = f64::from_excel(value, argument)?;
        identity.f64(value);
        Ok(Retained(value))
    }
    fn materialize(value: Retained) -> XllResult<Self> {
        PREPARED_COUNTS.with(|counts| {
            let (materialized, dropped) = counts.get();
            counts.set((materialized + 1, dropped));
        });
        Ok(Self(value.0))
    }
}

#[test]
fn miri_finalized_arguments_keep_prepared_capture_cleanup_with_its_owner() {
    PREPARED_COUNTS.with(|counts| counts.set((0, 0)));
    let raw = XLOPER12::number(11.0);
    crate::call::with_excel_call_scope_and_state(&raw, |raw, scope| {
        let value = XlValueRef::from_array_cell(raw).unwrap();
        let mut warm = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
        let prepared = warm.prepare::<Observed>(0, "value", value).unwrap();
        let finished = warm.finish_typed().unwrap();
        drop(finished);
        assert_eq!(PREPARED_COUNTS.with(Cell::get), (0, 0));
        drop(prepared); // warm-hit discard
        assert_eq!(PREPARED_COUNTS.with(Cell::get), (0, 1));

        let mut cold = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
        let prepared = cold.prepare::<Observed>(0, "value", value).unwrap();
        let finished = cold.finish_typed().unwrap();
        assert_eq!(prepared.materialize().unwrap().0, 11.0);
        assert_eq!(PREPARED_COUNTS.with(Cell::get), (1, 2));
        drop(finished);

        let result = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let prepared = arguments.prepare::<Observed>(0, "value", value).unwrap();
            let finished = arguments.finish_typed().unwrap();
            let _retained = (finished, prepared);
            panic!("after input finalization");
        }));
        assert!(result.is_err());
        assert_eq!(PREPARED_COUNTS.with(Cell::get), (1, 3));
    });
}

#[test]
#[cfg(feature = "handles")]
fn miri_finalized_formula_inputs_transfer_the_existing_service_permit() {
    finalized_inputs_transfer_service_permit::<FormulaInputMode>();
}

#[test]
#[cfg(feature = "handles")]
fn miri_finalized_plain_inputs_transfer_the_existing_service_permit() {
    finalized_inputs_transfer_service_permit::<PlainInputMode>();
}

#[cfg(feature = "handles")]
fn finalized_inputs_transfer_service_permit<M: InputMode>() {
    let slot = crate::handle::FormulaHandleServiceSlot::new();
    slot.arm(crate::HandleConfig::default()).unwrap();
    slot.initialize().unwrap();
    let callbacks = crate::host_callback::HostCallbackSession::new();
    crate::call::with_excel_call_scope_and_state(&(slot, callbacks), |(slot, callbacks), scope| {
        let resolver = crate::handle::FormulaHandleServiceResolver::new(slot);
        resolver.get().unwrap(); // acquire the single service permit before finish
        let mut arguments = ArgumentContext::<M>::from_handle_access(scope, resolver, 1);
        arguments.native_f64(0, "value", 5.0).unwrap();
        let finished = arguments.finish_typed().unwrap();
        std::thread::scope(|threads| {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            threads.spawn(move || {
                started_tx.send(()).unwrap();
                slot.seal(Some(crate::generation::RuntimeGeneration::new(1).unwrap()))
                    .unwrap()
                    .finish()
                    .unwrap();
                done_tx.send(()).unwrap();
            });
            started_rx.recv().unwrap();
            // Wait until sealing has withdrawn publication, so completion is
            // blocked on the retained permit rather than thread scheduling.
            while slot.read_if_ready().is_some() {
                std::thread::yield_now();
            }
            assert!(done_rx.try_recv().is_err());
            let returns = finished
                .return_context::<(), crate::DynamicRtdSources<()>>(
                    crate::call_return::ReturnFrameAccess {
                        udf_id: "permit-transfer",
                        callbacks,
                    },
                )
                .unwrap();
            assert!(done_rx.try_recv().is_err());
            drop(returns);
            done_rx.recv().unwrap();
        });
    });
}

#[test]
#[cfg(feature = "handles")]
fn miri_failed_finalization_keeps_decoded_handles_safe_through_scope_exit() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct TrackedValue(usize, Arc<AtomicUsize>);
    impl crate::handle::ExcelHandleObject for TrackedValue {}
    impl Drop for TrackedValue {
        fn drop(&mut self) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
    }

    let drops = Arc::new(AtomicUsize::new(0));
    let slot = Arc::new(crate::handle::FormulaHandleServiceSlot::new());
    slot.arm(crate::HandleConfig::default()).unwrap();
    slot.initialize().unwrap();
    let token = {
        let service = slot.read().unwrap();
        service
            .prepare_observed(
                crate::handle::test_topic_key("finalization-failure"),
                || Ok(TrackedValue(42, Arc::clone(&drops))),
                |_, _| Ok(()),
            )
            .unwrap()
            .into_token()
    };
    let mut text: Vec<u16> = token.encode_utf16().collect();
    text.insert(0, u16::try_from(text.len()).unwrap());
    let raw = XLOPER12 {
        value: xlfn_sys::XLOPER12Value {
            string: text.as_mut_ptr(),
        },
        xltype: xlfn_sys::XLTYPE_STR,
    };
    // This read owner models the admitted CallGuard that encloses production
    // CallScope construction. It must outlive every permit's maintenance tail.
    let admitted_service = slot.read().unwrap();
    let service = &*admitted_service;
    let input = (&slot, service, raw);
    std::thread::scope(|threads| {
        let (begin_tx, begin_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = threads.spawn(move || {
            begin_rx.recv().unwrap();
            // Close the actual bindings while the service storage itself is
            // still retained by the enclosing owner, as production requires.
            let _sealed = service.seal().unwrap();
            done_tx.send(()).unwrap();
        });
        crate::call::with_excel_call_scope_and_state(&input, |(slot, service, raw), scope| {
            let mut arguments = ArgumentContext::<FormulaInputMode>::from_handle_access(
                scope,
                crate::handle::FormulaHandleServiceResolver::new(slot),
                2,
            );
            let handle = arguments
                .decode::<crate::handle::Handle<'_, TrackedValue>>(
                    0,
                    "handle",
                    XlValueRef::from_array_cell(raw).unwrap(),
                )
                .unwrap();
            assert_eq!(handle.0, 42);
            // The second input was never recorded, so consuming finalization
            // fails rather than minting a return publication owner.
            assert!(arguments.finish_typed().is_err());
            begin_tx.send(()).unwrap();
            // Observe the close's admission withdrawal, not just scheduling
            // of the worker. The already admitted Handle remains readable.
            while service.lookup::<TrackedValue>(scope, &token).is_ok() {
                std::thread::yield_now();
            }
            assert!(done_rx.try_recv().is_err());
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            assert_eq!(handle.0, 42);
            drop(handle);
            // Scope admission still protects the withdrawn binding even after
            // the last Handle local drops. The external service owner remains
            // alive through both permit release and its maintenance tail.
            assert!(done_rx.try_recv().is_err());
            assert_eq!(drops.load(Ordering::SeqCst), 0);
        });
        done_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        worker.join().unwrap();
    });
    drop(admitted_service);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
