//! Production scoped-builder, lifetime erasure, delivery, and executor fixture.
//! Worker startup and seed object publication are outside measured cycles.

use super::{InstrumentedHandleTask, UninstrumentedHandleTask};
use crate::XllResult;
use crate::async_udf::executor::{AsyncTaskScope, HandleScopedBuilder};
use crate::async_udf::instrumentation::AsyncObservation;
use crate::async_udf::manager::AsyncManager;
use crate::cancellation::{CancellationGuarantee, CancellationSource};
use crate::execution::{CalculationId, CallId, CallMetadata, CallTimer};
use crate::generation::RuntimeGeneration;
use crate::handle::{
    ExcelHandleObject, FormulaCaller, FormulaHandleService, FormulaRevisionKey, HandleTopicKey,
    PendingHandleLease,
};
use crate::input_identity::InputFingerprint;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;
use xlfn_sys::{
    XL_ASYNC_RETURN, XLOPER12, XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value, XLRET_FAILED,
    XLRET_SUCCESS, XLTYPE_BIG_DATA, XLTYPE_BOOL,
};

const UDF_ID: &str = "BENCH.HANDLE.SCOPED";
static DELIVERIES: AtomicUsize = AtomicUsize::new(0);

struct Payload(f64);
impl ExcelHandleObject for Payload {}

struct Builder {
    pending: PendingHandleLease<Payload>,
}

impl HandleScopedBuilder<f64> for Builder {
    fn build<'generation>(
        self,
        scope: AsyncTaskScope<'generation>,
    ) -> Pin<Box<dyn Future<Output = XllResult<f64>> + Send + 'generation>> {
        let lease = self.pending.bind(scope);
        Box::pin(async move { Ok(std::hint::black_box(lease.0)) })
    }
}

unsafe extern "system" fn callback(
    function: i32,
    count: i32,
    arguments: *mut *mut XLOPER12,
    result: *mut XLOPER12,
) -> i32 {
    if function != XL_ASYNC_RETURN || count != 2 || arguments.is_null() || result.is_null() {
        return XLRET_FAILED;
    }
    // SAFETY: the production async-return wrapper supplies two initialized
    // operand pointers. The opaque handle itself is never dereferenced.
    let arguments = unsafe { std::slice::from_raw_parts(arguments, 2) };
    if arguments.iter().any(|argument| argument.is_null()) {
        return XLRET_FAILED;
    }
    // SAFETY: the callback contract supplies writable result storage.
    unsafe {
        *result = XLOPER12 {
            value: XLOPER12Value { boolean: 1 },
            xltype: XLTYPE_BOOL,
        };
    }
    DELIVERIES.fetch_add(1, Ordering::Relaxed);
    XLRET_SUCCESS
}

pub struct HandleScopedDeliveryBenchmark {
    // Drop drains async task pins before the handle arena is sealed.
    manager: AsyncManager,
    handles: FormulaHandleService,
    token: String,
    generation: RuntimeGeneration,
    instrumented: bool,
}

impl HandleScopedDeliveryBenchmark {
    pub fn new(instrumented: bool) -> Self {
        crate::module_runtime::reset_callbacks_for_test();
        // SAFETY: callback has the Excel ABI and remains process-live.
        unsafe {
            xlfn_sys::install_callback_for_abi_probe(
                callback as *const () as *mut std::ffi::c_void,
            );
        }
        let handles = FormulaHandleService::try_new(1).expect("handle service startup");
        let token = handles
            .prepare_observed(
                HandleTopicKey::Formula(FormulaRevisionKey::new(
                    FormulaCaller {
                        sheet_id: 1,
                        row: 0,
                        column: 0,
                    },
                    UDF_ID,
                    InputFingerprint::from_bytes([0; 32]),
                )),
                || Ok(Payload(42.0)),
                |_, _| Ok(()),
            )
            .expect("seed handle publication")
            .into_token();
        let manager = AsyncManager::new();
        manager.start(1).expect("scoped benchmark worker startup");
        let generation = RuntimeGeneration::new(1).unwrap();
        Self {
            manager,
            handles,
            token,
            generation,
            instrumented,
        }
    }

    pub fn run(&self, iterations: usize) {
        let before = DELIVERIES.load(Ordering::Relaxed);
        for _ in 0..iterations {
            let pending =
                crate::call::with_excel_call_scope_and_state(&self.handles, |handles, scope| {
                    handles
                        .lookup::<Payload>(scope, &self.token)
                        .expect("seed handle lookup")
                        .into_pending(self.generation)
                        .expect("task-owned object pin")
                });
            let build = Builder { pending };
            let (cancellation, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
            let mut raw = XLOPER12 {
                value: XLOPER12Value {
                    big_data: XLOPER12BigData {
                        handle: XLOPER12BigDataHandle {
                            data: std::ptr::null_mut(),
                        },
                        byte_count: 0,
                    },
                },
                xltype: XLTYPE_BIG_DATA,
            };
            // SAFETY: raw is a live, aligned, well-formed opaque async token.
            let responder = unsafe { super::ExcelAsyncResponder::from_raw(UDF_ID, &mut raw) }
                .expect("copy async responder");
            let reservation = self.manager.reserve_spawn(1).expect("task admission");
            if self.instrumented {
                let metadata = CallMetadata {
                    udf_id: UDF_ID,
                    excel_name: UDF_ID,
                    call_id: CallId::new(1),
                    calculation_id: CalculationId::new(1),
                    started_at: SystemTime::UNIX_EPOCH,
                    concurrent_calls: 1,
                };
                let observation =
                    AsyncObservation::new(&metadata, CallTimer::start(), Some(()), false, token);
                reservation.commit_handle_scoped(
                    self.generation,
                    InstrumentedHandleTask {
                        build,
                        responder,
                        token,
                        observation,
                        udf_id: UDF_ID,
                        _result: std::marker::PhantomData,
                    },
                    cancellation,
                );
            } else {
                reservation.commit_handle_scoped(
                    self.generation,
                    UninstrumentedHandleTask {
                        build,
                        responder,
                        token,
                        udf_id: UDF_ID,
                        _result: std::marker::PhantomData,
                    },
                    cancellation,
                );
            }
        }
        assert!(self.manager.wait_idle(), "scoped tasks drain");
        assert_eq!(DELIVERIES.load(Ordering::Relaxed) - before, iterations);
    }
}

impl Drop for HandleScopedDeliveryBenchmark {
    fn drop(&mut self) {
        assert!(self.manager.close().issues.is_empty());
        self.handles.terminate_all_topics();
        self.handles
            .seal()
            .expect("handle service shutdown after task drain");
    }
}
