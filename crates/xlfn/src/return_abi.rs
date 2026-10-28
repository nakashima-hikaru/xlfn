#[cfg(test)]
use crate::call_return::ReturnPayload;
#[cfg(test)]
use crate::call_return::{ExcelReturn, ReturnContext};
use crate::execution::{CallId, CallMetadata, CallOutcome};
#[cfg(test)]
use crate::execution::{UdfCompletionOutcome, UdfDeliveryOutcome, UdfErrorKind};
use crate::panic_boundary::catch_no_unwind;
use crate::runtime::Runtime;
#[cfg(test)]
use crate::value::ExcelCellOutput;
use crate::{XllError, XllResult};
use std::cell::{Cell, UnsafeCell};
use std::mem::MaybeUninit;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use xlfn_kernel::published_owner::PublishedOwner;

#[cfg(test)]
use std::sync::atomic::Ordering;
#[cfg(test)]
use xlfn_sys::XLTYPE_STR;
use xlfn_sys::{XLBIT_DLL_FREE, XLOPER12, XLOPER12Array, XLOPER12Value, XLTYPE_MULTI};

pub(crate) mod array;
pub(crate) mod conversion;
mod detached_scalar;
pub(crate) mod metadata;
pub(crate) mod numeric;
pub(crate) mod ownership;
pub(crate) mod scalar;
pub(crate) mod storage;

pub(crate) use conversion::{CallbackCleanupDebt, ExcelCallbackStatus};
pub(crate) use ownership::ReturnFreeBoundaryGuard;
pub(crate) use ownership::{ReturnFreeGuard, ReturnObligation, ReturnProducerGuard, ReturnTracker};

pub use array::{XlArrayBuilder, XlArrayOutput};
pub use numeric::{
    MAX_MODULE_NUMERIC_RETURN_BYTES, MAX_NUMERIC_RETURN_BYTES, MAX_PROCESS_NUMERIC_RETURN_BYTES,
    MAX_THREAD_NUMERIC_RETURN_BYTES, NumericArrayBuilder, NumericArrayOutput,
    NumericReturnMemoryMetrics, numeric_return_memory_metrics,
};
pub use scalar::XlScalarOutput;
pub(crate) use storage::ReturnStorage;

const RETURN_MAGIC: u64 = 0x584c_4c52_4554_3132;
const MAX_RETURN_BYTES: usize = core::cfg_select! {
    target_pointer_width = "32" => 64 * 1024 * 1024,
    _ => 256 * 1024 * 1024,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReturnBlockBacking {
    ThreadLocal,
    Heap,
}

/// String ownership is selected once when a completed scalar or array is
/// published. Array builders retain their untagged arena on the per-cell path.
#[derive(Debug)]
enum ReturnStringStorage {
    Array(ReturnStorage),
    Scalar(storage::ScalarStringStorage),
}

#[repr(C)]
struct ReturnBlock {
    // This must remain first: Excel receives a pointer to this field and
    // xlAutoFree12 casts it back to ReturnBlock.
    oper: XLOPER12,
    storage: Option<ReturnStringStorage>,
    array: Option<PublishedOwner<[XLOPER12]>>,
    obligation: Option<ReturnObligation<'static>>,
    magic: u64,
    backing: ReturnBlockBacking,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReturnBlockSlotState {
    Vacant,
    Occupied,
    Poisoned,
}

struct ReturnBlockSlot {
    state: Cell<ReturnBlockSlotState>,
    block: UnsafeCell<MaybeUninit<ReturnBlock>>,
}

impl ReturnBlockSlot {
    const fn new() -> Self {
        Self {
            state: Cell::new(ReturnBlockSlotState::Vacant),
            block: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    fn try_acquire(&self) -> Option<NonNull<ReturnBlock>> {
        if self.state.get() != ReturnBlockSlotState::Vacant {
            return None;
        }

        self.state.set(ReturnBlockSlotState::Occupied);
        // SAFETY: Vacant is the only state in which the slot may be acquired,
        // so no initialized ReturnBlock is being aliased or overwritten.
        let raw_block = unsafe { &mut *self.block.get() };
        let block_ptr = raw_block.as_mut_ptr();
        // SAFETY: pointer to internal UnsafeCell storage is non-null.
        Some(unsafe { NonNull::new_unchecked(block_ptr) })
    }

    fn owns(&self, pointer: *mut ReturnBlock) -> bool {
        self.state.get() == ReturnBlockSlotState::Occupied
            && self.block.get().cast::<ReturnBlock>() == pointer
    }

    /// Marks the slot available after its initialized value has been dropped.
    ///
    /// # Safety
    ///
    /// `pointer` must be the pointer returned by `try_acquire`, and the value
    /// at that pointer must already have been destroyed.
    unsafe fn release(&self, pointer: *mut ReturnBlock) {
        debug_assert!(self.owns(pointer));
        self.state.set(ReturnBlockSlotState::Vacant);
    }

    fn poison(&self) {
        debug_assert_eq!(self.state.get(), ReturnBlockSlotState::Occupied);
        self.state.set(ReturnBlockSlotState::Poisoned);
    }
}

// The slot contains only non-owning storage. Its TLS value must never run a
// destructor while a cdylib is being unloaded.
const _: () = assert!(!std::mem::needs_drop::<ReturnBlockSlot>());

thread_local! {
    static RETURN_BLOCK_SLOT: ReturnBlockSlot = const { ReturnBlockSlot::new() };
}

#[derive(Debug)]
#[doc(hidden)]
pub struct PreparedReturn {
    oper: XLOPER12,
    storage: Option<ReturnStringStorage>,
    array: Option<PublishedOwner<[XLOPER12]>>,
}

impl PreparedReturn {
    fn encode(value: impl crate::call_return::EncodedReturn) -> XllResult<Self> {
        value.prepare()
    }

    pub(crate) fn from_scalar_output(encoded: XlScalarOutput) -> Self {
        Self {
            oper: encoded.oper,
            storage: encoded.storage.map(ReturnStringStorage::Scalar),
            array: None,
        }
    }

    pub(crate) fn from_array_output(encoded: XlArrayOutput) -> XllResult<Self> {
        let total_bytes = encoded
            .payload_bytes
            .checked_add(std::mem::size_of::<ReturnBlock>())
            .ok_or(XllError::Domain {
                code: crate::error::DomainErrorCode::Overflow,
            })?;

        enforce_return_limit(total_bytes)?;

        let rows = i32::try_from(encoded.rows).map_err(|_| XllError::Domain {
            code: crate::error::DomainErrorCode::Overflow,
        })?;

        let columns = i32::try_from(encoded.columns).map_err(|_| XllError::Domain {
            code: crate::error::DomainErrorCode::Overflow,
        })?;

        // Establish the non-retagging owner before deriving the ABI pointer:
        // PreparedReturn and ReturnBlock both move after cells are published.
        let cells = PublishedOwner::from_box(encoded.cells);
        let storage = encoded.storage.map(ReturnStringStorage::Array);
        let pointer = cells.as_ptr().cast::<XLOPER12>();

        Ok(Self {
            oper: XLOPER12 {
                value: XLOPER12Value {
                    array: XLOPER12Array {
                        values: pointer,
                        rows,
                        columns,
                    },
                },
                xltype: XLTYPE_MULTI,
            },
            storage,
            array: Some(cells),
        })
    }

    fn error(error: &XllError) -> Self {
        Self {
            oper: XLOPER12::error(error.excel_error().code()),
            storage: None,
            array: None,
        }
    }

    fn publish_excel(self, producer: &mut ReturnProducerGuard<'static>) -> ValuePublication {
        if self.storage.is_none()
            && self.array.is_none()
            && matches!(
                self.oper.base_type(),
                xlfn_sys::XLTYPE_NUM | xlfn_sys::XLTYPE_BOOL | xlfn_sys::XLTYPE_ERR
            )
        {
            // Layer exits, tracing, and call teardown may reenter a UDF. Keep
            // the primitive private until those callbacks have completed.
            return ValuePublication::Scalar(self.oper);
        }
        ValuePublication::Owned(self.publish_owned_excel(producer))
    }

    fn publish_owned_excel(mut self, producer: &mut ReturnProducerGuard<'static>) -> *mut XLOPER12 {
        let obligation = producer.transfer_to_block();
        self.oper.xltype |= XLBIT_DLL_FREE;

        let block = ReturnBlock {
            oper: self.oper,
            storage: self.storage,
            array: self.array,
            obligation: Some(obligation),
            magic: RETURN_MAGIC,
            backing: ReturnBlockBacking::Heap,
        };

        #[cfg(test)]
        LIVE_BLOCKS.fetch_add(1, Ordering::Relaxed);

        RETURN_BLOCK_SLOT.with(|slot| {
            if let Some(block_pointer) = slot.try_acquire() {
                let mut block = block;
                block.backing = ReturnBlockBacking::ThreadLocal;
                // SAFETY: `try_acquire` reserved this slot and returned its
                // uninitialized storage for exactly one ReturnBlock value.
                unsafe { block_pointer.as_ptr().write(block) };
                block_pointer.cast::<XLOPER12>().as_ptr()
            } else {
                ReturnBlock::into_non_null(Box::new(block)).as_ptr()
            }
        })
    }
}

impl ReturnBlock {
    fn into_non_null(block: Box<Self>) -> NonNull<XLOPER12> {
        Box::into_non_null(block).cast()
    }
}

/// Total payload requested from the allocator before UTF-16 buffers are added.
/// `ReturnBlock` includes the root XLOPER12, string owner, and collection
/// control structures; the other terms cover the separately allocated array
/// slots.
fn base_allocation_payload_bytes(array_cells: usize) -> XllResult<usize> {
    array_cells
        .checked_mul(std::mem::size_of::<XLOPER12>())
        .and_then(|array_bytes| array_bytes.checked_add(std::mem::size_of::<ReturnBlock>()))
        .ok_or(XllError::Domain {
            code: crate::error::DomainErrorCode::Overflow,
        })
}

impl Drop for ReturnBlock {
    fn drop(&mut self) {
        debug_assert_eq!(self.magic, RETURN_MAGIC);
        #[cfg(test)]
        {
            if self.storage.is_some() {
                RETURN_BLOCKS_WITH_STORAGE.fetch_add(1, Ordering::Relaxed);
            }
            if self.array.is_some() {
                RETURN_BLOCKS_WITH_ARRAY.fetch_add(1, Ordering::Relaxed);
            }
            let _ = xlfn_kernel::invariant::checked_atomic_dec(&LIVE_BLOCKS);
            let payload = PANICKING_RETURN_DROP_PAYLOAD.lock().unwrap().take();
            if let Some(payload) = payload {
                std::panic::panic_any(payload);
            }
            if PANIC_ON_RETURN_BLOCK_DROP.swap(false, Ordering::SeqCst) {
                panic!("injected ReturnBlock drop panic");
            }
        }
    }
}

fn enforce_return_limit(bytes: usize) -> XllResult<()> {
    if bytes > MAX_RETURN_BYTES {
        Err(XllError::input(
            "<return>",
            crate::error::InputError::TooLarge {
                limit: MAX_RETURN_BYTES,
                actual: bytes,
            },
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn allocate_excel_owned(
    value: impl crate::call_return::EncodedReturn,
    producer: &mut ReturnProducerGuard<'static>,
) -> XllResult<*mut XLOPER12> {
    let prepared = PreparedReturn::encode(value)?;
    Ok(prepared.publish_owned_excel(producer))
}

#[cfg(test)]
fn allocate_excel_error(
    error: &XllError,
    producer: &mut ReturnProducerGuard<'static>,
) -> *mut XLOPER12 {
    PreparedReturn::error(error).publish_owned_excel(producer)
}

static CLOSING_ERROR: std::sync::OnceLock<detached_scalar::PermanentRoot> =
    std::sync::OnceLock::new();

pub(crate) fn closing_error_pointer() -> *mut XLOPER12 {
    // Return admission is already closed, so publishing another DLL-free block
    // would race the terminal drain. A permanently owned scalar has no
    // xlAutoFree12 callback and remains valid even if Excel keeps the pointer
    // until after the XLL has been unmapped.
    // Use a process-wide static singleton to prevent memory leaks on repeated late calls.
    CLOSING_ERROR
        .get_or_init(|| {
            detached_scalar::permanent(XLOPER12::error(XllError::Closing.excel_error().code()))
        })
        .pointer()
}

/// Owns a value only for the synchronous `xlAsyncReturn` callback.
///
/// Excel does not retain this root or ask `xlAutoFree12` to release it. Keep
/// the root inline and borrow its ABI representation only at delivery; owned
/// array and string buffers remain stable when this owner moves.
#[cfg(feature = "async")]
pub(crate) struct AsyncReturnValue {
    prepared: PreparedReturn,
}

#[cfg(feature = "async")]
impl AsyncReturnValue {
    pub(crate) fn from_value(value: impl crate::call_return::EncodedReturn) -> XllResult<Self> {
        PreparedReturn::encode(value).map(|prepared| Self { prepared })
    }

    pub(crate) fn error(error: &XllError) -> Self {
        Self {
            prepared: PreparedReturn::error(error),
        }
    }

    pub(crate) fn as_raw(&mut self) -> &mut XLOPER12 {
        &mut self.prepared.oper
    }
}

/// Exercises generation-owned return allocation behind its admission gates.
#[cfg(test)]
#[doc(hidden)]
#[must_use]
pub(crate) fn ffi_boundary<A, F, T>(
    runtime: &'static Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    operation: F,
) -> *mut XLOPER12
where
    A: crate::Addin,
    F: FnOnce() -> XllResult<T>,
    T: ExcelReturn<InputMode = crate::value::PlainInputMode>,
{
    let ingress = match crate::module_runtime::ingress()
        .enter_with(|| {})
        .into_admitted()
    {
        Ok(ingress) => ingress,
        Err(_) => return closing_error_pointer(),
    };
    let _external = runtime.observer().observe_external();
    let _call = match runtime.enter(&ingress) {
        Ok(call) => call,
        Err(_) => return closing_error_pointer(),
    };
    let Some(mut producer) = runtime.enter_return_producer() else {
        return closing_error_pointer();
    };
    match catch_no_unwind(AssertUnwindSafe(|| {
        let mut context = ReturnContext::new();
        let value = T::invoke(&mut context, operation)?;
        allocate_excel_owned(value, &mut producer)
    })) {
        Ok(Ok(pointer)) => pointer,
        Ok(Err(error)) => allocate_excel_error(&error, &mut producer),
        Err(_) => {
            if !producer.is_armed() {
                std::process::abort();
            }
            allocate_excel_error(&XllError::Panic, &mut producer)
        }
    }
}

/// Outermost panic boundary for void-returning `extern "system"` entry points.
///
/// Prevents panics from unwinding across the ABI boundary. Used by async UDF
/// wrappers, async calculation lifecycle exports, and similar void-returning
/// Excel callbacks.
#[doc(hidden)]
pub(crate) fn ffi_boundary_void<A: crate::Addin>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    operation: impl FnOnce(),
) {
    let ingress = match crate::module_runtime::ingress()
        .enter_with(|| {})
        .into_admitted()
    {
        Ok(ingress) => ingress,
        Err(_) => return,
    };
    let _external = runtime.observer().observe_external();
    let _call = match runtime.enter(&ingress) {
        Ok(call) => call,
        Err(_) => return,
    };
    let _ = catch_no_unwind(AssertUnwindSafe(operation));
}

/// ABI-specific publication behind the common admission and execution boundary.
trait SyncReturnAbi {
    type Payload;
    type Pointer: Copy;
    type Publication;
    const INCLUDE_EXCEL_VALUE: bool;
    fn rejected() -> Self::Pointer;
    fn publish(
        payload: Self::Payload,
        producer: &mut ReturnProducerGuard<'static>,
        numeric: &numeric::Fp12CallGuard,
    ) -> XllResult<Self::Publication>;
    fn error(error: &XllError, producer: &mut ReturnProducerGuard<'static>) -> Self::Publication;
    /// Final ABI handoff after all potentially reentrant call cleanup.
    fn finish(publication: Self::Publication) -> Self::Pointer;
}

enum ValuePublication {
    Scalar(XLOPER12),
    Owned(*mut XLOPER12),
}

struct ValueReturnAbi<P>(std::marker::PhantomData<P>);
impl<P: crate::call_return::EncodedReturn> SyncReturnAbi for ValueReturnAbi<P> {
    type Payload = P;
    type Pointer = *mut XLOPER12;
    type Publication = ValuePublication;
    const INCLUDE_EXCEL_VALUE: bool = false;
    fn rejected() -> Self::Pointer {
        closing_error_pointer()
    }
    fn publish(
        payload: P,
        producer: &mut ReturnProducerGuard<'static>,
        _: &numeric::Fp12CallGuard,
    ) -> XllResult<Self::Publication> {
        Ok(PreparedReturn::encode(payload)?.publish_excel(producer))
    }
    fn error(error: &XllError, producer: &mut ReturnProducerGuard<'static>) -> Self::Publication {
        PreparedReturn::error(error).publish_excel(producer)
    }
    fn finish(publication: Self::Publication) -> Self::Pointer {
        match publication {
            ValuePublication::Scalar(value) => detached_scalar::publish(value),
            ValuePublication::Owned(pointer) => pointer,
        }
    }
}

struct NumericReturnAbi;
impl SyncReturnAbi for NumericReturnAbi {
    type Payload = numeric::NumericArrayOutput;
    type Pointer = *mut xlfn_sys::FP12;
    type Publication = Self::Pointer;
    const INCLUDE_EXCEL_VALUE: bool = true;
    fn rejected() -> Self::Pointer {
        std::ptr::null_mut()
    }
    fn publish(
        payload: Self::Payload,
        _: &mut ReturnProducerGuard<'static>,
        numeric: &numeric::Fp12CallGuard,
    ) -> XllResult<Self::Pointer> {
        payload.publish(numeric)
    }
    fn error(_: &XllError, _: &mut ReturnProducerGuard<'static>) -> Self::Pointer {
        std::ptr::null_mut()
    }
    fn finish(publication: Self::Publication) -> Self::Pointer {
        publication
    }
}

/// Runs a generated Q UDF with common execution and shutdown protection.
#[must_use]
pub(crate) fn udf_boundary_named<A, F, P: crate::call_return::EncodedReturn>(
    runtime: &'static Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    udf_id: &'static str,
    excel_name: &'static str,
    operation: F,
) -> *mut XLOPER12
where
    A: crate::Addin,
    F: for<'call> FnOnce(
        &'call A::SharedState,
        &'call crate::runtime::CallGuard<'call, A>,
    ) -> XllResult<P>,
{
    sync_boundary::<A, ValueReturnAbi<P>, F>(runtime, udf_id, excel_name, operation)
}

/// Runs a generated K% UDF; null is Excel's #NUM! pointer result.
#[must_use]
pub(crate) fn udf_boundary_numeric_named<A, F>(
    runtime: &'static Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    udf_id: &'static str,
    excel_name: &'static str,
    operation: F,
) -> *mut xlfn_sys::FP12
where
    A: crate::Addin,
    F: for<'call> FnOnce(
        &'call A::SharedState,
        &'call crate::runtime::CallGuard<'call, A>,
    ) -> XllResult<numeric::NumericArrayOutput>,
{
    sync_boundary::<A, NumericReturnAbi, F>(runtime, udf_id, excel_name, operation)
}

fn report_sync_panic<A: crate::Addin>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    udf_id: &'static str,
    call_id: CallId,
    calculation_id: crate::execution::CalculationId,
    message: &str,
) {
    crate::diagnostics::report_udf_panic_with_history_no_unwind(
        runtime.diagnostics(),
        udf_id,
        Some(call_id),
        Some(calculation_id),
        message,
    );
}

fn report_sync_failure<A: crate::Addin, B: SyncReturnAbi>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    udf_id: &'static str,
    call_id: Option<CallId>,
    calculation_id: crate::execution::CalculationId,
    error: &XllError,
) {
    crate::diagnostics::report_udf_failure_with_history_no_unwind(
        runtime.diagnostics(),
        udf_id,
        call_id,
        Some(calculation_id),
        error,
        B::INCLUDE_EXCEL_VALUE,
    );
}

fn sync_boundary<A, B, F>(
    runtime: &'static Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    udf_id: &'static str,
    excel_name: &'static str,
    operation: F,
) -> B::Pointer
where
    A: crate::Addin,
    B: SyncReturnAbi,
    F: for<'call> FnOnce(
        &'call A::SharedState,
        &'call crate::runtime::CallGuard<'call, A>,
    ) -> XllResult<B::Payload>,
{
    // Retain nesting authority through admission, publication, and every guard's
    // teardown: teardown can itself reenter a UDF before Excel reads this result.
    let numeric = numeric::Fp12CallGuard::enter();
    let calculation_id = runtime.calculation_id();
    let ingress = match crate::module_runtime::ingress()
        .enter_udf_with(|| {})
        .into_admitted()
    {
        Ok(ingress) => ingress,
        Err(_) => {
            report_sync_failure::<A, B>(runtime, udf_id, None, calculation_id, &XllError::Closing);
            return B::rejected();
        }
    };
    let _external = runtime.observer().observe_external();
    let call = match runtime.enter(&ingress) {
        Ok(call) => call,
        Err(error) => {
            report_sync_failure::<A, B>(runtime, udf_id, None, calculation_id, &error);
            return B::rejected();
        }
    };
    let Some(mut producer) = runtime.enter_return_producer() else {
        report_sync_failure::<A, B>(runtime, udf_id, None, calculation_id, &XllError::Closing);
        return B::rejected();
    };
    // Allocate correlation even without a sink, layer, or tracing subscriber.
    let call_id = CallId::new(runtime.next_call_id());
    let result = match crate::panic_boundary::catch_udf_no_unwind(AssertUnwindSafe(|| {
        sync_boundary_inner::<A, B, F>(
            runtime,
            &call,
            &mut producer,
            &numeric,
            udf_id,
            excel_name,
            call_id,
            calculation_id,
            operation,
        )
    })) {
        Ok(pointer) => pointer,
        Err(message) => {
            if !producer.is_armed() {
                std::process::abort();
            }
            report_sync_panic(runtime, udf_id, call_id, calculation_id, &message);
            B::error(&XllError::Panic, &mut producer)
        }
    };
    drop(producer);
    drop(call);
    drop(_external);
    // Only the ingress counter and numeric nesting guards remain; their
    // destructors do not invoke application code. Retain both across handoff.
    B::finish(result)
}

#[allow(
    clippy::too_many_arguments,
    reason = "Shared UDF boundary carries ABI and invocation authority"
)]
fn sync_boundary_inner<A, B, F>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    guard: &crate::runtime::CallGuard<'_, A>,
    producer: &mut ReturnProducerGuard<'static>,
    numeric: &numeric::Fp12CallGuard,
    udf_id: &'static str,
    excel_name: &'static str,
    call_id: CallId,
    calculation_id: crate::execution::CalculationId,
    operation: F,
) -> B::Publication
where
    A: crate::Addin,
    B: SyncReturnAbi,
    F: for<'call> FnOnce(
        &'call A::SharedState,
        &'call crate::runtime::CallGuard<'call, A>,
    ) -> XllResult<B::Payload>,
{
    let instrumentation = crate::execution::InstrumentationPlan::for_call(guard);
    if !instrumentation.enabled() {
        let caught = crate::panic_boundary::catch_udf_no_unwind(AssertUnwindSafe(|| {
            B::publish(operation(guard.state(), guard)?, producer, numeric)
        }));
        let result = match caught {
            Ok(result) => result,
            Err(message) => {
                report_sync_panic(runtime, udf_id, call_id, calculation_id, &message);
                return B::error(&XllError::Panic, producer);
            }
        };
        return match result {
            Ok(pointer) => pointer,
            Err(error) => {
                report_sync_failure::<A, B>(runtime, udf_id, Some(call_id), calculation_id, &error);
                B::error(&error, producer)
            }
        };
    }
    let concurrent_calls = crate::module_runtime::ingress().active_udfs();
    let timer = crate::execution::CallTimer::start();
    let trace_metadata = crate::execution::UdfTraceMetadata {
        udf_id,
        excel_name,
        call_id,
        calculation_id,
        concurrent_calls,
    };
    let layers = if instrumentation.has_layers() {
        let metadata = CallMetadata {
            udf_id,
            excel_name,
            call_id,
            calculation_id,
            started_at: std::time::SystemTime::now(),
            concurrent_calls,
        };
        match crate::execution::enter_layers(guard.layers(), &metadata) {
            Ok(layers) => Some(layers),
            Err(error) => {
                report_sync_failure::<A, B>(runtime, udf_id, Some(call_id), calculation_id, &error);
                let outcome = CallOutcome::from_error(&error, timer.elapsed());
                if instrumentation.trace_enabled() {
                    crate::execution::trace(&trace_metadata, &outcome);
                }
                return B::error(&error, producer);
            }
        }
    } else {
        None
    };
    let caught = crate::panic_boundary::catch_udf_no_unwind(AssertUnwindSafe(|| {
        B::publish(operation(guard.state(), guard)?, producer, numeric)
    }));
    let (result, panic_reported) = match caught {
        Ok(result) => (result, false),
        Err(message) => {
            report_sync_panic(runtime, udf_id, call_id, calculation_id, &message);
            (Err(XllError::Panic), true)
        }
    };
    let outcome = match &result {
        Ok(_) => CallOutcome::success(timer.elapsed()),
        Err(error) => {
            if !panic_reported {
                report_sync_failure::<A, B>(runtime, udf_id, Some(call_id), calculation_id, error);
            }
            CallOutcome::from_error(error, timer.elapsed())
        }
    };
    if let Some(layers) = layers {
        crate::execution::exit_layers(layers, &outcome);
    }
    if instrumentation.trace_enabled() {
        crate::execution::trace(&trace_metadata, &outcome);
    }
    match result {
        Ok(pointer) => pointer,
        Err(error) => B::error(&error, producer),
    }
}

/// Frees a return block produced by an Excel-owned return boundary.
///
/// # Safety
///
/// `pointer` must be null or the exact live root returned by this crate.
/// A root with `XLBIT_DLL_FREE` must be freed exactly once. A primitive root
/// without that bit has no cleanup obligation and this function is a no-op.
pub(crate) unsafe fn free_return(pointer: *mut XLOPER12) {
    // SAFETY: caller contract guarantees pointer is a live return pointer or null.
    let operation = unsafe { enter_return_free_operation(pointer) };
    // SAFETY: caller contract guarantees pointer is a live return pointer or null.
    unsafe { free_return_block(pointer, operation.as_ref()) };
}

/// Runs DLL-owned return cleanup behind a no-unwind Excel ABI boundary.
///
/// # Safety
///
/// The pointer must satisfy `free_return`'s ownership contract.
#[must_use = "the guard must remain live until the generated xlAutoFree12 callback returns"]
pub(crate) unsafe fn free_return_boundary(pointer: *mut XLOPER12) -> ReturnFreeBoundaryGuard {
    let mut operation = None;
    let _ = catch_no_unwind(AssertUnwindSafe(|| {
        // SAFETY: caller contract guarantees pointer is a live return pointer or null.
        operation = unsafe { enter_return_free_operation(pointer) };
        // SAFETY: caller contract guarantees pointer is a live return pointer or null.
        unsafe { free_return_block(pointer, operation.as_ref()) };
    }));
    ReturnFreeBoundaryGuard {
        _operation: operation,
    }
}

#[cfg(test)]
fn is_detached_error_pointer(pointer: *mut XLOPER12) -> bool {
    CLOSING_ERROR
        .get()
        .is_some_and(|root| pointer == root.pointer())
}

unsafe fn enter_return_free_operation(pointer: *mut XLOPER12) -> Option<ReturnFreeGuard> {
    // SAFETY: The caller supplies a live root or null. Primitive roots carry
    // no free bit, and must never be interpreted as an owning ReturnBlock.
    if pointer.is_null() || unsafe { (*pointer).xltype & XLBIT_DLL_FREE == 0 } {
        return None;
    }
    let block = pointer.cast::<ReturnBlock>();
    // SAFETY: caller contract guarantees pointer points to a valid live ReturnBlock.
    let slot = unsafe { &mut (*block).obligation };
    slot.as_ref()
        .expect("Excel return obligation is taken exactly once")
        .observe_begin_free();

    Some(ReturnFreeGuard {
        obligation: slot
            .take()
            .expect("Excel return obligation is taken exactly once"),
    })
}

unsafe fn free_return_block(pointer: *mut XLOPER12, operation: Option<&ReturnFreeGuard>) {
    let Some(operation) = operation else {
        return;
    };
    let Some(pointer) = NonNull::new(pointer) else {
        return;
    };
    let mut block_pointer = pointer.cast::<ReturnBlock>();
    // SAFETY: caller contract guarantees pointer refers to a live
    // ReturnBlock produced by publish_excel or its heap fallback.
    let block = unsafe { block_pointer.as_mut() };
    debug_assert_eq!(block.magic, RETURN_MAGIC);

    debug_assert!(block.obligation.is_none());
    operation.obligation.observe_release_block();

    let backing = block.backing;
    destroy_return_block(block_pointer, backing);
}

fn destroy_return_block(pointer: NonNull<ReturnBlock>, backing: ReturnBlockBacking) {
    match backing {
        ReturnBlockBacking::Heap => {
            // SAFETY: Heap backing was created with Box::into_non_null and is
            // destroyed exactly once on this path.
            unsafe { drop(Box::from_non_null(pointer)) };
        }
        ReturnBlockBacking::ThreadLocal => destroy_thread_local_return_block(pointer.as_ptr()),
    }
}

fn destroy_thread_local_return_block(pointer: *mut ReturnBlock) {
    RETURN_BLOCK_SLOT.with(|slot| {
        // A cross-thread free remains supported defensively. In that case the
        // producer's slot cannot be touched safely from this thread, so it is
        // deliberately left occupied and future returns use heap fallback.
        let owned_by_current_thread = slot.owns(pointer);
        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: pointer is a live ReturnBlock and its backing storage is
            // still initialized until drop_in_place completes.
            unsafe { std::ptr::drop_in_place(pointer) };
        }));

        match result {
            Ok(()) => {
                if owned_by_current_thread {
                    // SAFETY: the initialized ReturnBlock was just destroyed.
                    unsafe { slot.release(pointer) };
                }
            }
            Err(payload) => {
                if owned_by_current_thread {
                    slot.poison();
                }
                std::panic::resume_unwind(payload);
            }
        }
    });
}

#[cfg(test)]
static LIVE_BLOCKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static RETURN_BLOCKS_WITH_STORAGE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static RETURN_BLOCKS_WITH_ARRAY: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static PANIC_ON_RETURN_BLOCK_DROP: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static PANICKING_RETURN_DROP_PAYLOAD: std::sync::Mutex<
    Option<crate::panic_boundary::tests::PanickingPayload>,
> = std::sync::Mutex::new(None);

#[cfg(all(feature = "async", test))]
pub(crate) fn live_return_blocks() -> usize {
    LIVE_BLOCKS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Matrix;
    use crate::{Addin, ExcelError, OpenContext};

    mod output_error_policy {
        use super::*;
        use crate::call_return::ExcelReturn;
        use crate::error::DomainErrorCode;
        use crate::value::{Column, IntoExcel, Row};

        fn assert_numeric_error(error: XllError) {
            assert!(
                matches!(
                    error,
                    XllError::Domain {
                        code: DomainErrorCode::InvalidInput
                    }
                ),
                "output errors must not acquire input-argument metadata: {error}"
            );
            assert_eq!(error.excel_error(), ExcelError::Number);
        }

        fn assert_rejected_output<T: ExcelReturn<InputMode = crate::value::PlainInputMode>>(
            value: T,
        ) {
            let result = ExcelReturn::into_excel(value, &mut ReturnContext::new())
                .and_then(PreparedReturn::encode);
            match result {
                Ok(_) => panic!("non-finite output must be rejected"),
                Err(error) => assert_numeric_error(error),
            }
        }

        fn assert_all_output_shapes<T: IntoExcel>(make: impl Fn() -> T) {
            assert_rejected_output(make());
            assert_rejected_output(Matrix::new(1, 1, vec![make()]).unwrap());
            assert_rejected_output(Row::new(vec![make()]).unwrap());
            assert_rejected_output(Column::new(vec![make()]).unwrap());
            let mut builder = XlArrayBuilder::new(1, 1).unwrap();
            assert_numeric_error(builder.push(make()).unwrap_err());
            // A rejected cell leaves the builder usable and does not consume a slot.
            builder.push(1.0).unwrap();
            assert!(builder.finish().is_ok());
        }

        #[test]
        fn non_finite_output_error_is_independent_of_shape_and_converter() {
            struct UnvalidatedNumber(f64);
            impl IntoExcel for UnvalidatedNumber {
                fn into_excel(self) -> XllResult<ExcelCellOutput> {
                    // The ABI boundary must still validate custom semantic output.
                    Ok(ExcelCellOutput::Number(self.0))
                }
            }

            for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                assert_all_output_shapes(|| number);
                assert_all_output_shapes(|| ExcelCellOutput::Number(number));
                assert_all_output_shapes(|| UnvalidatedNumber(number));
                let mut builder = XlArrayBuilder::new(1, 1).unwrap();
                assert_numeric_error(builder.push_f64(number).unwrap_err());
                builder.push_f64(1.0).unwrap();
                assert!(builder.finish().is_ok());
            }
        }
    }
    use std::sync::{Arc, Barrier, mpsc};
    use std::time::Duration;
    use xlfn_sys::{XLTYPE_ERR, XLTYPE_NUM};

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct ReturnValueTestGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        _module_lease: crate::ingress::TestModuleLease,
    }

    fn test_lock() -> ReturnValueTestGuard {
        // Return-value tests directly reset the process-global ingress state.
        // Participate in the same reentrant module lease used by Runtime tests
        // so one test cannot close another test's active opening epoch.
        let module_lease = crate::ingress::acquire_test_module_lease();
        let lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if crate::module_runtime::ingress().phase() != crate::ingress::PHASE_CLOSED {
            crate::module_runtime::ingress().begin_close_with(|| {});
            let _ = crate::module_runtime::ingress().seal_and_drain();
        }
        ReturnValueTestGuard {
            _lock: lock,
            _module_lease: module_lease,
        }
    }

    fn open_static_test_runtime() -> crate::runtime::StaticTestRuntime<()> {
        open_test_runtime(crate::runtime::StaticTestRuntime::new())
    }

    fn open_test_runtime(
        fixture: crate::runtime::StaticTestRuntime<()>,
    ) -> crate::runtime::StaticTestRuntime<()> {
        let runtime = fixture.runtime();
        let open_attempt = runtime.begin_open().unwrap();
        let mut open_attempt = runtime.publish(open_attempt, (), ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        drop(open_attempt);
        fixture
    }

    #[test]
    fn miri_generated_primitive_returns_reuse_a_root_without_free_callbacks() {
        static RUNTIME: Runtime<()> = Runtime::new();
        let _test = test_lock();
        let fixture = open_test_runtime(crate::runtime::StaticTestRuntime::from_static(&RUNTIME));
        let runtime = fixture.runtime();
        let number = udf_boundary_named(runtime, "scalar", "SCALAR", |_, _| {
            ReturnPayload::scalar(-0.0)
        });
        // SAFETY: The root is the initialized result of this boundary, and
        // Excel consumes it before the next same-thread publication below.
        let value = unsafe { &*number };
        assert_eq!(value.xltype, XLTYPE_NUM);
        // SAFETY: The checked tag selects the initialized numeric field.
        let number_bits = unsafe { value.value.number }.to_bits();
        assert_eq!(number_bits, (-0.0_f64).to_bits());
        assert!(runtime.returns_are_quiescent());

        let boolean = udf_boundary_named(runtime, "scalar", "SCALAR", |_, _| {
            ReturnPayload::scalar(true)
        });
        assert_eq!(boolean, number);
        // SAFETY: This root now contains the latest boolean publication.
        let value = unsafe { &*boolean };
        assert_eq!(value.xltype, xlfn_sys::XLTYPE_BOOL);
        // SAFETY: The checked tag selects the initialized Boolean field.
        assert_eq!(unsafe { value.value.boolean }, 1);
        let error = udf_boundary_named(runtime, "scalar", "SCALAR", |_, _| {
            ReturnPayload::scalar(ExcelError::NotAvailable)
        });
        assert_eq!(error, number);
        // SAFETY: The latest publication initialized the error union member.
        let value = unsafe { &*error };
        assert_eq!(value.xltype, XLTYPE_ERR);
        // SAFETY: The checked tag selects the initialized error field.
        let error_code = unsafe { value.value.error };
        assert_eq!(error_code, ExcelError::NotAvailable.code());
        // SAFETY: This is a live scalar root. Defensive cleanup must not cast
        // the detached scalar to ReturnBlock.
        assert!(unsafe { enter_return_free_operation(error) }.is_none());
        // SAFETY: This live root carries no free bit, so cleanup is a no-op.
        unsafe { free_return(error) };
        assert!(runtime.returns_are_quiescent());
        assert_eq!(crate::boundary::host::host_auto_remove(runtime), 1);
    }

    #[test]
    fn miri_nested_udf_consumes_inner_scalar_before_outer_publication() {
        static RUNTIME: Runtime<()> = Runtime::new();
        let _test = test_lock();
        let fixture = open_test_runtime(crate::runtime::StaticTestRuntime::from_static(&RUNTIME));
        let runtime = fixture.runtime();
        let inner_address = Cell::new(0);
        let outer = udf_boundary_named(runtime, "outer", "OUTER", |_, _| {
            let inner = udf_boundary_named(runtime, "inner", "INNER", |_, _| {
                ReturnPayload::scalar(17.0)
            });
            inner_address.set(inner.addr());
            // SAFETY: The inner result is consumed while the outer operation
            // is still running, before that outer result is published.
            let inner_root = unsafe { &*inner };
            // SAFETY: The inner boundary encoded a numeric scalar.
            let inner_value = unsafe { inner_root.value.number };
            assert_eq!(inner_value, 17.0);
            ReturnPayload::scalar(inner_value + 25.0)
        });
        assert_eq!(outer.addr(), inner_address.get());
        // SAFETY: The outer result is now the latest initialized publication.
        let value = unsafe { &*outer };
        // SAFETY: The outer boundary encoded a numeric scalar.
        assert_eq!(unsafe { value.value.number }, 42.0);
        assert!(runtime.returns_are_quiescent());
        assert_eq!(crate::boundary::host::host_auto_remove(runtime), 1);
    }

    #[test]
    fn scalar_publication_survives_layer_exit_reentry() {
        struct ReentrantAddin;
        struct ReentrantLayer;
        struct ReentrantGuard(bool);
        static RUNTIME: Runtime<ReentrantAddin> = Runtime::new();
        static REENTRIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        impl crate::execution::UdfLayer for ReentrantLayer {
            type Guard = ReentrantGuard;
            fn enter(&self, metadata: &CallMetadata) -> XllResult<Self::Guard> {
                Ok(ReentrantGuard(metadata.udf_id == "outer"))
            }
        }
        impl crate::execution::UdfLayerGuard for ReentrantGuard {
            fn exit(self, _: &CallOutcome<'_>) {
                if self.0 {
                    let inner = udf_boundary_named(&RUNTIME, "inner", "INNER", |_, _| {
                        ReturnPayload::scalar(17.0)
                    });
                    // SAFETY: consume the inner numeric result before another UDF call.
                    let inner = unsafe { &*inner };
                    assert_eq!(inner.xltype, XLTYPE_NUM);
                    // SAFETY: the checked tag selects the numeric field.
                    assert_eq!(unsafe { inner.value.number }, 17.0);
                    REENTRIES.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        impl Addin for ReentrantAddin {
            #[cfg(feature = "rtd")]
            type RtdSubscription = ();
            #[cfg(feature = "rtd")]
            type RtdSources = crate::DynamicRtdSources<Self::RtdSubscription>;
            type SharedState = ();
            type LifecycleState = ();
            type Error = XllError;
            type Layers = (ReentrantLayer,);
            #[cfg(feature = "async")]
            type AsyncExecutor = crate::NoAsyncExecutor;

            fn open(
                _: &OpenContext<Self>,
            ) -> XllResult<crate::addin::Opened<(), (), Self::Layers>> {
                unreachable!()
            }
        }

        let _test = test_lock();
        let fixture = crate::runtime::StaticTestRuntime::from_static(&RUNTIME);
        let runtime = fixture.runtime();
        let opening = runtime.begin_open().unwrap();
        let mut opening = runtime.publish(opening, (), (ReentrantLayer,));
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        drop(opening);
        REENTRIES.store(0, Ordering::Relaxed);

        for value in [
            ExcelCellOutput::Number(42.0),
            ExcelCellOutput::Boolean(true),
            ExcelCellOutput::Error(ExcelError::NotAvailable),
        ] {
            let outer = udf_boundary_named(runtime, "outer", "OUTER", |_, _| {
                ReturnPayload::scalar(value.clone())
            });
            // SAFETY: the boundary returned this live, initialized scalar root.
            let oper = unsafe { &*outer };
            assert_eq!(oper.xltype & XLBIT_DLL_FREE, 0);
            match value {
                ExcelCellOutput::Number(number) => {
                    assert_eq!(oper.xltype, XLTYPE_NUM);
                    // SAFETY: the checked tag selects the numeric field.
                    assert_eq!(unsafe { oper.value.number }, number);
                }
                ExcelCellOutput::Boolean(boolean) => {
                    assert_eq!(oper.xltype, xlfn_sys::XLTYPE_BOOL);
                    // SAFETY: the checked tag selects the Boolean field.
                    assert_eq!(unsafe { oper.value.boolean }, i32::from(boolean));
                }
                ExcelCellOutput::Error(error) => {
                    assert_eq!(oper.xltype, XLTYPE_ERR);
                    // SAFETY: the checked tag selects the error field.
                    assert_eq!(unsafe { oper.value.error }, error.code());
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(REENTRIES.load(Ordering::Relaxed), 3);
        assert!(runtime.returns_are_quiescent());
        assert_eq!(crate::boundary::host::host_auto_remove(runtime), 1);
    }

    #[test]
    fn scalar_publication_survives_tracing_reentry() {
        static RUNTIME: Runtime<()> = Runtime::new();
        struct ReentrantSubscriber(Arc<std::sync::atomic::AtomicUsize>);
        impl tracing::Subscriber for ReentrantSubscriber {
            fn register_callsite(
                &self,
                _: &'static tracing::Metadata<'static>,
            ) -> tracing::subscriber::Interest {
                tracing::subscriber::Interest::sometimes()
            }
            fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
                metadata.target() == crate::execution::UDF_TRACE_TARGET
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
            fn event(&self, _: &tracing::Event<'_>) {
                if self.0.load(Ordering::Relaxed) != 0 {
                    return;
                }
                self.0.store(1, Ordering::Relaxed);
                let inner = udf_boundary_named(&RUNTIME, "inner", "INNER", |_, _| {
                    ReturnPayload::scalar(17.0)
                });
                // SAFETY: consume the inner numeric result before another UDF call.
                let inner = unsafe { &*inner };
                assert_eq!(inner.xltype, XLTYPE_NUM);
                // SAFETY: the checked tag selects the numeric field.
                assert_eq!(unsafe { inner.value.number }, 17.0);
                self.0.store(2, Ordering::Relaxed);
            }
        }

        let _test = test_lock();
        let fixture = open_test_runtime(crate::runtime::StaticTestRuntime::from_static(&RUNTIME));
        let reentries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let outer =
            tracing::subscriber::with_default(ReentrantSubscriber(Arc::clone(&reentries)), || {
                udf_boundary_named(fixture.runtime(), "outer", "OUTER", |_, _| {
                    ReturnPayload::scalar(42.0)
                })
            });
        assert_eq!(reentries.load(Ordering::Relaxed), 2);
        // SAFETY: the boundary returned a live, initialized numeric root.
        let outer = unsafe { &*outer };
        assert_eq!(outer.xltype, XLTYPE_NUM);
        // SAFETY: the checked tag selects the numeric field.
        assert_eq!(unsafe { outer.value.number }, 42.0);
        assert!(fixture.runtime().returns_are_quiescent());
        assert_eq!(
            crate::boundary::host::host_auto_remove(fixture.runtime()),
            1
        );
    }

    #[test]
    fn miri_primitive_roots_remain_readable_after_thread_exit_and_terminal_close() {
        static RUNTIME: Runtime<()> = Runtime::new();
        struct JoinedRoot(*mut XLOPER12);
        // SAFETY: This test never reads a transferred root until the producing
        // thread has joined, and that thread can no longer rewrite it. The
        // pointee remains allocated independently of the terminated thread.
        unsafe impl Send for JoinedRoot {}

        let _test = test_lock();
        let fixture = open_test_runtime(crate::runtime::StaticTestRuntime::from_static(&RUNTIME));
        let runtime = fixture.runtime();
        let (sender, receiver) = mpsc::channel();
        std::thread::scope(|scope| {
            for value in [11.0, 22.0] {
                let sender = sender.clone();
                scope.spawn(move || {
                    let root = udf_boundary_named(runtime, "thread", "THREAD", |_, _| {
                        ReturnPayload::scalar(value)
                    });
                    sender.send((JoinedRoot(root), value)).unwrap();
                });
            }
        });
        drop(sender);
        let roots: Vec<_> = receiver.into_iter().collect();
        assert_ne!(roots[0].0.0, roots[1].0.0);
        assert!(runtime.returns_are_quiescent());
        assert_eq!(crate::boundary::host::host_auto_remove(runtime), 1);
        for (pointer, expected) in roots {
            // SAFETY: These roots own process-lifetime memory independent of
            // both the terminated threads and the removed runtime generation.
            let root = unsafe { &*pointer.0 };
            assert_eq!(root.xltype, XLTYPE_NUM);
            // SAFETY: The checked tag selects the initialized number field.
            assert_eq!(unsafe { root.value.number }, expected);
        }
    }

    #[test]
    fn generated_owned_returns_still_hold_cleanup_obligations() {
        static RUNTIME: Runtime<()> = Runtime::new();
        let _test = test_lock();
        let fixture = open_test_runtime(crate::runtime::StaticTestRuntime::from_static(&RUNTIME));
        let runtime = fixture.runtime();
        let root = udf_boundary_named(runtime, "text", "TEXT", |_, _| {
            ReturnPayload::scalar("owned text")
        });
        // SAFETY: This returned root owns counted UTF-16 storage.
        assert_ne!(unsafe { (*root).xltype } & XLBIT_DLL_FREE, 0);
        assert!(!runtime.returns_are_quiescent());
        // SAFETY: Excel would return this live owned root exactly once.
        unsafe { free_return(root) };
        assert!(runtime.returns_are_quiescent());
        assert_eq!(crate::boundary::host::host_auto_remove(runtime), 1);
    }

    #[test]
    fn miri_primitive_root_survives_the_physical_unload_release_path() {
        struct EmptyPhysicalAddin;
        impl Addin for EmptyPhysicalAddin {
            #[cfg(feature = "rtd")]
            type RtdSubscription = ();
            #[cfg(feature = "rtd")]
            type RtdSources = crate::DynamicRtdSources<Self::RtdSubscription>;
            type SharedState = ();
            type LifecycleState = ();
            type Error = XllError;
            type Layers = ();
            #[cfg(feature = "async")]
            type AsyncExecutor = crate::NoAsyncExecutor;

            fn open(_: &OpenContext<Self>) -> crate::OpenResult<Self> {
                Ok(crate::Opened::new(()))
            }
        }
        // SAFETY: This fixture has no application state or executable sources.
        unsafe impl crate::PhysicallyUnloadableAddin for EmptyPhysicalAddin {}

        let _test = test_lock();
        static RUNTIME: Runtime<EmptyPhysicalAddin, crate::runtime::PhysicalUnload> =
            Runtime::new_with_physical_unload();
        let runtime = &RUNTIME;
        fn residency_probe_anchor() {}
        runtime
            .ensure_module_residency(residency_probe_anchor as *const ())
            .unwrap();
        let opening = runtime.begin_open().unwrap();
        let mut opening = runtime.publish(opening, (), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        drop(opening);
        let root = udf_boundary_named(runtime, "physical", "PHYSICAL", |_, _| {
            ReturnPayload::scalar(42.0)
        });
        assert!(runtime.module_residency_held());
        assert!(runtime.returns_are_quiescent());
        assert_eq!(crate::boundary::host::host_auto_remove(runtime), 1);
        assert_eq!(crate::boundary::host::host_auto_close(runtime), 1);
        assert!(!runtime.module_residency_held());
        // SAFETY: The primitive lives in process-owned memory. This test
        // exercises residency release, not actual test-binary unmapping.
        let value = unsafe { &*root };
        assert_eq!(value.xltype, XLTYPE_NUM);
        // SAFETY: The checked tag selects the initialized numeric field.
        assert_eq!(unsafe { value.value.number }, 42.0);
    }

    fn allocate_excel_for_test(
        runtime: &'static crate::runtime::Runtime<()>,
        value: impl crate::call_return::EncodedReturn,
    ) -> XllResult<*mut XLOPER12> {
        let ingress = crate::module_runtime::ingress()
            .enter_with(|| {})
            .into_admitted()
            .map_err(|_| XllError::Closing)?;
        let _call = runtime.enter(&ingress)?;
        let mut producer = runtime.enter_return_producer().ok_or(XllError::Closing)?;
        allocate_excel_owned(value, &mut producer)
    }

    fn backing_of(pointer: *mut XLOPER12) -> ReturnBlockBacking {
        // SAFETY: callers pass a live pointer returned by this module.
        unsafe { (*pointer.cast::<ReturnBlock>()).backing }
    }

    #[test]
    fn oper_is_the_first_field_and_is_freed_once() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        assert!(runtime.returns_are_quiescent());
        let pointer = ffi_boundary(runtime, || Ok(42.0));
        assert!(!pointer.is_null());
        // SAFETY: pointer is the live return from ffi_boundary.
        assert_eq!(unsafe { (*pointer).base_type() }, XLTYPE_NUM);
        // Excel-owned returns carry the free bit and are registered with the
        // runtime before the producer is released.
        // SAFETY: pointer remains the live return from ffi_boundary.
        assert_ne!(unsafe { (*pointer).xltype } & XLBIT_DLL_FREE, 0);
        assert!(
            !runtime.returns_are_quiescent(),
            "Excel-owned return must keep a runtime-local return obligation live"
        );
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
        assert!(
            runtime.returns_are_quiescent(),
            "free_return must release the runtime-local return obligation"
        );
    }

    #[test]
    fn excel_returns_reuse_tls_and_fallback_to_heap_when_occupied() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let worker = std::thread::spawn(move || {
            let first = ffi_boundary(runtime, || Ok(1.0));
            assert_eq!(backing_of(first), ReturnBlockBacking::ThreadLocal);

            let second = ffi_boundary(runtime, || Ok(2.0));
            assert_eq!(backing_of(second), ReturnBlockBacking::Heap);

            // SAFETY: `second` is a live Excel-owned return from this thread.
            unsafe { free_return(second) };
            // SAFETY: `first` is a live Excel-owned return from this thread.
            unsafe { free_return(first) };

            let reused = ffi_boundary(runtime, || Ok(3.0));
            assert_eq!(backing_of(reused), ReturnBlockBacking::ThreadLocal);
            assert_eq!(reused, first);

            // SAFETY: `reused` is the live return produced immediately above.
            unsafe { free_return(reused) };
        });
        worker.join().unwrap();
    }

    #[test]
    fn excel_return_tls_slots_are_isolated_between_threads() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let barrier = Arc::new(Barrier::new(3));
        let (pointer_tx, pointer_rx) = mpsc::channel();
        let mut workers = Vec::new();

        for value in [11.0, 22.0] {
            let barrier = Arc::clone(&barrier);
            let pointer_tx = pointer_tx.clone();
            workers.push(std::thread::spawn(move || {
                let pointer = ffi_boundary(runtime, || Ok(value));
                assert_eq!(backing_of(pointer), ReturnBlockBacking::ThreadLocal);
                pointer_tx.send(pointer.addr()).unwrap();
                barrier.wait();

                // SAFETY: this worker owns the live pointer it produced.
                unsafe { free_return(pointer) };
            }));
        }
        drop(pointer_tx);

        let first = pointer_rx.recv().unwrap();
        let second = pointer_rx.recv().unwrap();
        assert_ne!(first, second);
        barrier.wait();

        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn tls_return_drops_storage_and_array_payloads_before_reuse() {
        let _test = test_lock();
        let storage_before = RETURN_BLOCKS_WITH_STORAGE.load(Ordering::Relaxed);
        let array_before = RETURN_BLOCKS_WITH_ARRAY.load(Ordering::Relaxed);

        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let worker = std::thread::spawn(move || {
            let string_pointer = ffi_boundary(runtime, || Ok("hello".to_owned()));
            assert_eq!(backing_of(string_pointer), ReturnBlockBacking::ThreadLocal);
            // SAFETY: string_pointer is the live return produced above.
            unsafe { free_return(string_pointer) };

            let matrix = Matrix::new(1, 2, vec!["left".to_owned(), "right".to_owned()]).unwrap();
            let array_pointer = ffi_boundary(runtime, || Ok(matrix));
            assert_eq!(backing_of(array_pointer), ReturnBlockBacking::ThreadLocal);
            // SAFETY: array_pointer is the live return produced above.
            unsafe { free_return(array_pointer) };
        });
        worker.join().unwrap();

        assert!(
            RETURN_BLOCKS_WITH_STORAGE.load(Ordering::Relaxed) > storage_before,
            "TLS return cleanup must drop its string owner"
        );
        assert!(
            RETURN_BLOCKS_WITH_ARRAY.load(Ordering::Relaxed) > array_before,
            "TLS return cleanup must drop array cells"
        );
    }

    #[test]
    fn panicking_tls_drop_poison_falls_back_to_heap() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let worker = std::thread::spawn(move || {
            let pointer = ffi_boundary(runtime, || Ok(42.0));
            assert_eq!(backing_of(pointer), ReturnBlockBacking::ThreadLocal);
            PANIC_ON_RETURN_BLOCK_DROP.store(true, Ordering::SeqCst);

            // The FFI boundary catches the injected destructor panic and the
            // slot remains poisoned instead of exposing partially-dropped
            // storage to a future return.
            // SAFETY: pointer is the live return produced above.
            let free_guard = unsafe { free_return_boundary(pointer) };
            RETURN_BLOCK_SLOT.with(|slot| {
                assert_eq!(slot.state.get(), ReturnBlockSlotState::Poisoned);
            });

            let fallback = ffi_boundary(runtime, || Ok(43.0));
            assert_eq!(backing_of(fallback), ReturnBlockBacking::Heap);
            // SAFETY: fallback is the live heap-backed return produced above.
            unsafe { free_return(fallback) };
            drop(free_guard);
        });
        worker.join().unwrap();
    }

    #[test]
    fn strings_use_counted_utf16_owned_by_block() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let pointer =
            allocate_excel_for_test(runtime, ReturnPayload::scalar("日本語").unwrap()).unwrap();
        // SAFETY: pointer is live and non-null.
        let oper = unsafe { &*pointer };
        // SAFETY: pointer is live and the type selects the string member.
        let text = unsafe { oper.value.string };
        // SAFETY: the return block owns a prefix and three UTF-16 units.
        let text = unsafe { std::slice::from_raw_parts(text, 4) };
        assert_eq!(text[0], 3);
        assert_eq!(&text[1..], &[26085, 26412, 35486]);
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
    }

    #[test]
    fn miri_array_cells_remain_readable_in_tls_and_heap_returns() {
        let _test = test_lock();
        let tracker_owner = Box::into_raw(Box::new(ReturnTracker::new_closed()));
        // SAFETY: the tracker is retained until the worker joins and all
        // published return obligations have been released below.
        let tracker: &'static ReturnTracker = unsafe { &*tracker_owner };
        tracker.reopen_admission().unwrap();
        std::thread::spawn(move || {
            for homogeneous in [false, true] {
                let matrix = || {
                    let value = if homogeneous {
                        Matrix::new(1, 2, vec![1.0_f64, 2.0])
                            .unwrap()
                            .into_excel(&mut ReturnContext::new())
                            .unwrap()
                    } else {
                        let matrix = Matrix::new(
                            1,
                            2,
                            vec![
                                ExcelCellOutput::Number(17.0),
                                ExcelCellOutput::String("日本語 😀".to_owned()),
                            ],
                        )
                        .unwrap();
                        <Matrix<_> as ExcelReturn>::into_excel(matrix, &mut ReturnContext::new())
                            .unwrap()
                    };
                    let mut producer = tracker.try_enter_producer().unwrap();
                    allocate_excel_owned(value, &mut producer).unwrap()
                };
                let first = matrix();
                let second = matrix();
                assert_eq!(backing_of(first), ReturnBlockBacking::ThreadLocal);
                assert_eq!(backing_of(second), ReturnBlockBacking::Heap);
                for pointer in [second, first] {
                    // SAFETY: this thread owns both live matrix return blocks.
                    let oper = unsafe { &*pointer };
                    assert_eq!(oper.base_type(), XLTYPE_MULTI);
                    // SAFETY: the checked root type selects the array member.
                    let array = unsafe { oper.value.array };
                    assert_eq!((array.rows, array.columns), (1, 2));
                    // SAFETY: the output array contains exactly two initialized cells.
                    let cells = unsafe { std::slice::from_raw_parts(array.values, 2) };
                    assert_eq!(cells[0].base_type(), XLTYPE_NUM);
                    assert_eq!(
                        // SAFETY: the first cell's checked type selects its number.
                        unsafe { cells[0].value.number },
                        if homogeneous { 1.0 } else { 17.0 }
                    );
                    if homogeneous {
                        assert_eq!(cells[1].base_type(), XLTYPE_NUM);
                        // SAFETY: the checked type selects the second number.
                        assert_eq!(unsafe { cells[1].value.number }, 2.0);
                    } else {
                        assert_eq!(cells[1].base_type(), XLTYPE_STR);
                        // SAFETY: the second cell's checked type selects counted UTF-16.
                        let text = unsafe { cells[1].value.string };
                        // SAFETY: text starts with its initialized count prefix.
                        let length = unsafe { *text } as usize;
                        // SAFETY: the counted allocation includes its prefix unit.
                        let content = unsafe { text.add(1) };
                        // SAFETY: the arena retains all counted UTF-16 units.
                        let units = unsafe { std::slice::from_raw_parts(content, length) };
                        assert_eq!(String::from_utf16(units).unwrap(), "日本語 😀");
                    }
                    // SAFETY: the block and its cells have not been freed before.
                    unsafe { free_return(pointer) };
                }
            }
        })
        .join()
        .unwrap();
        assert!(tracker.is_quiescent());
        // SAFETY: the worker has joined and no return block or producer
        // retains the tracker; reclaim its original allocation exactly once.
        unsafe { drop(Box::from_raw(tracker_owner)) };
    }

    #[test]
    fn miri_scalar_strings_remain_readable_in_tls_and_heap_returns() {
        let _test = test_lock();
        let tracker_owner = Box::into_raw(Box::new(ReturnTracker::new_closed()));
        // SAFETY: retain this tracker until the worker joins and every
        // published return obligation has been released below.
        let tracker: &'static ReturnTracker = unsafe { &*tracker_owner };
        tracker.reopen_admission().unwrap();
        std::thread::spawn(move || {
            let publish = |source: String, borrowed: bool| {
                let payload = if borrowed {
                    let payload = ReturnPayload::scalar(source.as_str()).unwrap();
                    drop(source);
                    payload
                } else {
                    // Scalar conversion consumes and drops the UTF-8 String
                    // before returning its separate UTF-16 payload owner.
                    ReturnPayload::scalar(source).unwrap()
                };
                let mut producer = tracker.try_enter_producer().unwrap();
                allocate_excel_owned(payload, &mut producer).unwrap()
            };
            let read = |pointer: *mut XLOPER12, expected: &str| {
                // SAFETY: each call below precedes this root's single free.
                let root = unsafe { &*pointer };
                assert_eq!(root.base_type(), XLTYPE_STR);
                assert_ne!(root.xltype & XLBIT_DLL_FREE, 0);
                // SAFETY: the checked type selects this counted UTF-16 buffer.
                let text = unsafe { root.value.string };
                // SAFETY: the live scalar owner retains the initialized prefix.
                let length = usize::from(unsafe { *text });
                // SAFETY: the counted allocation includes its prefix; the
                // payload starts immediately after it, including when empty.
                let payload = unsafe { text.add(1) };
                // SAFETY: the same owner retains every counted payload unit.
                let units = unsafe { std::slice::from_raw_parts(payload, length) };
                assert_eq!(units, expected.encode_utf16().collect::<Vec<_>>());
            };
            let mut previous_tls = None;
            for (first_text, second_text) in [
                ("", "a\0z"),
                ("日本語💡", "ASCII second"),
                ("\u{800}\u{d7ff}\u{e000}\u{ffff}", "aé日💡"),
            ] {
                let first = publish(first_text.to_owned(), true);
                let second = publish(second_text.to_owned(), false);
                assert_eq!(backing_of(first), ReturnBlockBacking::ThreadLocal);
                assert_eq!(backing_of(second), ReturnBlockBacking::Heap);
                assert_ne!(first, second);
                if let Some(previous) = previous_tls {
                    assert_eq!(first, previous, "the freed TLS slot must be reused");
                }
                assert_eq!(tracker.outstanding_obligations(), 2);
                // Both source Strings have already been destroyed. Read both
                // allocations while simultaneously published, then free in
                // reverse order and verify the first allocation again.
                read(first, first_text);
                read(second, second_text);
                // SAFETY: second is the live heap root, freed exactly once.
                unsafe { free_return(second) };
                assert_eq!(tracker.outstanding_obligations(), 1);
                read(first, first_text);
                // SAFETY: first is the live TLS root, freed exactly once.
                unsafe { free_return(first) };
                assert!(tracker.is_quiescent());
                previous_tls = Some(first);
            }
        })
        .join()
        .unwrap();
        assert!(tracker.is_quiescent());
        // SAFETY: the worker has joined and all producers/blocks have drained;
        // reclaim the tracker allocation exactly once without a Runtime leak.
        unsafe { drop(Box::from_raw(tracker_owner)) };
    }

    #[test]
    fn encoded_array_buffer_is_adopted_without_copying_cells() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let mut builder = crate::return_abi::XlArrayBuilder::new(1, 2).unwrap();
        builder.push_f64(10.0).unwrap();
        builder.push_f64(20.0).unwrap();
        let encoded = builder.finish().unwrap();
        let original_cells = encoded.cells.as_ptr();
        let pointer = allocate_excel_for_test(runtime, ReturnPayload::Array(encoded)).unwrap();
        // SAFETY: pointer is live and non-null.
        let oper = unsafe { &*pointer };
        // SAFETY: pointer is a live encoded array return.
        let returned_cells = unsafe { oper.value.array.values };
        assert_eq!(returned_cells.cast_const(), original_cells);
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
    }

    #[test]
    fn return_limit_accounts_for_all_owned_allocation_payloads() {
        assert_eq!(
            base_allocation_payload_bytes(0).unwrap(),
            std::mem::size_of::<ReturnBlock>()
        );
        assert_eq!(
            base_allocation_payload_bytes(2).unwrap(),
            std::mem::size_of::<ReturnBlock>() + 2 * std::mem::size_of::<XLOPER12>()
        );
        assert!(base_allocation_payload_bytes(usize::MAX).is_err());
    }

    #[test]
    fn explicit_output_errors_are_encoded_as_not_available_errors() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let pointer = allocate_excel_for_test(
            runtime,
            ReturnPayload::scalar(ExcelCellOutput::Error(ExcelError::NotAvailable)).unwrap(),
        )
        .unwrap();
        // SAFETY: pointer is a live encoded return value.
        let oper = unsafe { &*pointer };
        assert_eq!(oper.base_type(), XLTYPE_ERR);
        // SAFETY: XLTYPE_ERR selects the error union member.
        let error = unsafe { oper.value.error };
        assert_eq!(error, ExcelError::NotAvailable.code());
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
    }

    #[test]
    fn errors_and_panics_do_not_cross_ffi() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let error_pointer = ffi_boundary(runtime, || {
            Err::<f64, _>(XllError::input("x", crate::error::InputError::NonFinite))
        });
        // SAFETY: pointer is a live encoded error.
        let oper = unsafe { &*error_pointer };
        assert_eq!(oper.base_type(), XLTYPE_ERR);
        // SAFETY: XLTYPE_ERR selects the error union member.
        let error_code = unsafe { oper.value.error };
        assert_eq!(error_code, ExcelError::Value.code());
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(error_pointer) };

        let panic_pointer = ffi_boundary(runtime, || -> XllResult<f64> { panic!("boundary test") });
        // SAFETY: pointer is a live encoded error.
        assert_eq!(unsafe { (*panic_pointer).base_type() }, XLTYPE_ERR);
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(panic_pointer) };
    }

    #[test]
    fn panicking_return_conversion_does_not_cross_ffi() {
        struct PanickingReturn;

        impl crate::call_return::ExcelReturnSealed for PanickingReturn {}

        impl ExcelReturn for PanickingReturn {
            type InputMode = crate::value::PlainInputMode;
            type Payload = ReturnPayload;

            fn into_excel<S: crate::RtdSubscription, C: crate::RtdSourceCatalog<S>>(
                self,
                _: &mut ReturnContext<'_, '_, Self::InputMode, S, C>,
            ) -> XllResult<ReturnPayload> {
                panic!("injected return conversion panic")
            }
        }

        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let pointer = ffi_boundary(runtime, || Ok(PanickingReturn));
        // SAFETY: pointer is a live encoded panic error.
        assert_eq!(unsafe { (*pointer).base_type() }, XLTYPE_ERR);
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
    }

    #[test]
    fn sync_boundaries_retain_panicking_payloads() {
        use crate::panic_boundary::tests::PanickingPayload;
        use std::sync::atomic::AtomicUsize;

        struct PanickingReturn(PanickingPayload);
        impl crate::call_return::ExcelReturnSealed for PanickingReturn {}
        impl ExcelReturn for PanickingReturn {
            type InputMode = crate::value::PlainInputMode;
            type Payload = ReturnPayload;
            fn into_excel<S: crate::RtdSubscription, C: crate::RtdSourceCatalog<S>>(
                self,
                _: &mut ReturnContext<'_, '_, Self::InputMode, S, C>,
            ) -> XllResult<ReturnPayload> {
                std::panic::panic_any(self.0)
            }
        }

        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let dropped = Arc::new(AtomicUsize::new(0));
        for pointer in [
            ffi_boundary(runtime, || -> XllResult<f64> {
                std::panic::panic_any(PanickingPayload(Arc::clone(&dropped)))
            }),
            udf_boundary_named(
                runtime,
                "payload",
                "PAYLOAD",
                |_, _| -> XllResult<ReturnPayload> {
                    std::panic::panic_any(PanickingPayload(Arc::clone(&dropped)))
                },
            ),
            ffi_boundary(runtime, || {
                Ok(PanickingReturn(PanickingPayload(Arc::clone(&dropped))))
            }),
        ] {
            // SAFETY: each boundary produced a live Excel error owned by this test.
            assert_eq!(unsafe { (*pointer).base_type() }, XLTYPE_ERR);
            // SAFETY: each distinct pointer is freed exactly once.
            unsafe { free_return(pointer) };
        }
        assert_eq!(dropped.load(Ordering::Acquire), 0);
    }

    #[test]
    fn free_boundary_retains_panicking_payload_and_poisons_tls_slot() {
        use crate::panic_boundary::tests::PanickingPayload;
        use std::sync::atomic::AtomicUsize;

        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let dropped = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&dropped);
        let worker = std::thread::spawn(move || {
            let pointer = ffi_boundary(runtime, || Ok(42.0));
            assert_eq!(backing_of(pointer), ReturnBlockBacking::ThreadLocal);
            *PANICKING_RETURN_DROP_PAYLOAD.lock().unwrap() = Some(PanickingPayload(observed));
            // SAFETY: pointer is the live return produced above.
            let free_guard = unsafe { free_return_boundary(pointer) };
            RETURN_BLOCK_SLOT.with(|slot| {
                assert_eq!(slot.state.get(), ReturnBlockSlotState::Poisoned);
            });
            let next = ffi_boundary(runtime, || Ok(43.0));
            assert_eq!(backing_of(next), ReturnBlockBacking::Heap);
            // SAFETY: next is a distinct live return and the hook was consumed.
            unsafe { free_return(next) };
            drop(free_guard);
        });
        worker.join().unwrap();
        assert_eq!(dropped.load(Ordering::Acquire), 0);
    }

    #[test]
    fn udf_guard_covers_return_conversion_and_allocation() {
        struct BlockingReturn {
            converting: mpsc::SyncSender<()>,
            release: mpsc::Receiver<()>,
        }

        impl crate::call_return::ExcelReturnSealed for BlockingReturn {}

        impl ExcelReturn for BlockingReturn {
            type InputMode = crate::value::PlainInputMode;
            type Payload = ReturnPayload;

            fn into_excel<S: crate::RtdSubscription, C: crate::RtdSourceCatalog<S>>(
                self,
                _: &mut ReturnContext<'_, '_, Self::InputMode, S, C>,
            ) -> XllResult<ReturnPayload> {
                self.converting.send(()).unwrap();
                self.release.recv().unwrap();
                ReturnPayload::scalar(1.0)
            }
        }

        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();

        let (converting_tx, converting_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let caller = std::thread::spawn(move || {
            let pointer = ffi_boundary(runtime, || {
                Ok(BlockingReturn {
                    converting: converting_tx,
                    release: release_rx,
                })
            });
            // SAFETY: this thread owns the live return pointer.
            unsafe { free_return(pointer) };
        });

        converting_rx.recv().unwrap();
        assert!(runtime.lifecycle_orchestrator().begin_close());
        crate::module_runtime::ingress().begin_close_with(|| {});
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer = std::thread::spawn(move || {
            let _ = crate::module_runtime::ingress().seal_and_drain();
            closed_tx.send(()).unwrap();
        });
        assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());
        release_tx.send(()).unwrap();
        closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        caller.join().unwrap();
        closer.join().unwrap();
    }

    #[test]
    fn close_waits_until_excel_releases_a_framework_return() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();

        let pointer = ffi_boundary(runtime, || Ok(7.0));
        assert!(!pointer.is_null());
        assert!(runtime.lifecycle_orchestrator().begin_close());

        let (drained_tx, drained_rx) = mpsc::sync_channel(1);
        let closer = std::thread::spawn(move || {
            runtime.wait_for_returns();
            drained_tx.send(()).unwrap();
        });

        assert!(drained_rx.recv_timeout(Duration::from_millis(20)).is_err());
        // SAFETY: this test owns the live pointer returned above.
        let free_operation = unsafe { free_return_boundary(pointer) };
        assert!(drained_rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(free_operation);
        drained_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        closer.join().unwrap();
    }

    #[test]
    fn closed_return_admission_never_publishes_another_dll_free_block() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        assert!(runtime.lifecycle_orchestrator().begin_close());

        let pointer = ffi_boundary(runtime, || Ok(7.0));
        assert!(!pointer.is_null());
        // SAFETY: Admission rejection returns the permanently owned detached closing
        // error singleton, which deliberately does not carry XLBIT_DLL_FREE.
        unsafe {
            assert_eq!((*pointer).xltype & xlfn_sys::XLBIT_DLL_FREE, 0);
        }
        assert!(is_detached_error_pointer(pointer));

        let second = ffi_boundary(runtime, || Ok(8.0));
        assert_eq!(pointer, second);
        assert!(is_detached_error_pointer(second));
    }

    #[test]
    fn udf_layer_sees_failures_and_call_metadata() {
        struct Recorder(Arc<std::sync::Mutex<Vec<(String, UdfErrorKind, usize)>>>);
        struct RecorderGuard {
            events: Arc<std::sync::Mutex<Vec<(String, UdfErrorKind, usize)>>>,
            udf_id: String,
            concurrent_calls: usize,
        }

        impl crate::execution::UdfLayer for Recorder {
            type Guard = RecorderGuard;

            fn enter(&self, metadata: &crate::execution::CallMetadata) -> XllResult<Self::Guard> {
                Ok(RecorderGuard {
                    events: Arc::clone(&self.0),
                    udf_id: metadata.udf_id.to_owned(),
                    concurrent_calls: metadata.concurrent_calls,
                })
            }
        }

        impl crate::execution::UdfLayerGuard for RecorderGuard {
            fn exit(self, outcome: &crate::execution::CallOutcome<'_>) {
                assert!(matches!(
                    outcome.delivery,
                    UdfDeliveryOutcome::NotApplicable
                ));
                let kind = match outcome.completion {
                    UdfCompletionOutcome::Error { kind, .. } => kind,
                    UdfCompletionOutcome::Success | UdfCompletionOutcome::Cancelled => {
                        panic!("expected synchronous UDF error")
                    }
                };
                self.events.lock().unwrap().push((
                    self.udf_id.clone(),
                    kind,
                    self.concurrent_calls,
                ));
            }
        }

        struct LayerTestAddin;
        impl Addin for LayerTestAddin {
            #[cfg(feature = "rtd")]
            type RtdSubscription = ();
            #[cfg(feature = "rtd")]
            type RtdSources = crate::DynamicRtdSources<Self::RtdSubscription>;
            type SharedState = ();
            type LifecycleState = ();
            type Error = XllError;
            type Layers = (Recorder,);
            #[cfg(feature = "async")]
            type AsyncExecutor = crate::NoAsyncExecutor;

            fn open(
                _: &OpenContext<Self>,
            ) -> Result<
                crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
                Self::Error,
            > {
                unreachable!()
            }
        }

        let _test = test_lock();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let fixture = crate::runtime::StaticTestRuntime::<LayerTestAddin>::new();
        let runtime = fixture.runtime();
        let open_attempt = runtime.begin_open().unwrap();
        let mut open_attempt = runtime.publish(open_attempt, (), (Recorder(Arc::clone(&events)),));
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        drop(open_attempt);

        let pointer = udf_boundary_named(runtime, "test_conversion", "TEST.CONVERSION", |_, _| {
            Err::<ReturnPayload, _>(XllError::input(
                "value",
                crate::error::InputError::NonFinite,
            ))
        });
        // SAFETY: this test owns the live return pointer.
        unsafe { free_return(pointer) };

        {
            let recorded = events.lock().unwrap();
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0].0, "test_conversion");
            assert_eq!(recorded[0].1, UdfErrorKind::Input);
            assert_eq!(recorded[0].2, 1);
        }

        let payload_dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let panic_pointer = udf_boundary_named(
            runtime,
            "test_panic",
            "TEST.PANIC",
            |_, _| -> XllResult<ReturnPayload> {
                std::panic::panic_any(crate::panic_boundary::tests::PanickingPayload(Arc::clone(
                    &payload_dropped,
                )))
            },
        );
        // SAFETY: this test owns the live return pointer.
        unsafe { free_return(panic_pointer) };

        let recorded = events.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[1].0, "test_panic");
        assert_eq!(recorded[1].1, UdfErrorKind::Panic);
        assert_eq!(payload_dropped.load(Ordering::Acquire), 0);
    }

    #[test]
    fn owned_values_cannot_bypass_scalar_validation() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let pointer = ffi_boundary(runtime, || Ok(f64::NAN));
        // SAFETY: pointer is a live encoded validation error.
        assert_eq!(unsafe { (*pointer).base_type() }, XLTYPE_ERR);
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
    }

    #[test]
    fn scalar_returns_do_not_evaluate_input_fingerprints() {
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let ingress = crate::module_runtime::ingress()
            .enter_with(|| {})
            .into_admitted()
            .expect("test call enters during OPEN");
        let call = runtime.enter(&ingress).unwrap();
        let callbacks = crate::host_callback::HostCallbackSession::new();
        crate::call::with_excel_callback_scope_and_call(
            &call,
            &callbacks,
            |call, scope, callbacks| {
                let mut frame =
                    crate::__private::v1::CallFrame::<crate::value::PlainInputMode>::new(
                        call, scope, callbacks, 0,
                    );
                let mut context = frame.return_context("scalar").unwrap();
                let value =
                    <f64 as crate::call_return::ExcelReturn>::invoke(&mut context, || Ok(4.5))
                        .unwrap();
                assert_eq!(
                    crate::value::XlValueRef::from_array_cell(value.as_raw())
                        .unwrap()
                        .as_f64()
                        .unwrap(),
                    4.5
                );
            },
        );
    }

    #[cfg(feature = "async")]
    #[test]
    fn async_return_value_does_not_set_xlbit_dll_free_or_create_a_return_block() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let excel_ptr = ffi_boundary(runtime, || Ok(42.0));
        let blocks = live_return_blocks();
        let mut async_value =
            AsyncReturnValue::from_value(ReturnPayload::scalar(42.0).unwrap()).unwrap();

        // SAFETY: excel_ptr is a valid ReturnBlock pointer.
        let excel_oper = unsafe { &*excel_ptr };
        let async_oper = async_value.as_raw();
        assert_ne!(excel_oper.xltype & xlfn_sys::XLBIT_DLL_FREE, 0);
        assert_eq!(async_oper.xltype & xlfn_sys::XLBIT_DLL_FREE, 0);
        assert_eq!(live_return_blocks(), blocks);
        drop(async_value);
        assert_eq!(live_return_blocks(), blocks);

        // SAFETY: excel_ptr is freed once.
        unsafe { free_return(excel_ptr) };
    }

    #[cfg(feature = "async")]
    #[test]
    fn miri_async_return_buffers_remain_live_after_moving_the_inline_root() {
        let mut builder = XlArrayBuilder::new(1, 2).unwrap();
        builder.push("日本語 💡").unwrap();
        builder.push(42.0).unwrap();
        let array =
            AsyncReturnValue::from_value(ReturnPayload::Array(builder.finish().unwrap())).unwrap();
        let string =
            AsyncReturnValue::from_value(ReturnPayload::scalar("価格 💡").unwrap()).unwrap();
        // Both the root and the arena owners move after their payload pointers
        // are encoded. Only borrow a root after it reaches its delivery site.
        let mut values = vec![array, string];
        let mut string = values.pop().unwrap();
        // SAFETY: the inline root and its owned UTF-16 storage remain live.
        let borrowed = unsafe { crate::value::XlValueRef::from_raw(string.as_raw()) }.unwrap();
        assert_eq!(
            borrowed.as_str().unwrap().try_to_string().unwrap(),
            "価格 💡"
        );

        let mut array = values.pop().unwrap();
        // SAFETY: the inline root and its owned cell/string buffers remain live.
        let borrowed = unsafe { crate::value::XlValueRef::from_raw(array.as_raw()) }.unwrap();
        let array_ref =
            <crate::value::XlArrayRef as crate::value::FromExcel>::from_excel(borrowed, "array")
                .unwrap();
        assert_eq!(
            array_ref.shape(),
            crate::error::Shape {
                rows: 1,
                columns: 2
            }
        );
        assert_eq!(
            array_ref
                .get(0, 0)
                .unwrap()
                .as_str()
                .unwrap()
                .try_to_string()
                .unwrap(),
            "日本語 💡"
        );
        assert_eq!(array_ref.get(0, 1).unwrap().as_f64().unwrap(), 42.0);
    }

    #[test]
    fn close_retains_admitted_producer_and_rejects_later_entries() {
        let tracker = ReturnTracker::new_closed();
        tracker.reopen_admission().unwrap();
        let producer = tracker.try_enter_producer().unwrap();
        tracker.close_admission();
        assert_eq!(tracker.outstanding_obligations(), 1);
        assert!(!tracker.is_quiescent());
        std::thread::scope(|scope| {
            scope
                .spawn(|| assert!(tracker.try_enter_producer().is_none()))
                .join()
                .unwrap();
        });
        drop(producer);
        tracker.wait_for_quiescence();
        assert_eq!(tracker.outstanding_obligations(), 0);
        assert!(tracker.is_quiescent());
    }

    #[test]
    fn closed_admission_rejects_all_producers() {
        let tracker = ReturnTracker::new_closed();

        std::thread::scope(|scope| {
            for _ in 0..32 {
                scope.spawn(|| {
                    for _ in 0..10_000 {
                        assert!(tracker.try_enter_producer().is_none());
                    }
                });
            }
        });

        assert!(tracker.is_quiescent());
    }

    #[test]
    fn obligation_transfer_does_not_change_count() {
        let tracker: &'static ReturnTracker = Box::leak(Box::new(ReturnTracker::new_closed()));
        tracker.reopen_admission().unwrap();

        let mut producer = tracker.try_enter_producer().unwrap();
        assert_eq!(tracker.outstanding_obligations(), 1);

        let ptr = PreparedReturn::encode(ReturnPayload::scalar(42.0).unwrap())
            .unwrap()
            .publish_owned_excel(&mut producer);
        assert_eq!(tracker.outstanding_obligations(), 1);
        drop(producer);
        assert_eq!(tracker.outstanding_obligations(), 1);

        // SAFETY: ptr is a live ReturnBlock produced by publish_excel above.
        let free_guard = unsafe { enter_return_free_operation(ptr) }.unwrap();
        assert_eq!(tracker.outstanding_obligations(), 1);

        // SAFETY: ptr is a live ReturnBlock and free_guard matches its active free operation.
        unsafe { free_return_block(ptr, Some(&free_guard)) };
        assert_eq!(tracker.outstanding_obligations(), 1);

        drop(free_guard);
        assert_eq!(tracker.outstanding_obligations(), 0);
        tracker.close_admission();
        tracker.wait_for_quiescence();
        assert!(tracker.is_quiescent());
    }

    #[test]
    fn return_obligation_can_be_released_on_another_thread() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();
        let pointer = ffi_boundary(runtime, || Ok(42.0));
        assert!(!runtime.returns_are_quiescent());
        let pointer = pointer as usize;

        let worker = std::thread::spawn(move || {
            // SAFETY: `pointer` is the live Excel-owned return produced above.
            unsafe { free_return(pointer as *mut XLOPER12) };
        });
        worker.join().unwrap();

        assert!(runtime.returns_are_quiescent());
    }

    #[test]
    fn failed_encoding_reuses_same_obligation() {
        let tracker: &'static ReturnTracker = Box::leak(Box::new(ReturnTracker::new_closed()));
        tracker.reopen_admission().unwrap();

        let mut producer = tracker.try_enter_producer().unwrap();
        assert_eq!(tracker.outstanding_obligations(), 1);

        let err_res = ReturnPayload::scalar(ExcelCellOutput::Number(f64::NAN));
        let err = match err_res {
            Ok(_) => panic!("expected encoding failure"),
            Err(e) => e,
        };
        assert_eq!(tracker.outstanding_obligations(), 1);

        let ptr = allocate_excel_error(&err, &mut producer);
        assert_eq!(tracker.outstanding_obligations(), 1);

        // SAFETY: ptr is a live error ReturnBlock produced by allocate_excel_error above.
        let free_guard = unsafe { free_return_boundary(ptr) };
        assert_eq!(tracker.outstanding_obligations(), 1);
        drop(free_guard);
        assert_eq!(tracker.outstanding_obligations(), 0);
    }

    #[test]
    fn generated_panic_error_releases_producer_without_cleanup_obligation() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();

        let ptr = udf_boundary_named(
            runtime,
            "test_panic_obligation",
            "TEST.PANIC_OBLIGATION",
            |_, _| -> XllResult<ReturnPayload> { panic!("injected UDF panic") },
        );

        let tracker = runtime.return_tracker();
        assert_eq!(tracker.outstanding_obligations(), 0);

        // SAFETY: ptr is a live process-owned panic error scalar; defensive
        // cleanup is a no-op and must not create a ReturnFree obligation.
        let free_guard = unsafe { free_return_boundary(ptr) };
        assert_eq!(tracker.outstanding_obligations(), 0);
        drop(free_guard);
        assert_eq!(tracker.outstanding_obligations(), 0);
    }

    #[test]
    fn string_backing_survives_in_matrix_return() {
        let matrix = Matrix::new(1, 2, vec!["hello".to_string(), "world".to_string()]).unwrap();
        let value =
            <Matrix<_> as ExcelReturn>::into_excel(matrix, &mut ReturnContext::new()).unwrap();
        let _: XlArrayOutput = value;

        let prepared = PreparedReturn::encode(value).unwrap();
        let cells = prepared.array.as_ref().unwrap();
        // SAFETY: cells[0] selects string member.
        let str0 = unsafe { cells[0].value.string };
        // SAFETY: cells[1] selects string member.
        let str1 = unsafe { cells[1].value.string };
        // SAFETY: str0 is valid counted UTF-16.
        let len0 = unsafe { *str0 } as usize;
        // SAFETY: str1 is valid counted UTF-16.
        let len1 = unsafe { *str1 } as usize;
        // SAFETY: str0 points to at least len0+1 units.
        let offset0 = unsafe { str0.add(1) };
        // SAFETY: str1 points to at least len1+1 units.
        let offset1 = unsafe { str1.add(1) };
        // SAFETY: offset0 has len0 initialized UTF-16 units.
        let slice0 = unsafe { std::slice::from_raw_parts(offset0, len0) };
        // SAFETY: offset1 has len1 initialized UTF-16 units.
        let slice1 = unsafe { std::slice::from_raw_parts(offset1, len1) };
        let s0 = String::from_utf16(slice0).unwrap();
        let s1 = String::from_utf16(slice1).unwrap();
        assert_eq!(s0, "hello");
        assert_eq!(s1, "world");
    }

    #[test]
    fn uninstrumented_udf_allocates_correlation_ids_for_history() {
        let _test = test_lock();
        let fixture = open_static_test_runtime();
        let runtime = fixture.runtime();

        let before = runtime.peek_next_call_id();

        for _ in 0..100 {
            let ptr = udf_boundary_named(runtime, "test_fast_path", "TEST.FAST_PATH", |_, _| {
                ReturnPayload::scalar(42.0)
            });
            // SAFETY: ptr is a live ReturnBlock produced above.
            let free_guard = unsafe { free_return_boundary(ptr) };
            drop(free_guard);
        }

        let after = runtime.peek_next_call_id();
        assert_eq!(after - before, 100);
    }
    #[test]
    fn fp12_failures_record_original_error_and_correlation_without_sink() {
        use crate::diagnostics::{DiagnosticCursor, DiagnosticErrorKind};
        static RUNTIME: Runtime<()> = Runtime::new();
        let _test = test_lock();
        let fixture = open_test_runtime(crate::runtime::StaticTestRuntime::from_static(&RUNTIME));
        let runtime = fixture.runtime();
        let errors = [
            XllError::input("spot", crate::error::InputError::NonFinite),
            XllError::ExcelValue(ExcelError::NotAvailable),
            XllError::Domain {
                code: crate::error::DomainErrorCode::InvalidInput,
            },
        ];
        for error in errors {
            let pointer =
                udf_boundary_numeric_named(runtime, "fp_failure", "FP.FAILURE", |_, _| Err(error));
            assert!(pointer.is_null());
        }
        assert!(
            udf_boundary_numeric_named(runtime, "fp_nonfinite", "FP.NONFINITE", |_, _| {
                numeric::NumericArrayOutput::from_f64(1, 1, &[f64::NAN])
            })
            .is_null()
        );
        assert!(
            udf_boundary_numeric_named(runtime, "fp_size", "FP.SIZE", |_, _| {
                numeric::NumericArrayBuilder::new(1_048_576, 4)?.finish()
            })
            .is_null()
        );
        assert!(
            udf_boundary_numeric_named(runtime, "fp_panic", "FP.PANIC", |_, _| {
                panic!("pricing failed at strike 42")
            })
            .is_null()
        );
        let page = runtime
            .diagnostics()
            .errors_since(DiagnosticCursor::START, 100);
        assert_eq!(page.errors.len(), 6);
        assert_eq!(page.missed_events, 0);
        assert_eq!(page.errors[0].argument.as_deref(), Some("spot"));
        assert_eq!(page.errors[0].excel_error, ExcelError::Value);
        assert_eq!(page.errors[1].excel_error, ExcelError::NotAvailable);
        assert_eq!(page.errors[1].error_kind, DiagnosticErrorKind::ExcelValue);
        assert_eq!(page.errors[5].error_kind, DiagnosticErrorKind::Panic);
        assert_eq!(page.errors[5].message, "pricing failed at strike 42");
        assert!(page.errors[4].message.contains("16777216"));
        for pair in page.errors.windows(2) {
            assert!(pair[0].call_id.unwrap().get() < pair[1].call_id.unwrap().get());
            assert_eq!(pair[0].calculation_id, pair[1].calculation_id);
        }
        let before = runtime.diagnostics().stats().retained_records;
        let success = udf_boundary_numeric_named(runtime, "fp_success", "FP.SUCCESS", |_, _| {
            numeric::NumericArrayOutput::from_f64(1, 1, &[42.0])
        });
        assert!(!success.is_null());
        assert_eq!(runtime.diagnostics().stats().retained_records, before);
        let q = udf_boundary_named(runtime, "q_error", "Q.ERROR", |_, _| {
            Err::<ReturnPayload, _>(XllError::ExcelValue(ExcelError::NotAvailable))
        });
        // SAFETY: q is the live scalar returned above, tagged as an error.
        let q = unsafe { &*q };
        // SAFETY: the boundary returned the error variant.
        assert_eq!(unsafe { q.value.error }, ExcelError::NotAvailable.code());
        assert_eq!(runtime.diagnostics().stats().retained_records, before);
        assert_eq!(crate::boundary::host::host_auto_remove(runtime), 1);
        assert!(
            udf_boundary_numeric_named(runtime, "closed", "CLOSED", |_, _| panic!("must not run"))
                .is_null()
        );
        let page = runtime.diagnostics().errors_since(page.next_cursor, 100);
        assert_eq!(
            page.errors.last().unwrap().error_kind,
            DiagnosticErrorKind::Closing
        );
    }

    #[test]
    fn miri_fp12_nested_in_q_and_logical_close_preserve_published_arrays() {
        static RUNTIME: Runtime<()> = Runtime::new();
        let _test = test_lock();
        let fixture = open_test_runtime(crate::runtime::StaticTestRuntime::from_static(&RUNTIME));
        let runtime = fixture.runtime();
        let pointer = Cell::new(std::ptr::null_mut::<xlfn_sys::FP12>());
        let q = udf_boundary_named(runtime, "outer_q", "OUTER.Q", |_, _| {
            let first = udf_boundary_numeric_named(runtime, "inner_k", "INNER.K", |_, _| {
                numeric::NumericArrayOutput::from_f64(1, 1, &[7.0])
            });
            let second = udf_boundary_numeric_named(runtime, "inner_k", "INNER.K", |_, _| {
                numeric::NumericArrayOutput::from_f64(1, 1, &[9.0])
            });
            // SAFETY: sibling nested calls defer reclamation until the next outer call.
            assert_eq!(unsafe { (*first).array[0] }, 7.0);
            // SAFETY: the second nested result remains published in the active epoch.
            assert_eq!(unsafe { (*second).array[0] }, 9.0);
            pointer.set(first);
            ReturnPayload::scalar(1.0)
        });
        // SAFETY: the outer root remains live, and both numeric buffers remain published.
        let q = unsafe { &*q };
        // SAFETY: the outer boundary returned a numeric scalar.
        assert_eq!(unsafe { q.value.number }, 1.0);
        // SAFETY: no subsequent outer UDF has started on this thread.
        assert_eq!(unsafe { (*pointer.get()).array[0] }, 7.0);
        assert_eq!(crate::boundary::host::host_auto_remove(runtime), 1);
        // SAFETY: logical close does not free process-owned published FP12 memory.
        assert_eq!(unsafe { (*pointer.get()).array[0] }, 7.0);
    }
    #[test]
    fn fp12_history_uses_admitted_calculation_even_if_epoch_advances() {
        static RUNTIME: Runtime<()> = Runtime::new();
        let _test = test_lock();
        let fixture = open_test_runtime(crate::runtime::StaticTestRuntime::from_static(&RUNTIME));
        let runtime = fixture.runtime();
        let entered = runtime.calculation_id();
        let pointer = udf_boundary_numeric_named(runtime, "epoch", "EPOCH", |_, _| {
            runtime.finish_calculation();
            Err::<numeric::NumericArrayOutput, _>(XllError::ExcelValue(ExcelError::NotAvailable))
        });
        assert!(pointer.is_null());
        let page = runtime
            .diagnostics()
            .errors_since(crate::diagnostics::DiagnosticCursor::START, 10);
        assert_eq!(page.errors.len(), 1);
        assert_eq!(page.errors[0].calculation_id, Some(entered));
        assert_ne!(runtime.calculation_id(), entered);
    }
}
