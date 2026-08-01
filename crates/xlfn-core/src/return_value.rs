use crate::{
    CallId, CallMetadata, CallOutcome, ExcelErrorValue, IntoExcelValue, OwnedExcelValue, Runtime,
    UdfResultKind, XllError, XllResult,
};
use parking_lot::{Condvar, Mutex};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use xlfn_sys::{
    XLBIT_DLL_FREE, XLOPER12, XLOPER12Array, XLOPER12Value, XLRET_ABORT, XLRET_SUCCESS,
    XLRET_UNCALCED, XLTYPE_MULTI, XLTYPE_STR,
};

/// Represents the terminal or recoverable status returned by Excel C API callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcelCallbackStatus {
    Success,
    Abort,
    Uncalced,
    Failed(i32),
}

impl ExcelCallbackStatus {
    pub fn from_raw(status: i32) -> Self {
        match status {
            XLRET_SUCCESS => Self::Success,
            XLRET_ABORT => Self::Abort,
            XLRET_UNCALCED => Self::Uncalced,
            other => Self::Failed(other),
        }
    }

    pub fn permits_callback(self) -> bool {
        !matches!(self, Self::Abort | Self::Uncalced)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Abort | Self::Uncalced)
    }

    pub fn raw_code(self) -> i32 {
        match self {
            Self::Success => XLRET_SUCCESS,
            Self::Abort => XLRET_ABORT,
            Self::Uncalced => XLRET_UNCALCED,
            Self::Failed(code) => code,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RegistrationDebt {
    pub id: u64,
    pub symbol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitCookieDebt {
    pub cookie: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RegistryKeyDebt {
    pub key_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackCleanupDebt {
    pub status: ExcelCallbackStatus,
}

#[derive(Debug, Default)]
pub struct CleanupDebtSet {
    pub registrations: Vec<RegistrationDebt>,
    pub git_cookies: Vec<GitCookieDebt>,
    pub registry_keys: Vec<RegistryKeyDebt>,
}

impl CleanupDebtSet {
    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
            && self.git_cookies.is_empty()
            && self.registry_keys.is_empty()
    }
}

#[derive(Default)]
struct ReturnTrackerState {
    producers: usize,
    blocks: usize,
    free_operations: usize,
}

/// Runtime-local accounting for code paths that can retain or re-enter the
/// XLL after a synchronous function has returned to Excel.
pub(crate) struct ReturnTracker {
    state: Mutex<ReturnTrackerState>,
    quiescent: Condvar,
}

impl ReturnTracker {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(ReturnTrackerState::default()),
            quiescent: Condvar::new(),
        }
    }

    pub(crate) fn enter_producer(self: &Arc<Self>) -> ReturnProducerGuard {
        let mut state = self.state.lock();
        state.producers = state
            .producers
            .checked_add(1)
            .expect("return producer count cannot overflow");
        drop(state);
        ReturnProducerGuard {
            tracker: Arc::clone(self),
        }
    }

    fn register_block(&self) {
        let mut state = self.state.lock();
        state.blocks = state
            .blocks
            .checked_add(1)
            .expect("return block count cannot overflow");
    }

    fn release_block(&self) {
        let mut state = self.state.lock();
        state.blocks = state
            .blocks
            .checked_sub(1)
            .expect("return block count remains balanced");
        self.quiescent.notify_all();
    }

    fn enter_free(self: &Arc<Self>) -> ReturnFreeGuard {
        let mut state = self.state.lock();
        state.free_operations = state
            .free_operations
            .checked_add(1)
            .expect("return free-operation count cannot overflow");
        drop(state);
        ReturnFreeGuard {
            tracker: Arc::clone(self),
        }
    }

    pub(crate) fn wait_for_quiescence(&self) {
        let mut state = self.state.lock();
        while state.producers != 0 || state.blocks != 0 || state.free_operations != 0 {
            self.quiescent.wait(&mut state);
        }
    }

    pub(crate) fn is_quiescent(&self) -> bool {
        let state = self.state.lock();
        state.producers == 0 && state.blocks == 0 && state.free_operations == 0
    }
}

pub(crate) struct ReturnProducerGuard {
    tracker: Arc<ReturnTracker>,
}

impl ReturnProducerGuard {
    fn tracker(&self) -> &Arc<ReturnTracker> {
        &self.tracker
    }
}

impl Drop for ReturnProducerGuard {
    fn drop(&mut self) {
        let mut state = self.tracker.state.lock();
        state.producers = state
            .producers
            .checked_sub(1)
            .expect("return producer count remains balanced");
        self.tracker.quiescent.notify_all();
    }
}

struct ReturnFreeGuard {
    tracker: Arc<ReturnTracker>,
}

/// Keeps one generated `xlAutoFree12` callback visible to terminal shutdown.
#[doc(hidden)]
pub struct ReturnFreeBoundaryGuard {
    _operation: Option<ReturnFreeGuard>,
}

impl Drop for ReturnFreeGuard {
    fn drop(&mut self) {
        let mut state = self.tracker.state.lock();
        state.free_operations = state
            .free_operations
            .checked_sub(1)
            .expect("return free-operation count remains balanced");
        self.tracker.quiescent.notify_all();
    }
}

/// Call-scoped services used by [`crate::ExcelReturn`] implementations.
#[doc(hidden)]
pub struct ReturnContext<'call> {
    runtime: Option<&'call dyn crate::value::HandleRuntimeProvider>,
    udf_id: Option<&'static str>,
    raw_arguments: Option<&'call [*mut XLOPER12]>,
    lifetime: PhantomData<Rc<()>>,
}

impl<'call> ReturnContext<'call> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            runtime: None,
            udf_id: None,
            raw_arguments: None,
            lifetime: PhantomData,
        }
    }

    #[doc(hidden)]
    /// Creates return services for one generated synchronous UDF call.
    ///
    /// # Safety
    ///
    /// Every pointer in `raw_arguments` must point to a live Excel-owned
    /// XLOPER12 for `'call`, including any nested payload selected by its type.
    pub unsafe fn for_call<S>(
        runtime: &'call Runtime<S>,
        udf_id: &'static str,
        raw_arguments: &'call [*mut XLOPER12],
    ) -> Self {
        Self {
            runtime: Some(runtime),
            udf_id: Some(udf_id),
            raw_arguments: Some(raw_arguments),
            lifetime: PhantomData,
        }
    }

    #[doc(hidden)]
    pub fn publish_new_handle<T>(
        &mut self,
        operation: impl FnOnce() -> XllResult<T>,
    ) -> XllResult<String>
    where
        T: crate::handle::ExcelHandleObject,
    {
        self.publish_handle_arc(|| operation().map(Arc::new))
    }

    #[doc(hidden)]
    pub fn publish_existing_handle<T>(
        &mut self,
        operation: impl FnOnce() -> XllResult<crate::Handle<T>>,
    ) -> XllResult<String>
    where
        T: crate::handle::ExcelHandleObject,
    {
        self.publish_handle_arc(|| operation().map(crate::Handle::into_arc))
    }

    fn publish_handle_arc<T>(
        &mut self,
        operation: impl FnOnce() -> XllResult<Arc<T>>,
    ) -> XllResult<String>
    where
        T: crate::handle::ExcelHandleObject,
    {
        let runtime = self.runtime.ok_or(crate::XllError::Internal {
            diagnostic_id: 0x4841_4e44_4354_5854,
        })?;
        let udf_id = self.udf_id.ok_or(crate::XllError::Internal {
            diagnostic_id: 0x4841_4e44_5544_4649,
        })?;
        let raw_arguments = self.raw_arguments.ok_or(crate::XllError::Internal {
            diagnostic_id: 0x4841_4e44_4449_4745,
        })?;
        // SAFETY: for_call's contract keeps every argument and nested payload
        // live for this context's lifetime.
        let argument_digest = unsafe { crate::formula_fingerprint::fingerprint(raw_arguments) }?;
        let key = crate::handle::formula_topic_key(udf_id, &argument_digest)?;
        let handles = runtime.handle_runtime()?;
        let observer_handles = Arc::clone(&handles);
        let (token, _) = handles.prepare_observed(key, operation, move |key, token| {
            crate::rtd::observe(observer_handles, key, token)
        })?;
        Ok(token)
    }
}

impl Default for ReturnContext<'_> {
    fn default() -> Self {
        Self::new()
    }
}

const RETURN_MAGIC: u64 = 0x584c_4c52_4554_3132;
#[cfg(target_pointer_width = "32")]
const MAX_RETURN_BYTES: usize = 64 * 1024 * 1024;
#[cfg(not(target_pointer_width = "32"))]
const MAX_RETURN_BYTES: usize = 256 * 1024 * 1024;

#[repr(C)]
struct ReturnBlock {
    // This must remain first: Excel receives a pointer to this field and
    // xlAutoFree12 casts it back to ReturnBlock.
    oper: XLOPER12,
    strings: Box<[Box<[u16]>]>,
    array: Option<Box<[XLOPER12]>>,
    tracker: Option<Arc<ReturnTracker>>,
    magic: u64,
}

impl ReturnBlock {
    fn build(value: OwnedExcelValue) -> XllResult<Box<Self>> {
        Self::build_with_dll_free(value, true, None)
    }

    fn build_tracked(value: OwnedExcelValue, tracker: Arc<ReturnTracker>) -> XllResult<Box<Self>> {
        Self::build_with_dll_free(value, true, Some(tracker))
    }

    #[cfg(any(feature = "async", test))]
    fn build_async(value: OwnedExcelValue) -> XllResult<Box<Self>> {
        Self::build_with_dll_free(value, false, None)
    }

    fn build_with_dll_free(
        value: OwnedExcelValue,
        dll_free: bool,
        tracker: Option<Arc<ReturnTracker>>,
    ) -> XllResult<Box<Self>> {
        debug_assert!(dll_free || tracker.is_none());
        let (array_cells, string_count) = allocation_shape(&value);
        let mut allocation_bytes = base_allocation_payload_bytes(array_cells, string_count)?;
        enforce_return_limit(allocation_bytes)?;
        let mut strings = Vec::with_capacity(string_count);
        let (mut oper, array) = match value {
            OwnedExcelValue::Matrix(matrix) => {
                let rows = i32::try_from(matrix.rows()).map_err(|_| XllError::Domain {
                    code: crate::DomainErrorCode::Overflow,
                })?;
                let columns = i32::try_from(matrix.columns()).map_err(|_| XllError::Domain {
                    code: crate::DomainErrorCode::Overflow,
                })?;
                let values = matrix.into_vec();
                let mut cells = values
                    .into_iter()
                    .map(|cell| encode_scalar(cell, &mut strings, &mut allocation_bytes))
                    .collect::<XllResult<Vec<_>>>()?
                    .into_boxed_slice();
                let pointer = cells.as_mut_ptr();
                (
                    XLOPER12 {
                        value: XLOPER12Value {
                            array: XLOPER12Array {
                                values: pointer,
                                rows,
                                columns,
                            },
                        },
                        xltype: XLTYPE_MULTI,
                    },
                    Some(cells),
                )
            }
            scalar => (
                encode_scalar(scalar, &mut strings, &mut allocation_bytes)?,
                None,
            ),
        };
        if dll_free {
            oper.xltype |= XLBIT_DLL_FREE;
        }

        #[cfg(test)]
        LIVE_BLOCKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if let Some(tracker) = tracker.as_ref() {
            tracker.register_block();
        }

        Ok(Box::new(Self {
            oper,
            strings: strings.into_boxed_slice(),
            array,
            tracker,
            magic: RETURN_MAGIC,
        }))
    }

    fn into_non_null(block: Box<Self>) -> NonNull<XLOPER12> {
        let pointer = Box::into_raw(block);
        // SAFETY: Box::into_raw always returns a non-null, properly aligned pointer.
        unsafe { NonNull::new_unchecked(pointer.cast::<XLOPER12>()) }
    }
}

fn allocation_shape(value: &OwnedExcelValue) -> (usize, usize) {
    match value {
        OwnedExcelValue::Matrix(matrix) => (
            matrix.as_slice().len(),
            matrix
                .as_slice()
                .iter()
                .filter(|cell| matches!(cell, OwnedExcelValue::String(_)))
                .count(),
        ),
        OwnedExcelValue::String(_) => (0, 1),
        _ => (0, 0),
    }
}

/// Total payload requested from the allocator before UTF-16 buffers are added.
/// `ReturnBlock` includes the root XLOPER12 and collection control structures;
/// the other terms cover the separately allocated array and string-owner slots.
fn base_allocation_payload_bytes(array_cells: usize, string_count: usize) -> XllResult<usize> {
    array_cells
        .checked_mul(std::mem::size_of::<XLOPER12>())
        .and_then(|array_bytes| array_bytes.checked_add(std::mem::size_of::<ReturnBlock>()))
        .and_then(|bytes| {
            string_count
                .checked_mul(std::mem::size_of::<Box<[u16]>>())
                .and_then(|string_slots| bytes.checked_add(string_slots))
        })
        .ok_or(XllError::Domain {
            code: crate::DomainErrorCode::Overflow,
        })
}

impl Drop for ReturnBlock {
    fn drop(&mut self) {
        debug_assert_eq!(self.magic, RETURN_MAGIC);
        if let Some(tracker) = self.tracker.as_ref() {
            // This runs before field drop glue. The matching free-operation
            // guard remains active until the complete block, including every
            // UTF-16 buffer and array cell, has been released.
            tracker.release_block();
        }
        #[cfg(test)]
        LIVE_BLOCKS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn encode_scalar(
    value: OwnedExcelValue,
    strings: &mut Vec<Box<[u16]>>,
    allocation_bytes: &mut usize,
) -> XllResult<XLOPER12> {
    match value {
        OwnedExcelValue::Number(number) if number.is_finite() => Ok(XLOPER12::number(number)),
        OwnedExcelValue::Number(_) => {
            Err(XllError::input("<return>", crate::InputError::NonFinite))
        }
        OwnedExcelValue::Boolean(boolean) => Ok(XLOPER12::boolean(boolean)),
        OwnedExcelValue::Integer(integer) => Ok(XLOPER12::integer(integer)),
        OwnedExcelValue::Error(ExcelErrorValue(error)) => Ok(XLOPER12::error(error.code())),
        // xltypeMissing/xltypeNil are argument concepts. Excel displays them
        // as numeric zero when returned from a UDF, so encode an explicit
        // absence error instead.
        OwnedExcelValue::Missing | OwnedExcelValue::Blank => {
            Ok(XLOPER12::error(crate::ExcelError::NotAvailable.code()))
        }
        OwnedExcelValue::String(text) => {
            let counted =
                crate::utf16::encode_counted(&text, "<return>", crate::utf16::EXCEL_STRING_LIMIT)?;
            let string_bytes = counted
                .len()
                .checked_mul(std::mem::size_of::<u16>())
                .ok_or(XllError::Domain {
                    code: crate::DomainErrorCode::Overflow,
                })?;
            *allocation_bytes =
                allocation_bytes
                    .checked_add(string_bytes)
                    .ok_or(XllError::Domain {
                        code: crate::DomainErrorCode::Overflow,
                    })?;
            enforce_return_limit(*allocation_bytes)?;
            let mut counted = counted.into_boxed_slice();
            let pointer = counted.as_mut_ptr();
            strings.push(counted);
            Ok(XLOPER12 {
                value: XLOPER12Value { string: pointer },
                xltype: XLTYPE_STR,
            })
        }
        OwnedExcelValue::Matrix(_) => Err(XllError::input(
            "<return>",
            crate::InputError::Malformed("nested return arrays are not supported"),
        )),
    }
}

fn enforce_return_limit(bytes: usize) -> XllResult<()> {
    if bytes > MAX_RETURN_BYTES {
        Err(XllError::input(
            "<return>",
            crate::InputError::TooLarge {
                limit: MAX_RETURN_BYTES,
                actual: bytes,
            },
        ))
    } else {
        Ok(())
    }
}

#[allow(dead_code)]
pub(crate) fn allocate(value: OwnedExcelValue) -> XllResult<*mut XLOPER12> {
    ReturnBlock::build(value).map(|block| ReturnBlock::into_non_null(block).as_ptr())
}

fn allocate_tracked(
    value: OwnedExcelValue,
    tracker: &Arc<ReturnTracker>,
) -> XllResult<*mut XLOPER12> {
    ReturnBlock::build_tracked(value, Arc::clone(tracker))
        .map(|block| ReturnBlock::into_non_null(block).as_ptr())
}

#[cfg(any(feature = "async", test))]
pub(crate) fn allocate_async_return(value: OwnedExcelValue) -> XllResult<NonNull<XLOPER12>> {
    ReturnBlock::build_async(value).map(ReturnBlock::into_non_null)
}

#[allow(dead_code)]
pub(crate) fn allocate_error(error: &XllError) -> *mut XLOPER12 {
    // Encoding an Excel error is allocation-only and cannot fail except for
    // process-wide OOM, which Rust defines as aborting.
    allocate(OwnedExcelValue::Error(ExcelErrorValue(error.excel_error())))
        .unwrap_or(ptr::null_mut())
}

fn allocate_tracked_error(error: &XllError, tracker: &Arc<ReturnTracker>) -> *mut XLOPER12 {
    // Encoding an Excel error is allocation-only and cannot fail except for
    // process-wide OOM, which Rust defines as aborting.
    allocate_tracked(
        OwnedExcelValue::Error(ExcelErrorValue(error.excel_error())),
        tracker,
    )
    .unwrap_or(ptr::null_mut())
}

static CLOSING_ERROR: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

pub(crate) fn allocate_detached_error(error: &XllError) -> *mut XLOPER12 {
    // Return admission is already closed, so publishing another DLL-free block
    // would race the terminal drain. A permanently owned scalar has no
    // xlAutoFree12 callback and remains valid even if Excel keeps the pointer
    // until after the XLL has been unmapped.
    // Use a process-wide static singleton to prevent memory leaks on repeated late calls.
    let _code = error.excel_error().code();
    let ptr = *CLOSING_ERROR.get_or_init(|| {
        Box::into_raw(Box::new(XLOPER12::error(
            XllError::Closing.excel_error().code(),
        ))) as usize
    });
    ptr as *mut XLOPER12
}

#[cfg(feature = "async")]
pub(crate) fn allocate_async_error(error: &XllError) -> NonNull<XLOPER12> {
    // Encoding a scalar Excel error cannot fail except for process-wide OOM,
    // which Rust defines as aborting.
    allocate_async_return(OwnedExcelValue::Error(ExcelErrorValue(error.excel_error())))
        .expect("scalar Excel error return allocation is infallible")
}

#[cfg(feature = "async")]
pub(crate) struct AsyncReturnPointer {
    pointer: NonNull<XLOPER12>,
}

#[cfg(feature = "async")]
impl AsyncReturnPointer {
    pub(crate) fn allocate(value: OwnedExcelValue) -> XllResult<Self> {
        allocate_async_return(value).map(|pointer| Self { pointer })
    }

    pub(crate) fn error(error: &XllError) -> Self {
        Self {
            pointer: allocate_async_error(error),
        }
    }

    pub(crate) fn as_non_null(&self) -> NonNull<XLOPER12> {
        self.pointer
    }
}

#[cfg(feature = "async")]
impl Drop for AsyncReturnPointer {
    fn drop(&mut self) {
        // SAFETY: this RAII owner is created only from a fresh ReturnBlock raw
        // pointer and never transfers ownership.
        unsafe { free_return(self.pointer.as_ptr()) };
    }
}

/// Runs one generated UDF body behind a Rust panic and ownership boundary.
#[doc(hidden)]
#[allow(dead_code)]
#[must_use]
pub fn ffi_boundary<F, T>(operation: F) -> *mut XLOPER12
where
    F: FnOnce() -> XllResult<T>,
    T: IntoExcelValue,
{
    let (_guard, accepted) = crate::ingress::global_ingress().enter();
    if !accepted {
        return allocate_detached_error(&XllError::Closing);
    }
    match catch_unwind(AssertUnwindSafe(|| {
        let value = operation()?;
        let value = value.into_excel_value()?;
        allocate(value)
    })) {
        Ok(Ok(pointer)) => pointer,
        Ok(Err(error)) => allocate_error(&error),
        Err(_) => allocate_error(&XllError::Panic),
    }
}

/// Runs a return-producing framework callback under this runtime's terminal
/// return-admission gate.
#[doc(hidden)]
#[must_use]
pub fn ffi_boundary_tracked<S, F, T>(runtime: &Runtime<S>, operation: F) -> *mut XLOPER12
where
    F: FnOnce() -> XllResult<T>,
    T: IntoExcelValue,
{
    let (_guard, accepted) = crate::ingress::global_ingress().enter();
    if !accepted {
        return allocate_detached_error(&XllError::Closing);
    }
    let Some(producer) = runtime.enter_return_producer() else {
        return allocate_detached_error(&XllError::Closing);
    };
    let tracker = producer.tracker();
    match catch_unwind(AssertUnwindSafe(|| {
        let value = operation()?;
        let value = value.into_excel_value()?;
        allocate_tracked(value, tracker)
    })) {
        Ok(Ok(pointer)) => pointer,
        Ok(Err(error)) => allocate_tracked_error(&error, tracker),
        Err(_) => allocate_tracked_error(&XllError::Panic, tracker),
    }
}

/// Outermost panic boundary for void-returning `extern "system"` entry points.
///
/// Prevents panics from unwinding across the ABI boundary. Used by async UDF
/// wrappers, async calculation lifecycle exports, and similar void-returning
/// Excel callbacks.
#[doc(hidden)]
#[allow(dead_code)]
pub fn ffi_boundary_void(operation: impl FnOnce()) {
    let (_guard, accepted) = crate::ingress::global_ingress().enter();
    if !accepted {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(operation));
}

/// Runs a generated UDF boundary and reports detailed failures to the configured sink.
#[must_use]
pub fn udf_boundary_named<S, F, T>(
    runtime: &Runtime<S>,
    udf_id: &'static str,
    excel_name: &'static str,
    operation: F,
) -> *mut XLOPER12
where
    F: FnOnce(&S) -> XllResult<T>,
    T: IntoExcelValue,
{
    let (_guard, accepted) = crate::ingress::global_ingress().enter();
    if !accepted {
        return allocate_detached_error(&XllError::Closing);
    }
    let Some(producer) = runtime.enter_return_producer() else {
        return allocate_detached_error(&XllError::Closing);
    };
    let tracker = producer.tracker();
    match catch_unwind(AssertUnwindSafe(|| {
        udf_boundary_named_inner(runtime, tracker, udf_id, excel_name, operation)
    })) {
        Ok(pointer) => pointer,
        Err(_) => allocate_tracked_error(&XllError::Panic, tracker),
    }
}

fn udf_boundary_named_inner<S, F, T>(
    runtime: &Runtime<S>,
    tracker: &Arc<ReturnTracker>,
    udf_id: &'static str,
    excel_name: &'static str,
    operation: F,
) -> *mut XLOPER12
where
    F: FnOnce(&S) -> XllResult<T>,
    T: IntoExcelValue,
{
    let call_id = runtime.next_call_id();
    let started_at = SystemTime::now();
    let started = Instant::now();
    match runtime.enter() {
        Ok(guard) => {
            let concurrent_calls = guard.concurrent_calls();
            let metadata = CallMetadata {
                udf_id,
                excel_name,
                call_id: CallId::from(call_id),
                calculation_id: runtime.calculation_id(),
                started_at,
                concurrent_calls,
            };
            let layers = match crate::execution::EnteredLayers::enter(&runtime.layers(), &metadata)
            {
                Ok(layers) => layers,
                Err(error) => {
                    crate::diagnostics::report_no_unwind(udf_id, &error);
                    let outcome = crate::execution::outcome_for_error(&error, started.elapsed());
                    crate::execution::trace(&metadata, &outcome);
                    return allocate_tracked_error(&error, tracker);
                }
            };
            let result = catch_unwind(AssertUnwindSafe(|| {
                let value = operation(guard.state())?;
                let value = value.into_excel_value()?;
                allocate_tracked(value, tracker)
            }))
            .unwrap_or(Err(XllError::Panic));
            let pointer = match result {
                Ok(pointer) => {
                    let outcome = CallOutcome {
                        result: UdfResultKind::Success,
                        error: None,
                        vendor_code: None,
                        duration: started.elapsed(),
                    };
                    layers.exit(&outcome);
                    crate::execution::trace(&metadata, &outcome);
                    pointer
                }
                Err(error) => {
                    crate::diagnostics::report_no_unwind(udf_id, &error);
                    let outcome = crate::execution::outcome_for_error(&error, started.elapsed());
                    layers.exit(&outcome);
                    crate::execution::trace(&metadata, &outcome);
                    allocate_tracked_error(&error, tracker)
                }
            };
            drop(guard);
            pointer
        }
        Err(error) => {
            crate::diagnostics::report_no_unwind(udf_id, &error);
            let metadata = CallMetadata {
                udf_id,
                excel_name,
                call_id: CallId::from(call_id),
                calculation_id: runtime.calculation_id(),
                started_at,
                concurrent_calls: 0,
            };
            let outcome = crate::execution::outcome_for_error(&error, started.elapsed());
            crate::execution::trace(&metadata, &outcome);
            allocate_tracked_error(&error, tracker)
        }
    }
}

/// Frees a DLL-owned return previously produced by `ffi_boundary`.
///
/// # Safety
///
/// `pointer` must be null or the exact live pointer returned by this crate.
/// It must be freed exactly once.
pub unsafe fn free_return(pointer: *mut XLOPER12) {
    // SAFETY: This function forwards its caller's ownership contract.
    let _operation = unsafe { enter_return_free_operation(pointer) };
    // SAFETY: This function forwards its caller's ownership contract.
    unsafe { free_return_block(pointer) };
}

/// Runs DLL-owned return cleanup behind a no-unwind Excel ABI boundary.
///
/// # Safety
///
/// The pointer must satisfy `free_return`'s ownership contract.
#[must_use = "the guard must remain live until the generated xlAutoFree12 callback returns"]
pub unsafe fn free_return_boundary(pointer: *mut XLOPER12) -> ReturnFreeBoundaryGuard {
    // SAFETY: the caller guarantees a live ReturnBlock or null pointer. Clone
    // its tracker before releasing the allocation so the callback remains
    // counted through the generated ABI wrapper's epilogue.
    let operation = unsafe { enter_return_free_operation(pointer) };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: This function forwards its caller's ownership contract. The
        // outer operation guard already covers the complete block teardown.
        unsafe { free_return_block(pointer) };
    }));
    ReturnFreeBoundaryGuard {
        _operation: operation,
    }
}

fn is_detached_error_pointer(pointer: *mut XLOPER12) -> bool {
    CLOSING_ERROR
        .get()
        .is_some_and(|static_ptr| pointer as usize == *static_ptr)
}

unsafe fn enter_return_free_operation(pointer: *mut XLOPER12) -> Option<ReturnFreeGuard> {
    if pointer.is_null() || is_detached_error_pointer(pointer) {
        return None;
    }
    let block_pointer = pointer.cast::<ReturnBlock>();
    // SAFETY: the caller contract guarantees a live ReturnBlock. Cloning the
    // tracker does not transfer or mutate ownership of the block itself.
    let tracker = unsafe { (*block_pointer).tracker.as_ref().cloned() };
    tracker.map(|tracker| tracker.enter_free())
}

unsafe fn free_return_block(pointer: *mut XLOPER12) {
    if pointer.is_null() || is_detached_error_pointer(pointer) {
        return;
    }
    let block_pointer = pointer.cast::<ReturnBlock>();
    // SAFETY: The caller contract guarantees this is a unique ReturnBlock.
    let block = unsafe { Box::from_raw(block_pointer) };
    debug_assert_eq!(block.magic, RETURN_MAGIC);
    drop(block);
}

#[cfg(test)]
static LIVE_BLOCKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(all(feature = "async", test))]
pub(crate) fn live_return_blocks() -> usize {
    LIVE_BLOCKS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExcelError, Matrix};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use xlfn_sys::{XLTYPE_ERR, XLTYPE_NUM};

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::ingress::global_ingress().reset();
        guard
    }

    #[test]
    fn oper_is_the_first_field_and_is_freed_once() {
        let _test = test_lock();
        let before = LIVE_BLOCKS.load(std::sync::atomic::Ordering::Relaxed);
        let pointer = ffi_boundary(|| Ok(42.0));
        assert!(!pointer.is_null());
        // SAFETY: pointer is the live return from ffi_boundary.
        assert_eq!(unsafe { (*pointer).base_type() }, XLTYPE_NUM);
        assert_eq!(
            LIVE_BLOCKS.load(std::sync::atomic::Ordering::Relaxed),
            before + 1
        );
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
        assert_eq!(
            LIVE_BLOCKS.load(std::sync::atomic::Ordering::Relaxed),
            before
        );
    }

    #[test]
    fn strings_use_counted_utf16_owned_by_block() {
        let _test = test_lock();
        let pointer = ffi_boundary(|| Ok("日本語".to_owned()));
        // SAFETY: pointer is live and the type selects the string member.
        let text = unsafe { (*pointer).value.string };
        // SAFETY: the return block owns a prefix and three UTF-16 units.
        let text = unsafe { std::slice::from_raw_parts(text, 4) };
        assert_eq!(text[0], 3);
        assert_eq!(&text[1..], &[26085, 26412, 35486]);
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
    }

    #[test]
    fn arrays_hold_independent_cells() {
        let _test = test_lock();
        let matrix = Matrix::new(1, 2, vec![1.0, 2.0]).unwrap();
        let pointer = ffi_boundary(|| Ok(matrix));
        // SAFETY: pointer is live and its root type is multi.
        let array = unsafe { (*pointer).value.array };
        assert_eq!(array.rows, 1);
        assert_eq!(array.columns, 2);
        // SAFETY: the array contains two live cells.
        assert_eq!(unsafe { (*array.values.add(1)).base_type() }, XLTYPE_NUM);
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
    }

    #[test]
    fn return_limit_accounts_for_all_owned_allocation_payloads() {
        let scalar = OwnedExcelValue::String("x".to_owned());
        let (cells, strings) = allocation_shape(&scalar);
        assert_eq!((cells, strings), (0, 1));
        assert_eq!(
            base_allocation_payload_bytes(cells, strings).unwrap(),
            std::mem::size_of::<ReturnBlock>() + std::mem::size_of::<Box<[u16]>>()
        );

        let matrix = OwnedExcelValue::Matrix(
            Matrix::new(
                1,
                2,
                vec![
                    OwnedExcelValue::String("x".to_owned()),
                    OwnedExcelValue::Number(1.0),
                ],
            )
            .unwrap(),
        );
        let (cells, strings) = allocation_shape(&matrix);
        assert_eq!((cells, strings), (2, 1));
        assert_eq!(
            base_allocation_payload_bytes(cells, strings).unwrap(),
            std::mem::size_of::<ReturnBlock>()
                + 2 * std::mem::size_of::<XLOPER12>()
                + std::mem::size_of::<Box<[u16]>>()
        );
        assert!(base_allocation_payload_bytes(usize::MAX, 0).is_err());
    }

    #[test]
    fn missing_and_blank_returns_are_explicit_not_available_errors() {
        let _test = test_lock();
        for value in [OwnedExcelValue::Missing, OwnedExcelValue::Blank] {
            let pointer = ffi_boundary(|| Ok(value));
            // SAFETY: pointer is a live encoded return value.
            assert_eq!(unsafe { (*pointer).base_type() }, XLTYPE_ERR);
            // SAFETY: XLTYPE_ERR selects the error union member.
            let error = unsafe { (*pointer).value.error };
            assert_eq!(error, ExcelError::NotAvailable.code());
            // SAFETY: pointer has not yet been freed.
            unsafe { free_return(pointer) };
        }
    }

    #[test]
    fn errors_and_panics_do_not_cross_ffi() {
        let _test = test_lock();
        let error_pointer =
            ffi_boundary::<_, f64>(|| Err(XllError::input("x", crate::InputError::NonFinite)));
        // SAFETY: pointer is a live encoded error.
        assert_eq!(unsafe { (*error_pointer).base_type() }, XLTYPE_ERR);
        // SAFETY: XLTYPE_ERR selects error.
        // SAFETY: XLTYPE_ERR selects the error union member.
        let error_code = unsafe { (*error_pointer).value.error };
        assert_eq!(error_code, ExcelError::Value.code());
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(error_pointer) };

        let panic_pointer = ffi_boundary::<_, f64>(|| panic!("boundary test"));
        // SAFETY: pointer is a live encoded error.
        assert_eq!(unsafe { (*panic_pointer).base_type() }, XLTYPE_ERR);
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(panic_pointer) };
    }

    #[test]
    fn panicking_into_excel_value_does_not_cross_ffi() {
        struct PanickingReturn;

        impl IntoExcelValue for PanickingReturn {
            fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
                panic!("injected IntoExcelValue panic")
            }
        }

        let _test = test_lock();
        let pointer = ffi_boundary(|| Ok(PanickingReturn));
        // SAFETY: pointer is a live encoded panic error.
        assert_eq!(unsafe { (*pointer).base_type() }, XLTYPE_ERR);
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
    }

    #[test]
    fn udf_guard_covers_return_conversion_and_allocation() {
        struct BlockingReturn {
            converting: mpsc::SyncSender<()>,
            release: mpsc::Receiver<()>,
        }

        impl IntoExcelValue for BlockingReturn {
            fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
                self.converting.send(()).unwrap();
                self.release.recv().unwrap();
                Ok(OwnedExcelValue::Number(1.0))
            }
        }

        let _test = test_lock();
        let runtime = Arc::new(Runtime::new());
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        let (converting_tx, converting_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let caller_runtime = Arc::clone(&runtime);
        let caller = std::thread::spawn(move || {
            let pointer = udf_boundary_named(&caller_runtime, "test", "TEST", |_| {
                Ok(BlockingReturn {
                    converting: converting_tx,
                    release: release_rx,
                })
            });
            // SAFETY: this thread owns the live return pointer.
            unsafe { free_return(pointer) };
        });

        converting_rx.recv().unwrap();
        assert!(runtime.begin_close());
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer_runtime = Arc::clone(&runtime);
        let closer = std::thread::spawn(move || {
            closer_runtime.wait_for_calls();
            closed_tx.send(()).unwrap();
        });
        assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());
        release_tx.send(()).unwrap();
        closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        caller.join().unwrap();
        closer.join().unwrap();
    }

    #[test]
    fn close_waits_until_excel_releases_a_tracked_return() {
        let _test = test_lock();
        let runtime = Arc::new(Runtime::new());
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        let pointer = udf_boundary_named(&runtime, "tracked", "TRACKED", |_| Ok(7.0));
        assert!(!pointer.is_null());
        assert!(runtime.begin_close());

        let (drained_tx, drained_rx) = mpsc::sync_channel(1);
        let closer_runtime = Arc::clone(&runtime);
        let closer = std::thread::spawn(move || {
            closer_runtime.wait_for_returns();
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
        let runtime = Runtime::new();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        assert!(runtime.begin_close());

        let pointer = ffi_boundary_tracked(&runtime, || Ok(7.0));
        assert!(!pointer.is_null());
        // SAFETY: admission rejection returns a standalone Box<XLOPER12>, not a
        // ReturnBlock, specifically so Excel will never call xlAutoFree12.
        unsafe {
            assert_eq!((*pointer).xltype & xlfn_sys::XLBIT_DLL_FREE, 0);
            drop(Box::from_raw(pointer));
        }
    }

    #[test]
    fn udf_layer_sees_failures_and_call_metadata() {
        struct Recorder(Arc<std::sync::Mutex<Vec<(String, UdfResultKind, usize)>>>);
        struct RecorderGuard {
            events: Arc<std::sync::Mutex<Vec<(String, UdfResultKind, usize)>>>,
            udf_id: String,
            concurrent_calls: usize,
        }

        impl crate::UdfLayer for Recorder {
            fn enter(
                &self,
                metadata: &crate::CallMetadata,
            ) -> XllResult<Box<dyn crate::UdfLayerGuard>> {
                Ok(Box::new(RecorderGuard {
                    events: Arc::clone(&self.0),
                    udf_id: metadata.udf_id.to_owned(),
                    concurrent_calls: metadata.concurrent_calls,
                }))
            }
        }

        impl crate::UdfLayerGuard for RecorderGuard {
            fn exit(self: Box<Self>, outcome: &crate::CallOutcome<'_>) {
                self.events.lock().unwrap().push((
                    self.udf_id.clone(),
                    outcome.result,
                    self.concurrent_calls,
                ));
            }
        }

        let _test = test_lock();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let runtime = Runtime::new();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), vec![Arc::new(Recorder(Arc::clone(&events)))]);
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        let pointer = udf_boundary_named(&runtime, "test_conversion", "TEST.CONVERSION", |_| {
            Err::<f64, _>(XllError::input("value", crate::InputError::NonFinite))
        });
        // SAFETY: this test owns the live return pointer.
        unsafe { free_return(pointer) };

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "test_conversion");
        assert_eq!(events[0].1, UdfResultKind::InputError);
        assert_eq!(events[0].2, 1);
    }

    #[test]
    fn owned_values_cannot_bypass_scalar_validation() {
        let _test = test_lock();
        let pointer = ffi_boundary(|| Ok(OwnedExcelValue::Number(f64::NAN)));
        // SAFETY: pointer is a live encoded validation error.
        assert_eq!(unsafe { (*pointer).base_type() }, XLTYPE_ERR);
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
    }

    #[test]
    fn scalar_returns_do_not_evaluate_formula_fingerprints() {
        let runtime: Runtime<()> = Runtime::new();
        let mut unsupported = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                sref: xlfn_sys::XLOPER12SRef {
                    count: 1,
                    reference: xlfn_sys::XLREF12 {
                        rw_first: 0,
                        rw_last: 0,
                        col_first: 0,
                        col_last: 0,
                    },
                },
            },
            xltype: xlfn_sys::XLTYPE_SREF,
        };
        let raw_arguments = [&mut unsupported as *mut _];
        // SAFETY: unsupported and its inline reference remain live for the
        // context lifetime. A handle fingerprint would reject this type.
        let mut context = unsafe { ReturnContext::for_call(&runtime, "scalar", &raw_arguments) };
        let value = <f64 as crate::ExcelReturn>::invoke(&mut context, || Ok(4.5)).unwrap();
        assert_eq!(value, 4.5);
    }

    #[test]
    fn async_return_allocation_does_not_set_xlbit_dll_free() {
        let _test = test_lock();
        let udf_ptr = allocate(OwnedExcelValue::Number(42.0)).unwrap();
        let async_ptr = allocate_async_return(OwnedExcelValue::Number(42.0)).unwrap();

        // SAFETY: both pointers are valid ReturnBlock pointers
        unsafe {
            assert_ne!((*udf_ptr).xltype & xlfn_sys::XLBIT_DLL_FREE, 0);
            assert_eq!((*async_ptr.as_ptr()).xltype & xlfn_sys::XLBIT_DLL_FREE, 0);

            free_return(udf_ptr);
            free_return(async_ptr.as_ptr());
        }
    }
}
