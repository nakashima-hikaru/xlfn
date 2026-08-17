use crate::host_callback::HostCallbackSession;
use crate::input_identity::InputFingerprint;
use crate::return_storage::ReturnStorage;
use crate::{
    CallId, CallMetadata, CallOutcome, ExcelErrorValue, IntoExcelValue, OwnedExcelValue, Runtime,
    UdfResultKind, XlArrayOutput, XllError, XllResult,
};
use parking_lot::{Condvar, Mutex};
use std::cell::{Cell, UnsafeCell};
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
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

const RETURN_STRIPE_COUNT: usize = 32;
const RETURN_STRIPE_SEALED: usize = 1_usize << (usize::BITS - 1);
const RETURN_STRIPE_COUNT_MASK: usize = RETURN_STRIPE_SEALED - 1;
const RETURN_QUIESCENCE_RECHECK_INTERVAL: Duration = Duration::from_millis(1);

thread_local! {
    static RETURN_STRIPE: Cell<usize> = const { Cell::new(usize::MAX) };
}

static NEXT_RETURN_STRIPE: AtomicUsize = AtomicUsize::new(0);

fn current_return_stripe() -> usize {
    RETURN_STRIPE.with(|stripe| {
        let current = stripe.get();
        if current != usize::MAX {
            return current;
        }
        let assigned =
            NEXT_RETURN_STRIPE.fetch_add(1, Ordering::Relaxed) & (RETURN_STRIPE_COUNT - 1);
        stripe.set(assigned);
        assigned
    })
}

#[derive(Debug)]
#[repr(C, align(128))]
struct ReturnStripe {
    // The high bit seals this stripe against new producers during shutdown.
    // The low bits count live Excel-owned return obligations.
    state: AtomicUsize,
}

impl ReturnStripe {
    const fn new_closed() -> Self {
        Self {
            state: AtomicUsize::new(RETURN_STRIPE_SEALED),
        }
    }

    fn try_enter(&self) -> bool {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if observed & RETURN_STRIPE_SEALED != 0 {
                return false;
            }
            if observed & RETURN_STRIPE_COUNT_MASK == RETURN_STRIPE_COUNT_MASK {
                std::process::abort();
            }
            match self.state.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(current) => observed = current,
            }
        }
    }

    fn release(&self) {
        let previous = self.state.fetch_sub(1, Ordering::AcqRel);
        if previous & RETURN_STRIPE_COUNT_MASK == 0 {
            std::process::abort();
        }
    }

    fn seal(&self) {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if observed & RETURN_STRIPE_SEALED != 0 {
                return;
            }
            match self.state.compare_exchange_weak(
                observed,
                observed | RETURN_STRIPE_SEALED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => observed = current,
            }
        }
    }

    fn reopen(&self) {
        debug_assert!(self.state.load(Ordering::Acquire) & RETURN_STRIPE_SEALED != 0);
        debug_assert_eq!(self.active(), 0);
        self.state.store(0, Ordering::Release);
    }

    fn is_sealed(&self) -> bool {
        self.state.load(Ordering::Acquire) & RETURN_STRIPE_SEALED != 0
    }

    fn active(&self) -> usize {
        self.state.load(Ordering::Acquire) & RETURN_STRIPE_COUNT_MASK
    }
}

/// Runtime-local accounting for code paths that can retain or re-enter the
/// XLL after a synchronous function has returned to Excel.
pub(crate) struct ReturnTracker {
    // Ordinary producer entry and obligation release use one thread-assigned
    // stripe. Shutdown seals all stripes and only then scans them for quiescence.
    stripes: [Arc<ReturnStripe>; RETURN_STRIPE_COUNT],
    wait_lock: Mutex<()>,
    quiescent: Condvar,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

pub(crate) struct ReturnObligation {
    stripe: Arc<ReturnStripe>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    tracker: Arc<ReturnTracker>,
}

#[cfg(any(test, feature = "shutdown-refinement"))]
impl ReturnObligation {
    fn tracker(&self) -> &ReturnTracker {
        &self.tracker
    }
}

impl Drop for ReturnObligation {
    fn drop(&mut self) {
        self.stripe.release();
    }
}

impl ReturnTracker {
    pub(crate) fn new_closed() -> Self {
        Self {
            stripes: std::array::from_fn(|_| Arc::new(ReturnStripe::new_closed())),
            wait_lock: Mutex::new(()),
            quiescent: Condvar::new(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: std::sync::OnceLock::new(),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        let _ = self.ghost.set(ghost);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.get() {
            ghost.record_event(event);
        }
    }

    pub(crate) fn reopen_admission(&self) -> XllResult<()> {
        if !self.admission_closed() || !self.is_quiescent() {
            return Err(XllError::Internal {
                diagnostic_id: 0x5254_4e52_454f_504e,
            });
        }
        for stripe in &self.stripes {
            stripe.reopen();
        }
        Ok(())
    }

    pub(crate) fn close_admission(&self) {
        for stripe in &self.stripes {
            stripe.seal();
        }
    }

    pub(crate) fn try_enter_producer(self: &Arc<Self>) -> Option<ReturnProducerGuard> {
        let stripe_index = current_return_stripe();
        if !self.stripes[stripe_index].try_enter() {
            return None;
        }
        Some(ReturnProducerGuard {
            obligation: Some(ReturnObligation {
                stripe: Arc::clone(&self.stripes[stripe_index]),
                #[cfg(any(test, feature = "shutdown-refinement"))]
                tracker: Arc::clone(self),
            }),
        })
    }

    pub(crate) fn is_quiescent(&self) -> bool {
        self.stripes.iter().all(|stripe| stripe.active() == 0)
    }

    pub(crate) fn admission_closed(&self) -> bool {
        self.stripes.iter().all(|stripe| stripe.is_sealed())
    }

    pub(crate) fn wait_for_quiescence(&self) {
        debug_assert!(self.admission_closed());

        let mut guard = self.wait_lock.lock();

        while !self.is_quiescent() {
            self.quiescent
                .wait_for(&mut guard, RETURN_QUIESCENCE_RECHECK_INTERVAL);
        }
    }

    #[cfg(test)]
    fn outstanding_obligations(&self) -> usize {
        self.stripes.iter().map(|stripe| stripe.active()).sum()
    }
}

pub(crate) struct ReturnProducerGuard {
    obligation: Option<ReturnObligation>,
}

impl ReturnProducerGuard {
    fn transfer_to_block(&mut self) -> ReturnObligation {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        {
            let _obligation = self
                .obligation
                .as_ref()
                .expect("return obligation is transferred exactly once");

            _obligation
                .tracker()
                .record_ghost_event(crate::shutdown_refinement::GhostEvent::CreateReturnBlock);
        }

        self.obligation
            .take()
            .expect("return obligation is transferred exactly once")
    }

    fn is_armed(&self) -> bool {
        self.obligation.is_some()
    }
}

struct ReturnFreeGuard {
    #[allow(
        dead_code,
        reason = "RAII obligation field held for lifecycle accounting"
    )]
    obligation: ReturnObligation,
}

/// Keeps one generated `xlAutoFree12` callback visible to terminal shutdown.
#[doc(hidden)]
pub struct ReturnFreeBoundaryGuard {
    _operation: Option<ReturnFreeGuard>,
}

impl Drop for ReturnFreeGuard {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.obligation
            .tracker()
            .record_ghost_event(crate::shutdown_refinement::GhostEvent::EndReturnFree);
    }
}

/// Call-scoped services used by [`crate::ExcelReturn`] implementations.
#[doc(hidden)]
pub struct ReturnContext<'call, 'scope> {
    runtime: Option<&'call dyn crate::value::HandleRuntimeProvider>,
    udf_id: Option<&'static str>,
    inputs: Option<InputFingerprint>,
    callbacks: Option<&'scope HostCallbackSession>,
    lifetime: PhantomData<Rc<()>>,
}

impl<'call, 'scope> ReturnContext<'call, 'scope> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            runtime: None,
            udf_id: None,
            inputs: None,
            callbacks: None,
            lifetime: PhantomData,
        }
    }

    #[doc(hidden)]
    /// Creates return services for one generated synchronous UDF call.
    ///
    pub fn for_call<S>(
        runtime: &'call Runtime<S>,
        udf_id: &'static str,
        inputs: Option<[u8; 32]>,
        scope: &'scope crate::CallScope<'scope>,
    ) -> Self {
        Self {
            runtime: Some(runtime),
            udf_id: Some(udf_id),
            inputs: inputs.map(InputFingerprint::from_bytes),
            callbacks: Some(scope.callbacks()),
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
        self.publish_handle(operation)
    }

    #[doc(hidden)]
    pub fn publish_existing_alias<'handle, T>(
        &mut self,
        operation: impl FnOnce() -> XllResult<crate::HandleAlias<'handle, T>>,
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
        let inputs = self.inputs.ok_or(crate::XllError::Internal {
            diagnostic_id: 0x4841_4e44_4449_4745,
        })?;
        let callbacks = self.callbacks.ok_or(crate::XllError::Internal {
            diagnostic_id: 0x4841_4e44_4342_4b53,
        })?;
        let key = crate::handle::formula_revision_key(callbacks, udf_id, inputs)?;
        let handles = runtime.handle_runtime()?;
        let (object_id, object) = operation()?.into_parts();
        let observer_handles = Arc::clone(&handles);
        let (token, _) =
            handles.prepare_observed_alias::<T, _>(key, object_id, object, move |key, token| {
                crate::rtd::observe(observer_handles, key, token, callbacks)
            })?;
        Ok(token)
    }

    fn publish_handle<T>(&mut self, operation: impl FnOnce() -> XllResult<T>) -> XllResult<String>
    where
        T: crate::handle::ExcelHandleObject,
    {
        let runtime = self.runtime.ok_or(crate::XllError::Internal {
            diagnostic_id: 0x4841_4e44_4354_5854,
        })?;
        let udf_id = self.udf_id.ok_or(crate::XllError::Internal {
            diagnostic_id: 0x4841_4e44_5544_4649,
        })?;
        let inputs = self.inputs.ok_or(crate::XllError::Internal {
            diagnostic_id: 0x4841_4e44_4449_4745,
        })?;
        let callbacks = self.callbacks.ok_or(crate::XllError::Internal {
            diagnostic_id: 0x4841_4e44_4342_4b53,
        })?;
        let key = crate::handle::formula_revision_key(callbacks, udf_id, inputs)?;
        let handles = runtime.handle_runtime()?;
        let observer_handles = Arc::clone(&handles);
        let (token, _) = handles.prepare_observed(key, operation, move |key, token| {
            crate::rtd::observe(observer_handles, key, token, callbacks)
        })?;
        Ok(token)
    }
}

impl Default for ReturnContext<'_, '_> {
    fn default() -> Self {
        Self::new()
    }
}

const RETURN_MAGIC: u64 = 0x584c_4c52_4554_3132;
#[cfg(target_pointer_width = "32")]
const MAX_RETURN_BYTES: usize = 64 * 1024 * 1024;
#[cfg(not(target_pointer_width = "32"))]
const MAX_RETURN_BYTES: usize = 256 * 1024 * 1024;

enum ReturnOwnership {
    Excel(Option<ReturnObligation>),
    #[cfg(any(feature = "async", test))]
    Local,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReturnBlockBacking {
    ThreadLocal,
    Heap,
}

#[repr(C)]
struct ReturnBlock {
    // This must remain first: Excel receives a pointer to this field and
    // xlAutoFree12 casts it back to ReturnBlock.
    oper: XLOPER12,
    storage: Option<ReturnStorage>,
    array: Option<Box<[XLOPER12]>>,
    ownership: ReturnOwnership,
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
        Some(unsafe { NonNull::new_unchecked((*self.block.get()).as_mut_ptr()) })
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
struct PreparedReturn {
    oper: XLOPER12,
    storage: Option<ReturnStorage>,
    array: Option<Box<[XLOPER12]>>,
}

impl PreparedReturn {
    fn encode(value: OwnedExcelValue) -> XllResult<Self> {
        match value {
            OwnedExcelValue::ArrayOutput(encoded) => Self::from_array_output(encoded),
            other => Self::encode_dynamic(other),
        }
    }

    fn from_array_output(encoded: XlArrayOutput) -> XllResult<Self> {
        let total_bytes = encoded
            .payload_bytes
            .checked_add(std::mem::size_of::<ReturnBlock>())
            .ok_or(XllError::Domain {
                code: crate::DomainErrorCode::Overflow,
            })?;

        enforce_return_limit(total_bytes)?;

        let rows = i32::try_from(encoded.rows).map_err(|_| XllError::Domain {
            code: crate::DomainErrorCode::Overflow,
        })?;

        let columns = i32::try_from(encoded.columns).map_err(|_| XllError::Domain {
            code: crate::DomainErrorCode::Overflow,
        })?;

        let mut cells = encoded.cells;
        let storage = encoded.storage;
        let pointer = cells.as_mut_ptr();

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

    fn encode_dynamic(value: OwnedExcelValue) -> XllResult<Self> {
        let mut storage = None;
        let (oper, array) = match value {
            OwnedExcelValue::Matrix(matrix) => {
                let rows = i32::try_from(matrix.rows()).map_err(|_| XllError::Domain {
                    code: crate::DomainErrorCode::Overflow,
                })?;
                let columns = i32::try_from(matrix.columns()).map_err(|_| XllError::Domain {
                    code: crate::DomainErrorCode::Overflow,
                })?;
                let values = matrix.into_vec();
                let mut allocation_bytes = base_allocation_payload_bytes(values.len())?;
                let mut cells = values
                    .into_iter()
                    .map(|cell| encode_scalar(cell, &mut storage, &mut allocation_bytes))
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
            OwnedExcelValue::ArrayOutput(encoded) => {
                return Self::from_array_output(encoded);
            }
            scalar => {
                let mut allocation_bytes = base_allocation_payload_bytes(0)?;
                let oper = encode_scalar(scalar, &mut storage, &mut allocation_bytes)?;
                (oper, None)
            }
        };
        Ok(Self {
            oper,
            storage,
            array,
        })
    }

    fn error(error: &XllError) -> Self {
        Self {
            oper: XLOPER12::error(error.excel_error().code()),
            storage: None,
            array: None,
        }
    }

    fn publish_excel(mut self, producer: &mut ReturnProducerGuard) -> *mut XLOPER12 {
        let obligation = producer.transfer_to_block();
        self.oper.xltype |= XLBIT_DLL_FREE;

        let block = ReturnBlock {
            oper: self.oper,
            storage: self.storage,
            array: self.array,
            ownership: ReturnOwnership::Excel(Some(obligation)),
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

    #[cfg(any(feature = "async", test))]
    fn publish_local(self) -> NonNull<XLOPER12> {
        let block = Box::new(ReturnBlock {
            oper: self.oper,
            storage: self.storage,
            array: self.array,
            ownership: ReturnOwnership::Local,
            magic: RETURN_MAGIC,
            backing: ReturnBlockBacking::Heap,
        });

        #[cfg(test)]
        LIVE_BLOCKS.fetch_add(1, Ordering::Relaxed);

        ReturnBlock::into_non_null(block)
    }
}

impl ReturnBlock {
    fn into_non_null(block: Box<Self>) -> NonNull<XLOPER12> {
        let pointer = Box::into_raw(block);
        // SAFETY: Box::into_raw always returns a non-null, properly aligned pointer.
        unsafe { NonNull::new_unchecked(pointer.cast::<XLOPER12>()) }
    }
}

/// Total payload requested from the allocator before UTF-16 buffers are added.
/// `ReturnBlock` includes the root XLOPER12, arena handle, and collection
/// control structures; the other terms cover the separately allocated array
/// slots.
fn base_allocation_payload_bytes(array_cells: usize) -> XllResult<usize> {
    array_cells
        .checked_mul(std::mem::size_of::<XLOPER12>())
        .and_then(|array_bytes| array_bytes.checked_add(std::mem::size_of::<ReturnBlock>()))
        .ok_or(XllError::Domain {
            code: crate::DomainErrorCode::Overflow,
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
            LIVE_BLOCKS.fetch_sub(1, Ordering::Relaxed);
            if PANIC_ON_RETURN_BLOCK_DROP.swap(false, Ordering::SeqCst) {
                panic!("injected ReturnBlock drop panic");
            }
        }
    }
}

fn encode_scalar(
    value: OwnedExcelValue,
    storage: &mut Option<ReturnStorage>,
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
            let utf16_length = crate::utf16::checked_utf16_len(
                &text,
                "<return>",
                crate::utf16::EXCEL_STRING_LIMIT,
            )?;
            let string_bytes = utf16_length
                .checked_add(1)
                .ok_or(XllError::Domain {
                    code: crate::DomainErrorCode::Overflow,
                })?
                .checked_mul(std::mem::size_of::<u16>())
                .ok_or(XllError::Domain {
                    code: crate::DomainErrorCode::Overflow,
                })?;
            let additional = string_bytes;
            *allocation_bytes =
                allocation_bytes
                    .checked_add(additional)
                    .ok_or(XllError::Domain {
                        code: crate::DomainErrorCode::Overflow,
                    })?;
            enforce_return_limit(*allocation_bytes)?;
            let storage = storage.get_or_insert_with(ReturnStorage::new);
            let pointer = storage.alloc_counted_utf16_with_length(
                &text,
                "<return>",
                crate::utf16::EXCEL_STRING_LIMIT,
                utf16_length,
            )?;
            Ok(XLOPER12 {
                value: XLOPER12Value { string: pointer },
                xltype: XLTYPE_STR,
            })
        }
        OwnedExcelValue::Matrix(_) | OwnedExcelValue::ArrayOutput(_) => Err(XllError::input(
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

fn allocate_excel_owned(
    value: OwnedExcelValue,
    producer: &mut ReturnProducerGuard,
) -> XllResult<*mut XLOPER12> {
    let prepared = PreparedReturn::encode(value)?;
    Ok(prepared.publish_excel(producer))
}

#[cfg(any(feature = "async", test))]
pub(crate) fn allocate_local_async_return(value: OwnedExcelValue) -> XllResult<NonNull<XLOPER12>> {
    PreparedReturn::encode(value).map(PreparedReturn::publish_local)
}

fn allocate_excel_error(error: &XllError, producer: &mut ReturnProducerGuard) -> *mut XLOPER12 {
    PreparedReturn::error(error).publish_excel(producer)
}

static CLOSING_ERROR: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

pub(crate) fn closing_error_pointer() -> *mut XLOPER12 {
    // Return admission is already closed, so publishing another DLL-free block
    // would race the terminal drain. A permanently owned scalar has no
    // xlAutoFree12 callback and remains valid even if Excel keeps the pointer
    // until after the XLL has been unmapped.
    // Use a process-wide static singleton to prevent memory leaks on repeated late calls.
    let ptr = *CLOSING_ERROR.get_or_init(|| {
        Box::into_raw(Box::new(XLOPER12::error(
            XllError::Closing.excel_error().code(),
        ))) as usize
    });
    ptr as *mut XLOPER12
}

#[cfg(feature = "async")]
pub(crate) fn allocate_local_async_error(error: &XllError) -> NonNull<XLOPER12> {
    // Encoding a scalar Excel error cannot fail except for process-wide OOM,
    // which Rust defines as aborting.
    allocate_local_async_return(OwnedExcelValue::Error(ExcelErrorValue(error.excel_error())))
        .expect("scalar Excel error return allocation is infallible")
}

#[cfg(feature = "async")]
pub(crate) struct AsyncReturnPointer {
    pointer: NonNull<XLOPER12>,
}

#[cfg(feature = "async")]
impl AsyncReturnPointer {
    pub(crate) fn from_value(value: OwnedExcelValue) -> XllResult<Self> {
        allocate_local_async_return(value).map(|pointer| Self { pointer })
    }

    pub(crate) fn error(error: &XllError) -> Self {
        Self {
            pointer: allocate_local_async_error(error),
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

/// Runs a return-producing framework callback behind the runtime's terminal
/// return-admission gate.
#[doc(hidden)]
#[must_use]
pub fn ffi_boundary<S, F, T>(runtime: &Runtime<S>, operation: F) -> *mut XLOPER12
where
    F: FnOnce() -> XllResult<T>,
    T: IntoExcelValue,
{
    let (_guard, accepted) = crate::ingress::global_ingress().enter_with(|| {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::EnterExternal);
    });
    if !accepted {
        return closing_error_pointer();
    }
    let _call = match runtime.enter() {
        Ok(call) => call,
        Err(_) => {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
            return closing_error_pointer();
        }
    };
    let Some(mut producer) = runtime.enter_return_producer() else {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
        return closing_error_pointer();
    };
    let result = match catch_unwind(AssertUnwindSafe(|| {
        let value = operation()?;
        let value = value.into_excel_value()?;
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
    };
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
    result
}

/// Outermost panic boundary for void-returning `extern "system"` entry points.
///
/// Prevents panics from unwinding across the ABI boundary. Used by async UDF
/// wrappers, async calculation lifecycle exports, and similar void-returning
/// Excel callbacks.
#[doc(hidden)]
pub fn ffi_boundary_void<S>(runtime: &Runtime<S>, operation: impl FnOnce()) {
    let (_guard, accepted) = crate::ingress::global_ingress().enter_with(|| {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::EnterExternal);
    });
    if !accepted {
        return;
    }
    let _call = match runtime.enter() {
        Ok(call) => call,
        Err(_) => {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
            return;
        }
    };
    let _ = catch_unwind(AssertUnwindSafe(operation));
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
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
    let (_guard, accepted) = crate::ingress::global_ingress().enter_udf_with(|| {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::EnterExternal);
    });
    if !accepted {
        return closing_error_pointer();
    }
    let call = match runtime.enter() {
        Ok(call) => call,
        Err(_) => {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
            return closing_error_pointer();
        }
    };
    let Some(mut producer) = runtime.enter_return_producer() else {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
        return closing_error_pointer();
    };
    let result = match catch_unwind(AssertUnwindSafe(|| {
        udf_boundary_named_inner(runtime, &call, &mut producer, udf_id, excel_name, operation)
    })) {
        Ok(pointer) => pointer,
        Err(_) => {
            if !producer.is_armed() {
                std::process::abort();
            }
            allocate_excel_error(&XllError::Panic, &mut producer)
        }
    };
    drop(producer);
    drop(call);
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
    result
}

fn udf_boundary_named_inner<S, F, T>(
    runtime: &Runtime<S>,
    guard: &crate::runtime::CallGuard<'_, S>,
    producer: &mut ReturnProducerGuard,
    udf_id: &'static str,
    excel_name: &'static str,
    operation: F,
) -> *mut XLOPER12
where
    F: FnOnce(&S) -> XllResult<T>,
    T: IntoExcelValue,
{
    let instrumentation = crate::execution::InstrumentationPlan::for_runtime(runtime);

    if !instrumentation.enabled() {
        return udf_boundary_uninstrumented(guard, producer, udf_id, operation);
    }

    let concurrent_calls = crate::ingress::global_ingress().active_udfs();

    udf_boundary_instrumented(
        runtime,
        guard,
        producer,
        udf_id,
        excel_name,
        operation,
        InstrumentedUdfContext {
            instrumentation,
            concurrent_calls,
        },
    )
}

#[inline]
fn udf_boundary_uninstrumented<S, F, T>(
    guard: &crate::runtime::CallGuard<'_, S>,
    producer: &mut ReturnProducerGuard,
    udf_id: &'static str,
    operation: F,
) -> *mut XLOPER12
where
    F: FnOnce(&S) -> XllResult<T>,
    T: IntoExcelValue,
{
    let prepared = catch_unwind(AssertUnwindSafe(|| {
        let value = operation(guard.state())?;
        let value = value.into_excel_value()?;
        PreparedReturn::encode(value)
    }))
    .unwrap_or(Err(XllError::Panic));

    match prepared {
        Ok(prepared) => prepared.publish_excel(producer),
        Err(error) => {
            crate::diagnostics::report_no_unwind(udf_id, &error);
            PreparedReturn::error(&error).publish_excel(producer)
        }
    }
}

struct InstrumentedUdfContext {
    instrumentation: crate::execution::InstrumentationPlan,
    concurrent_calls: usize,
}

fn udf_boundary_instrumented<S, F, T>(
    runtime: &Runtime<S>,
    guard: &crate::runtime::CallGuard<'_, S>,
    producer: &mut ReturnProducerGuard,
    udf_id: &'static str,
    excel_name: &'static str,
    operation: F,
    context: InstrumentedUdfContext,
) -> *mut XLOPER12
where
    F: FnOnce(&S) -> XllResult<T>,
    T: IntoExcelValue,
{
    let InstrumentedUdfContext {
        instrumentation,
        concurrent_calls,
    } = context;
    let call_id = CallId::from(runtime.next_call_id());
    let calculation_id = runtime.calculation_id();
    let timer = crate::execution::CallTimer::start();

    let trace_metadata = crate::execution::UdfTraceMetadata {
        udf_id,
        excel_name,
        call_id,
        calculation_id,
        concurrent_calls,
    };

    let layers = match instrumentation.layers() {
        Some(configured_layers) => {
            let layer_metadata = CallMetadata {
                udf_id,
                excel_name,
                call_id,
                calculation_id,
                started_at: std::time::SystemTime::now(),
                concurrent_calls,
            };
            match crate::execution::EnteredLayers::enter(configured_layers, &layer_metadata) {
                Ok(layers) => Some(layers),
                Err(error) => {
                    crate::diagnostics::report_no_unwind(udf_id, &error);
                    let outcome = crate::execution::outcome_for_error(&error, timer.elapsed());
                    if instrumentation.trace_enabled() {
                        crate::execution::trace(&trace_metadata, &outcome);
                    }
                    return allocate_excel_error(&error, producer);
                }
            }
        }
        None => None,
    };

    let prepared = catch_unwind(AssertUnwindSafe(|| {
        let value = operation(guard.state())?;
        let value = value.into_excel_value()?;
        PreparedReturn::encode(value)
    }))
    .unwrap_or(Err(XllError::Panic));

    match prepared {
        Ok(prepared) => {
            let outcome = CallOutcome {
                result: UdfResultKind::Success,
                error: None,
                vendor_code: None,
                duration: timer.elapsed(),
            };
            if let Some(layers) = layers {
                layers.exit(&outcome);
            }
            if instrumentation.trace_enabled() {
                crate::execution::trace(&trace_metadata, &outcome);
            }
            prepared.publish_excel(producer)
        }
        Err(error) => {
            crate::diagnostics::report_no_unwind(udf_id, &error);
            let outcome = crate::execution::outcome_for_error(&error, timer.elapsed());
            if let Some(layers) = layers {
                layers.exit(&outcome);
            }
            if instrumentation.trace_enabled() {
                crate::execution::trace(&trace_metadata, &outcome);
            }
            PreparedReturn::error(&error).publish_excel(producer)
        }
    }
}

/// Frees a return block produced by an Excel-owned return boundary.
///
/// # Safety
///
/// `pointer` must be null or the exact live pointer returned by this crate.
/// It must be freed exactly once.
pub unsafe fn free_return(pointer: *mut XLOPER12) {
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
pub unsafe fn free_return_boundary(pointer: *mut XLOPER12) -> ReturnFreeBoundaryGuard {
    // SAFETY: caller contract guarantees pointer is a live return pointer or null.
    let operation = unsafe { enter_return_free_operation(pointer) };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: caller contract guarantees pointer is a live return pointer or null.
        unsafe { free_return_block(pointer, operation.as_ref()) };
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
    let block = pointer.cast::<ReturnBlock>();
    // SAFETY: caller contract guarantees pointer points to a valid live ReturnBlock.
    let ownership = unsafe { &mut (*block).ownership };
    match ownership {
        ReturnOwnership::Excel(slot) => {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            {
                let _obligation = slot
                    .as_ref()
                    .expect("Excel return obligation is taken exactly once");

                _obligation
                    .tracker()
                    .record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginReturnFree);
            }

            Some(ReturnFreeGuard {
                obligation: slot
                    .take()
                    .expect("Excel return obligation is taken exactly once"),
            })
        }
        #[cfg(any(feature = "async", test))]
        ReturnOwnership::Local => None,
    }
}

unsafe fn free_return_block(pointer: *mut XLOPER12, operation: Option<&ReturnFreeGuard>) {
    if pointer.is_null() || is_detached_error_pointer(pointer) {
        return;
    }
    let block_pointer = pointer.cast::<ReturnBlock>();
    // SAFETY: caller contract guarantees pointer refers to a live
    // ReturnBlock produced by publish_excel or its heap fallback.
    let block = unsafe { &mut *block_pointer };
    debug_assert_eq!(block.magic, RETURN_MAGIC);

    match &block.ownership {
        ReturnOwnership::Excel(slot) => {
            debug_assert!(slot.is_none());
            let _operation = operation.expect("Excel return destruction owns a free guard");
            #[cfg(any(test, feature = "shutdown-refinement"))]
            _operation
                .obligation
                .tracker()
                .record_ghost_event(crate::shutdown_refinement::GhostEvent::ReleaseReturnBlock);
        }
        #[cfg(any(feature = "async", test))]
        ReturnOwnership::Local => {
            debug_assert!(operation.is_none());
        }
    }

    destroy_return_block(block_pointer, block.backing);
}

fn destroy_return_block(pointer: *mut ReturnBlock, backing: ReturnBlockBacking) {
    match backing {
        ReturnBlockBacking::Heap => {
            // SAFETY: Heap backing was created with Box::into_raw and is
            // destroyed exactly once on this path.
            unsafe { drop(Box::from_raw(pointer)) };
        }
        ReturnBlockBacking::ThreadLocal => destroy_thread_local_return_block(pointer),
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

#[cfg(all(feature = "async", test))]
pub(crate) fn live_return_blocks() -> usize {
    LIVE_BLOCKS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExcelError, Matrix};
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
        if crate::ingress::global_ingress().phase() != crate::ingress::PHASE_CLOSED {
            crate::ingress::global_ingress().begin_close_with(|| {});
            let _ = crate::ingress::global_ingress().seal_and_drain();
        }
        ReturnValueTestGuard {
            _lock: lock,
            _module_lease: module_lease,
        }
    }

    fn open_test_runtime() -> Runtime<()> {
        let runtime = Runtime::new();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        drop(open_attempt);
        runtime
    }

    fn allocate_local_for_test(value: OwnedExcelValue) -> XllResult<*mut XLOPER12> {
        PreparedReturn::encode(value).map(|prep| prep.publish_local().as_ptr())
    }

    fn backing_of(pointer: *mut XLOPER12) -> ReturnBlockBacking {
        // SAFETY: callers pass a live pointer returned by this module.
        unsafe { (*pointer.cast::<ReturnBlock>()).backing }
    }

    #[test]
    fn oper_is_the_first_field_and_is_freed_once() {
        let _test = test_lock();
        let runtime = open_test_runtime();
        assert!(runtime.returns_are_quiescent());
        let pointer = ffi_boundary(&runtime, || Ok(42.0));
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
        let runtime = Arc::new(open_test_runtime());
        let worker_runtime = Arc::clone(&runtime);
        let worker = std::thread::spawn(move || {
            let first = ffi_boundary(&worker_runtime, || Ok(1.0));
            assert_eq!(backing_of(first), ReturnBlockBacking::ThreadLocal);

            let second = ffi_boundary(&worker_runtime, || Ok(2.0));
            assert_eq!(backing_of(second), ReturnBlockBacking::Heap);

            // SAFETY: both pointers are live Excel-owned returns from this
            // thread and are released exactly once.
            unsafe {
                free_return(second);
                free_return(first);
            }

            let reused = ffi_boundary(&worker_runtime, || Ok(3.0));
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
        let runtime = Arc::new(open_test_runtime());
        let barrier = Arc::new(Barrier::new(3));
        let (pointer_tx, pointer_rx) = mpsc::channel();
        let mut workers = Vec::new();

        for value in [11.0, 22.0] {
            let runtime = Arc::clone(&runtime);
            let barrier = Arc::clone(&barrier);
            let pointer_tx = pointer_tx.clone();
            workers.push(std::thread::spawn(move || {
                let pointer = ffi_boundary(&runtime, || Ok(value));
                assert_eq!(backing_of(pointer), ReturnBlockBacking::ThreadLocal);
                pointer_tx.send(pointer as usize).unwrap();
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

        let runtime = Arc::new(open_test_runtime());
        let worker_runtime = Arc::clone(&runtime);
        let worker = std::thread::spawn(move || {
            let string_pointer = ffi_boundary(&worker_runtime, || Ok("hello".to_owned()));
            assert_eq!(backing_of(string_pointer), ReturnBlockBacking::ThreadLocal);
            // SAFETY: string_pointer is the live return produced above.
            unsafe { free_return(string_pointer) };

            let matrix = Matrix::new(1, 2, vec!["left".to_owned(), "right".to_owned()]).unwrap();
            let array_pointer = ffi_boundary(&worker_runtime, || Ok(matrix));
            assert_eq!(backing_of(array_pointer), ReturnBlockBacking::ThreadLocal);
            // SAFETY: array_pointer is the live return produced above.
            unsafe { free_return(array_pointer) };
        });
        worker.join().unwrap();

        assert!(
            RETURN_BLOCKS_WITH_STORAGE.load(Ordering::Relaxed) > storage_before,
            "TLS return cleanup must drop ReturnStorage"
        );
        assert!(
            RETURN_BLOCKS_WITH_ARRAY.load(Ordering::Relaxed) > array_before,
            "TLS return cleanup must drop array cells"
        );
    }

    #[test]
    fn panicking_tls_drop_poison_falls_back_to_heap() {
        let _test = test_lock();
        let runtime = Arc::new(open_test_runtime());
        let worker_runtime = Arc::clone(&runtime);
        let worker = std::thread::spawn(move || {
            let pointer = ffi_boundary(&worker_runtime, || Ok(42.0));
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

            let fallback = ffi_boundary(&worker_runtime, || Ok(43.0));
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
        let pointer =
            allocate_local_for_test(OwnedExcelValue::String("日本語".to_owned())).unwrap();
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
        let matrix = Matrix::new(
            1,
            2,
            vec![OwnedExcelValue::Number(1.0), OwnedExcelValue::Number(2.0)],
        )
        .unwrap();
        let pointer = allocate_local_for_test(OwnedExcelValue::Matrix(matrix)).unwrap();
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
    fn encoded_array_buffer_is_adopted_without_copying_cells() {
        let _test = test_lock();
        let mut builder = crate::XlArrayBuilder::numbers(1, 2).unwrap();
        builder.push_f64(10.0).unwrap();
        builder.push_f64(20.0).unwrap();
        let encoded = builder.finish().unwrap();
        let original_cells = encoded.cells.as_ptr();
        let pointer = allocate_local_for_test(OwnedExcelValue::ArrayOutput(encoded)).unwrap();
        // SAFETY: pointer is a live encoded array return.
        let returned_cells = unsafe { (*pointer).value.array.values };
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
    fn missing_and_blank_returns_are_explicit_not_available_errors() {
        let _test = test_lock();
        for value in [OwnedExcelValue::Missing, OwnedExcelValue::Blank] {
            let pointer = allocate_local_for_test(value).unwrap();
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
        let runtime = open_test_runtime();
        let error_pointer = ffi_boundary(&runtime, || {
            Err::<f64, _>(XllError::input("x", crate::InputError::NonFinite))
        });
        // SAFETY: pointer is a live encoded error.
        assert_eq!(unsafe { (*error_pointer).base_type() }, XLTYPE_ERR);
        // SAFETY: XLTYPE_ERR selects error.
        // SAFETY: XLTYPE_ERR selects the error union member.
        let error_code = unsafe { (*error_pointer).value.error };
        assert_eq!(error_code, ExcelError::Value.code());
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(error_pointer) };

        let panic_pointer =
            ffi_boundary(&runtime, || -> XllResult<f64> { panic!("boundary test") });
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
        let runtime = open_test_runtime();
        let pointer = ffi_boundary(&runtime, || Ok(PanickingReturn));
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
        crate::ingress::global_ingress().begin_close_with(|| {});
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer = std::thread::spawn(move || {
            let _ = crate::ingress::global_ingress().seal_and_drain();
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
        let runtime = Arc::new(Runtime::new());
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        let pointer = ffi_boundary(&runtime, || Ok(7.0));
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

        let pointer = ffi_boundary(&runtime, || Ok(7.0));
        assert!(!pointer.is_null());
        // SAFETY: Admission rejection returns the permanently owned detached closing
        // error singleton, which deliberately does not carry XLBIT_DLL_FREE.
        unsafe {
            assert_eq!((*pointer).xltype & xlfn_sys::XLBIT_DLL_FREE, 0);
        }
        assert!(is_detached_error_pointer(pointer));

        let second = ffi_boundary(&runtime, || Ok(8.0));
        assert_eq!(pointer, second);
        assert!(is_detached_error_pointer(second));
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

        {
            let recorded = events.lock().unwrap();
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0].0, "test_conversion");
            assert_eq!(recorded[0].1, UdfResultKind::InputError);
            assert_eq!(recorded[0].2, 1);
        }

        let panic_pointer = udf_boundary_named(
            &runtime,
            "test_panic",
            "TEST.PANIC",
            |_| -> XllResult<f64> { panic!("injected UDF panic") },
        );
        // SAFETY: this test owns the live return pointer.
        unsafe { free_return(panic_pointer) };

        let recorded = events.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[1].0, "test_panic");
        assert_eq!(recorded[1].1, UdfResultKind::Panic);
    }

    #[test]
    fn owned_values_cannot_bypass_scalar_validation() {
        let _test = test_lock();
        let runtime = open_test_runtime();
        let pointer = ffi_boundary(&runtime, || Ok(OwnedExcelValue::Number(f64::NAN)));
        // SAFETY: pointer is a live encoded validation error.
        assert_eq!(unsafe { (*pointer).base_type() }, XLTYPE_ERR);
        // SAFETY: pointer has not yet been freed.
        unsafe { free_return(pointer) };
    }

    #[test]
    fn scalar_returns_do_not_evaluate_input_fingerprints() {
        let runtime: Runtime<()> = Runtime::new();
        crate::with_excel_call_scope(|scope| {
            let mut context = ReturnContext::for_call(&runtime, "scalar", None, scope);
            let value = <f64 as crate::ExcelReturn>::invoke(&mut context, || Ok(4.5)).unwrap();
            assert_eq!(value, 4.5);
        });
    }

    #[test]
    fn async_return_allocation_does_not_set_xlbit_dll_free() {
        let _test = test_lock();
        let runtime = open_test_runtime();
        let excel_ptr = ffi_boundary(&runtime, || Ok(42.0));
        let async_ptr = allocate_local_async_return(OwnedExcelValue::Number(42.0)).unwrap();

        // SAFETY: both pointers are valid ReturnBlock pointers
        unsafe {
            assert_ne!((*excel_ptr).xltype & xlfn_sys::XLBIT_DLL_FREE, 0);
            assert_eq!((*async_ptr.as_ptr()).xltype & xlfn_sys::XLBIT_DLL_FREE, 0);

            free_return(excel_ptr);
            free_return(async_ptr.as_ptr());
        }
    }

    #[test]
    fn producer_entry_is_linearized_against_close() {
        for _ in 0..10_000 {
            let tracker = Arc::new(ReturnTracker::new_closed());
            tracker.reopen_admission().unwrap();

            let barrier = Arc::new(Barrier::new(2));

            let producer_tracker = Arc::clone(&tracker);
            let producer_barrier = Arc::clone(&barrier);
            let producer = std::thread::spawn(move || {
                producer_barrier.wait();
                let guard = producer_tracker.try_enter_producer();
                let admitted = guard.is_some();
                drop(guard);
                admitted
            });

            barrier.wait();
            tracker.close_admission();

            let _admitted = producer.join().unwrap();

            tracker.wait_for_quiescence();
            assert!(tracker.is_quiescent());
        }
    }

    #[test]
    fn closed_admission_rejects_all_producers() {
        let tracker = Arc::new(ReturnTracker::new_closed());

        std::thread::scope(|scope| {
            for _ in 0..32 {
                let tracker_ref = &tracker;
                scope.spawn(move || {
                    for _ in 0..10_000 {
                        assert!(tracker_ref.try_enter_producer().is_none());
                    }
                });
            }
        });

        assert!(tracker.is_quiescent());
    }

    #[test]
    fn quiescent_lost_wakeup_stress_test() {
        let _test = test_lock();
        for _ in 0..200 {
            let runtime = Arc::new(open_test_runtime());

            let barrier = Arc::new(Barrier::new(2));
            let runtime_waiter = Arc::clone(&runtime);
            let barrier_waiter = Arc::clone(&barrier);
            let waiter_handle = std::thread::spawn(move || {
                barrier_waiter.wait();
                runtime_waiter.wait_for_returns();
            });

            let runtime_prod = Arc::clone(&runtime);
            let producer_handle = std::thread::spawn(move || {
                let ptr = ffi_boundary(&runtime_prod, || Ok(42.0));
                // SAFETY: ptr is a live return pointer produced by ffi_boundary above.
                let free_guard = unsafe { free_return_boundary(ptr) };
                drop(free_guard);
            });

            producer_handle.join().unwrap();
            runtime.return_tracker().close_admission();
            barrier.wait();
            waiter_handle.join().unwrap();
            assert!(runtime.returns_are_quiescent());
        }
    }

    #[test]
    fn obligation_transfer_does_not_change_count() {
        let tracker = Arc::new(ReturnTracker::new_closed());
        tracker.reopen_admission().unwrap();

        let mut producer = tracker.try_enter_producer().unwrap();
        assert_eq!(tracker.outstanding_obligations(), 1);

        let ptr = PreparedReturn::encode(OwnedExcelValue::Number(42.0))
            .unwrap()
            .publish_excel(&mut producer);
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
    }

    #[test]
    fn return_obligation_can_be_released_on_another_thread() {
        let _test = test_lock();
        let runtime = Arc::new(open_test_runtime());
        let pointer = ffi_boundary(&runtime, || Ok(42.0));
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
        let tracker = Arc::new(ReturnTracker::new_closed());
        tracker.reopen_admission().unwrap();

        let mut producer = tracker.try_enter_producer().unwrap();
        assert_eq!(tracker.outstanding_obligations(), 1);

        let err_res = PreparedReturn::encode(OwnedExcelValue::Number(f64::NAN));
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
    fn panic_error_uses_same_obligation() {
        let _test = test_lock();
        let runtime = open_test_runtime();

        let ptr = udf_boundary_named(
            &runtime,
            "test_panic_obligation",
            "TEST.PANIC_OBLIGATION",
            |_| -> XllResult<f64> { panic!("injected UDF panic") },
        );

        let tracker = runtime.return_tracker();
        assert_eq!(tracker.outstanding_obligations(), 1);

        // SAFETY: ptr is a live panic error ReturnBlock produced by udf_boundary_named above.
        let free_guard = unsafe { free_return_boundary(ptr) };
        assert_eq!(tracker.outstanding_obligations(), 1);
        drop(free_guard);
        assert_eq!(tracker.outstanding_obligations(), 0);
    }

    #[test]
    fn string_backing_survives_in_matrix_return() {
        let matrix = Matrix::new(1, 2, vec!["hello".to_string(), "world".to_string()]).unwrap();
        let value = matrix.into_excel_value().unwrap();
        assert!(matches!(value, OwnedExcelValue::ArrayOutput(_)));

        let prepared = PreparedReturn::encode(value).unwrap();
        let cells = prepared.array.as_ref().unwrap();
        // SAFETY: array values pointer is non-null and valid.
        unsafe {
            let str0 = cells[0].value.string;
            let str1 = cells[1].value.string;
            let len0 = *str0 as usize;
            let len1 = *str1 as usize;
            let s0 = String::from_utf16(std::slice::from_raw_parts(str0.add(1), len0)).unwrap();
            let s1 = String::from_utf16(std::slice::from_raw_parts(str1.add(1), len1)).unwrap();
            assert_eq!(s0, "hello");
            assert_eq!(s1, "world");
        }
    }

    #[test]
    fn uninstrumented_udf_does_not_allocate_call_id() {
        let _test = test_lock();
        let runtime = open_test_runtime();

        let before = runtime.peek_next_call_id();

        for _ in 0..100 {
            let ptr =
                udf_boundary_named(&runtime, "test_fast_path", "TEST.FAST_PATH", |_| Ok(42.0));
            // SAFETY: ptr is a live ReturnBlock produced above.
            let free_guard = unsafe { free_return_boundary(ptr) };
            drop(free_guard);
        }

        let after = runtime.peek_next_call_id();
        assert_eq!(before, after);
    }
}
