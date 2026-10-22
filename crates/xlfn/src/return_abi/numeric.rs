//! Process-owned FP12 results and per-thread publication epochs.
//!
//! Excel consumes an outer worksheet call's result before starting its next
//! worksheet call on that thread. Only that next outer entry reclaims published
//! buffers. Nested entries never reclaim, and construction uses separate owned
//! allocations. A published allocation has no destructor or module callback:
//! thread exit and physical unload deliberately retain its final epoch.

use crate::error::{DomainErrorCode, InputError};
use crate::{ExcelError, XllError, XllResult};
use std::cell::Cell;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn_sys::FP12;

mod process;
pub use process::MAX_PROCESS_NUMERIC_RETURN_BYTES;

/// Provisional maximum FP12 result size, including its eight-byte ABI header.
///
/// Native Excel load qualification is required before increasing this limit.
pub const MAX_NUMERIC_RETURN_BYTES: usize = 16 * 1024 * 1024;
/// Maximum retained FP12 allocation bytes on one thread, including nesting.
pub const MAX_THREAD_NUMERIC_RETURN_BYTES: usize = 64 * 1024 * 1024;
/// Maximum retained FP12 allocation bytes for one loaded module image.
pub const MAX_MODULE_NUMERIC_RETURN_BYTES: usize = 256 * 1024 * 1024;

#[repr(C)]
struct NumericBlock {
    next: *mut NumericBlock,
    allocation_bytes: usize,
    budget: &'static process::ProcessState,
    fp: FP12,
}

const FP_OFFSET: usize = std::mem::offset_of!(NumericBlock, fp);
const VALUES_OFFSET: usize = FP_OFFSET + std::mem::offset_of!(FP12, array);
const _: () = assert!(std::mem::align_of::<NumericBlock>() == std::mem::align_of::<FP12>());

static MODULE_BYTES: AtomicUsize = AtomicUsize::new(0);
static MODULE_BUFFERS: AtomicUsize = AtomicUsize::new(0);
static MODULE_PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct ThreadState {
    depth: usize,
    head: *mut NumericBlock,
    bytes: usize,
    buffers: usize,
    peak_bytes: usize,
}

impl ThreadState {
    const fn new() -> Self {
        Self {
            depth: 0,
            head: std::ptr::null_mut(),
            bytes: 0,
            buffers: 0,
            peak_bytes: 0,
        }
    }
}

thread_local! {
    static THREAD_STATE: Cell<ThreadState> = const { Cell::new(ThreadState::new()) };
}

// No TLS destructor may free memory still readable by Excel, or call module
// code after physical unload.
const _: () = assert!(!std::mem::needs_drop::<Cell<ThreadState>>());

/// Retained FP12 memory for this thread and the current loaded module image.
///
/// Module totals include buffers abandoned by exited threads. Each result's
/// allocation includes a private intrusive-list header as well as its FP12 ABI
/// bytes. Independently read atomic module counters are approximate during
/// concurrent publication/reclamation. Reloaded module images start new
/// counters. Process counters live outside DLL images and include earlier
/// participating images' retained buffers. The reserved counters include
/// unpublished builders and in-progress allocations, with private headers;
/// published counters cover only buffers already exposed to Excel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NumericReturnMemoryMetrics {
    /// Bytes retained by the current thread.
    pub thread_published_bytes: usize,
    /// Buffers retained by the current thread.
    pub thread_published_buffers: usize,
    /// Largest retained byte count observed on the current thread.
    pub thread_peak_published_bytes: usize,
    /// Bytes retained by this loaded module, including exited threads.
    pub module_published_bytes: usize,
    /// Buffers retained by this loaded module, including exited threads.
    pub module_published_buffers: usize,
    /// Largest retained byte count observed by this module image.
    pub module_peak_published_bytes: usize,
    /// Bytes reserved process-wide, including unpublished/in-progress buffers.
    pub process_reserved_bytes: usize,
    /// Allocation reservations process-wide, including unpublished buffers.
    pub process_reserved_buffers: usize,
    /// Largest process-wide reserved byte count observed.
    pub process_peak_reserved_bytes: usize,
    /// Published bytes process-wide, including earlier participating images.
    pub process_published_bytes: usize,
    /// Published buffers process-wide, including earlier participating images.
    pub process_published_buffers: usize,
    /// Largest process-wide published byte count observed.
    pub process_peak_published_bytes: usize,
}

/// Reads FP12 retention counters without changing UDF execution or ownership.
///
/// Returns an error if the process accounting mapping cannot be attached or its
/// protocol is incompatible. Unavailable accounting is never reported as zero.
pub fn numeric_return_memory_metrics() -> XllResult<NumericReturnMemoryMetrics> {
    THREAD_STATE.with(|slot| {
        let state = slot.get();
        let mut metrics = NumericReturnMemoryMetrics {
            thread_published_bytes: state.bytes,
            thread_published_buffers: state.buffers,
            thread_peak_published_bytes: state.peak_bytes,
            module_published_bytes: MODULE_BYTES.load(Ordering::Relaxed),
            module_published_buffers: MODULE_BUFFERS.load(Ordering::Relaxed),
            module_peak_published_bytes: MODULE_PEAK_BYTES.load(Ordering::Relaxed),
            ..NumericReturnMemoryMetrics::default()
        };
        process::read_metrics(&mut metrics)?;
        Ok(metrics)
    })
}

/// A synchronous worksheet entry, including entries using the Q return ABI.
///
/// Must be entered at the external UDF boundary before executing user code.
/// An outer entry is the host protocol's evidence that its previous result was
/// consumed. Nested entries retain every result published in the active epoch.
pub(crate) struct Fp12CallGuard {
    _thread_bound: PhantomData<std::rc::Rc<()>>,
}

impl Fp12CallGuard {
    pub(crate) fn enter() -> Self {
        THREAD_STATE.with(|slot| {
            let mut state = slot.get();
            if state.depth == 0 {
                let previous = state;
                state.head = std::ptr::null_mut();
                state.bytes = 0;
                state.buffers = 0;
                state.depth = 1;
                slot.set(state);
                // SAFETY: this is the next outer worksheet entry on this
                // thread. Excel has consumed the preceding outer epoch, and
                // no nested call can still be using its results.
                unsafe { reclaim_epoch(previous.head) };
            } else {
                // Stack exhaustion necessarily occurs long before usize depth
                // could overflow. Keep this checked rather than wrap to zero.
                state.depth = state
                    .depth
                    .checked_add(1)
                    .expect("FP12 call depth overflow");
                slot.set(state);
            }
        });
        Self {
            _thread_bound: PhantomData,
        }
    }
}

impl Drop for Fp12CallGuard {
    fn drop(&mut self) {
        THREAD_STATE.with(|slot| {
            let mut state = slot.get();
            debug_assert!(state.depth > 0);
            state.depth -= 1;
            slot.set(state);
        });
    }
}

unsafe fn reclaim_epoch(mut pointer: *mut NumericBlock) {
    while !pointer.is_null() {
        // SAFETY: each node belongs to the preceding completed epoch and has
        // not been released; its private header is initialized at construction.
        let next = unsafe { (*pointer).next };
        // SAFETY: the same retained header stores the original allocation size.
        let bytes = unsafe { (*pointer).allocation_bytes };
        // SAFETY: the original allocation retains its process-lifetime ledger.
        let budget = unsafe { (*pointer).budget };
        let allocation = NonNull::new(pointer).expect("epoch node is non-null");
        // SAFETY: the outer entry establishes that Excel no longer reads this
        // node. No Rust owner remains after publication.
        if unsafe { release_block(allocation, bytes) } {
            budget.release_published(bytes);
            MODULE_BYTES.fetch_sub(bytes, Ordering::Relaxed);
            MODULE_BUFFERS.fetch_sub(1, Ordering::Relaxed);
        }
        // If HeapFree fails, keep both process and module charges. The block
        // is abandoned conservatively rather than reopening quota for it.
        pointer = next;
    }
}

struct NumericAllocation {
    pointer: NonNull<NumericBlock>,
    bytes: usize,
    reservation: ManuallyDrop<process::Reservation>,
}

// SAFETY: an unpublished allocation is exclusively owned, with no pointers
// exposed to Excel. Moving its owner transfers that exclusive ownership.
unsafe impl Send for NumericAllocation {}
// SAFETY: shared access only reads initialized numeric cells. Mutation requires
// an exclusive builder reference; publication consumes the owner.
unsafe impl Sync for NumericAllocation {}

impl NumericAllocation {
    fn new(rows: usize, columns: usize, len: usize) -> XllResult<Self> {
        let abi_bytes = len
            .checked_mul(size_of::<f64>())
            .and_then(|bytes| bytes.checked_add(8))
            .ok_or(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })?;
        enforce_limit(abi_bytes, MAX_NUMERIC_RETURN_BYTES, "<numeric return>")?;
        let bytes = FP_OFFSET + abi_bytes;
        // Charge before allocation, including unpublished builders/async outputs.
        // Allocation failure drops this reservation and restores the budget.
        let reservation = process::Reservation::reserve(bytes)?;
        let pointer = allocate_block(bytes)?;
        // SAFETY: the allocation has space/alignment for the full block and
        // flexible array. Validated positive dimensions fit Excel's i32 axes.
        unsafe {
            pointer.as_ptr().write(NumericBlock {
                next: std::ptr::null_mut(),
                allocation_bytes: bytes,
                budget: reservation.state(),
                fp: FP12 {
                    rows: rows as i32,
                    columns: columns as i32,
                    array: [0.0],
                },
            });
        }
        Ok(Self {
            pointer,
            bytes,
            reservation: ManuallyDrop::new(reservation),
        })
    }

    fn values_pointer(&self) -> *mut f64 {
        // Derive from the allocation rather than the declared [f64; 1] field,
        // so flexible-array accesses cover the whole dynamic allocation.
        self.pointer
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(VALUES_OFFSET)
            .cast()
    }
}

impl Drop for NumericAllocation {
    fn drop(&mut self) {
        // SAFETY: this owner is never retained after publication. Thus an
        // ordinary drop releases only an unpublished, exclusively owned block.
        if unsafe { release_block(self.pointer, self.bytes) } {
            // SAFETY: publication forgets the owner; this is the unique
            // unpublished reservation, and its allocation was just freed.
            unsafe { ManuallyDrop::drop(&mut self.reservation) };
        }
        // A failed free retains the reservation until process exit.
    }
}

/// A validated numeric array stored directly in an FP12-compatible allocation.
///
/// Create it with [`NumericArrayBuilder::finish`] or [`Self::from_f64`]. Normal
/// synchronous UDFs return it through `K%`; `return_abi = "xloper12"` and async
/// UDFs use the ordinary XLOPER12 conversion.
pub struct NumericArrayOutput {
    allocation: NumericAllocation,
    rows: usize,
    columns: usize,
}

impl std::fmt::Debug for NumericArrayOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NumericArrayOutput")
            .field("rows", &self.rows)
            .field("columns", &self.columns)
            .finish_non_exhaustive()
    }
}

impl NumericArrayOutput {
    /// Copies a finite, row-major slice directly into its final FP12 buffer.
    pub fn from_f64(rows: usize, columns: usize, values: &[f64]) -> XllResult<Self> {
        NumericArrayBuilder::from_f64(rows, columns, values)?.finish()
    }

    /// Returns the number of rows.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// Returns the number of columns.
    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
    }

    /// Borrows all validated elements in row-major order.
    #[must_use]
    pub fn as_slice(&self) -> &[f64] {
        // SAFETY: finish requires every element to be initialized. The owner
        // keeps the allocation alive and shared access cannot mutate it.
        unsafe {
            std::slice::from_raw_parts(self.allocation.values_pointer(), self.rows * self.columns)
        }
    }

    pub(crate) fn publish(self, _call: &Fp12CallGuard) -> XllResult<*mut FP12> {
        THREAD_STATE.with(|slot| {
            let mut state = slot.get();
            debug_assert!(state.depth > 0);
            let bytes = self.allocation.bytes;
            let thread_bytes = retained_bytes_after(
                state.bytes,
                bytes,
                MAX_THREAD_NUMERIC_RETURN_BYTES,
                "<numeric thread retention>",
            )?;
            reserve_module_bytes(bytes)?;

            // From here to publication only primitive non-panicking operations
            // remain. The block becomes process owned and has no Rust drop.
            let output = ManuallyDrop::new(self);
            output.allocation.reservation.publish();
            let pointer = output.allocation.pointer.as_ptr();
            // SAFETY: this is an unpublished exclusive block; changing its
            // private list link does not touch any already published FP12.
            unsafe { (*pointer).next = state.head };
            state.head = pointer;
            state.bytes = thread_bytes;
            state.buffers += 1;
            state.peak_bytes = state.peak_bytes.max(thread_bytes);
            slot.set(state);
            MODULE_BUFFERS.fetch_add(1, Ordering::Relaxed);
            // SAFETY: this raw pointer covers the initialized FP12 header and
            // its complete validated array. The epoch retains its allocation.
            Ok(unsafe { pointer.cast::<u8>().add(FP_OFFSET).cast() })
        })
    }
}

/// Builds a finite numeric result directly in its final FP12 allocation.
pub struct NumericArrayBuilder {
    allocation: NumericAllocation,
    rows: usize,
    columns: usize,
    initialized: usize,
}

impl NumericArrayBuilder {
    /// Allocates a builder after checking positive dimensions and byte limits.
    pub fn new(rows: usize, columns: usize) -> XllResult<Self> {
        let len = numeric_element_count(rows, columns)?;
        Ok(Self {
            allocation: NumericAllocation::new(rows, columns, len)?,
            rows,
            columns,
            initialized: 0,
        })
    }

    /// Copies a complete row-major slice, rejecting non-finite values.
    pub fn from_f64(rows: usize, columns: usize, values: &[f64]) -> XllResult<Self> {
        let expected = numeric_element_count(rows, columns)?;
        if expected != values.len() {
            return Err(XllError::ElementCountMismatch {
                rows,
                columns,
                expected,
                actual: values.len(),
            });
        }
        let mut builder = Self::new(rows, columns)?;
        for (index, &value) in values.iter().enumerate() {
            let value = crate::value::output::validate_number(value)?;
            // SAFETY: the validated slice length equals the allocation's cell
            // count. Each index selects one distinct row-major cell.
            let cell = unsafe { builder.allocation.values_pointer().add(index) };
            // SAFETY: the selected cell belongs to this exclusive builder.
            unsafe { cell.write(value) };
        }
        builder.initialized = values.len();
        Ok(builder)
    }

    /// Appends one finite number in row-major order.
    #[inline]
    pub fn push_f64(&mut self, value: f64) -> XllResult<()> {
        let value = crate::value::output::validate_number(value)?;
        if self.initialized == self.rows * self.columns {
            return Err(XllError::input(
                "<numeric return>",
                InputError::Malformed("too many array cells"),
            ));
        }
        // SAFETY: dimensions and byte layout were validated before allocation,
        // and initialized selects the next cell inside the exclusive buffer.
        let cell = unsafe { self.allocation.values_pointer().add(self.initialized) };
        // SAFETY: the checked cell is inside the builder's exclusive buffer.
        unsafe { cell.write(value) };
        self.initialized += 1;
        Ok(())
    }

    /// Completes the result, rejecting an incomplete number of elements.
    pub fn finish(self) -> XllResult<NumericArrayOutput> {
        let expected = self.rows * self.columns;
        if self.initialized != expected {
            return Err(XllError::ElementCountMismatch {
                rows: self.rows,
                columns: self.columns,
                expected,
                actual: self.initialized,
            });
        }
        Ok(NumericArrayOutput {
            allocation: self.allocation,
            rows: self.rows,
            columns: self.columns,
        })
    }
}

fn numeric_element_count(rows: usize, columns: usize) -> XllResult<usize> {
    if rows == 0 || columns == 0 {
        return Err(XllError::input(
            "<numeric return>",
            InputError::Malformed("matrix dimensions must be non-zero"),
        ));
    }
    // FP12 has eight-byte cells. The generic XLOPER12 cell-count cap is
    // intentionally excluded: numeric results are bounded by their own byte
    // budget equally on 32-bit and 64-bit hosts.
    enforce_limit(rows, 1_048_576, "<numeric return rows>")?;
    enforce_limit(columns, 16_384, "<numeric return columns>")?;
    rows.checked_mul(columns).ok_or(XllError::Domain {
        code: DomainErrorCode::Overflow,
    })
}

fn enforce_limit(actual: usize, limit: usize, argument: &'static str) -> XllResult<()> {
    if actual > limit {
        Err(XllError::input(
            argument,
            InputError::TooLarge { actual, limit },
        ))
    } else {
        Ok(())
    }
}

fn reserve_module_bytes(bytes: usize) -> XllResult<()> {
    let mut current = MODULE_BYTES.load(Ordering::Relaxed);
    loop {
        let next = retained_bytes_after(
            current,
            bytes,
            MAX_MODULE_NUMERIC_RETURN_BYTES,
            "<numeric module retention>",
        )?;
        match MODULE_BYTES.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                MODULE_PEAK_BYTES.fetch_max(next, Ordering::Relaxed);
                return Ok(());
            }
            Err(observed) => current = observed,
        }
    }
}

fn retained_bytes_after(
    current: usize,
    additional: usize,
    limit: usize,
    argument: &'static str,
) -> XllResult<usize> {
    let next = current.checked_add(additional).ok_or(XllError::Domain {
        code: DomainErrorCode::Overflow,
    })?;
    enforce_limit(next, limit, argument)?;
    Ok(next)
}

fn allocation_error() -> XllError {
    XllError::custom(ExcelError::Number, "FP12 return buffer allocation failed")
}

#[cfg(target_os = "windows")]
fn allocate_block(bytes: usize) -> XllResult<NonNull<NumericBlock>> {
    windows_link::link!("kernel32.dll" "system" fn GetProcessHeap() -> *mut core::ffi::c_void);
    windows_link::link!("kernel32.dll" "system" fn HeapAlloc(heap: *mut core::ffi::c_void, flags: u32, bytes: usize) -> *mut core::ffi::c_void);
    // SAFETY: GetProcessHeap has no preconditions.
    let heap = unsafe { GetProcessHeap() };
    // SAFETY: HeapAlloc accepts this heap and returns memory aligned for
    // NumericBlock, independent of Rust's allocator.
    let pointer = unsafe { HeapAlloc(heap, 0, bytes) };
    NonNull::new(pointer.cast()).ok_or_else(allocation_error)
}

#[cfg(target_os = "windows")]
unsafe fn release_block(pointer: NonNull<NumericBlock>, _bytes: usize) -> bool {
    windows_link::link!("kernel32.dll" "system" fn GetProcessHeap() -> *mut core::ffi::c_void);
    windows_link::link!("kernel32.dll" "system" fn HeapFree(heap: *mut core::ffi::c_void, flags: u32, memory: *mut core::ffi::c_void) -> i32);
    // SAFETY: GetProcessHeap has no preconditions.
    let heap = unsafe { GetProcessHeap() };
    // SAFETY: only an unpublished owner or a completed epoch calls this, once,
    // with the original HeapAlloc allocation on the same process heap.
    unsafe { HeapFree(heap, 0, pointer.as_ptr().cast()) != 0 }
}

// Test/benchmark hosts retain process ownership explicitly so Miri can see the
// final epoch as reachable after its TLS slot or publishing thread disappears.
#[cfg(not(target_os = "windows"))]
struct ProcessAllocation(NonNull<NumericBlock>);

#[cfg(not(target_os = "windows"))]
// SAFETY: the registry never accesses payloads. Exclusive owners and the
// per-thread epoch protocol govern all mutation and reads.
unsafe impl Send for ProcessAllocation {}

#[cfg(not(target_os = "windows"))]
static PROCESS_ALLOCATIONS: std::sync::Mutex<Vec<ProcessAllocation>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(not(target_os = "windows"))]
fn block_layout(bytes: usize) -> std::alloc::Layout {
    std::alloc::Layout::from_size_align(bytes, std::mem::align_of::<NumericBlock>())
        .expect("validated FP12 layout")
}

#[cfg(not(target_os = "windows"))]
fn allocate_block(bytes: usize) -> XllResult<NonNull<NumericBlock>> {
    let mut allocations = PROCESS_ALLOCATIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    allocations.try_reserve(1).map_err(|_| allocation_error())?;
    // SAFETY: validated result sizes form a non-zero valid layout.
    let pointer = unsafe { std::alloc::alloc(block_layout(bytes)) };
    let pointer = NonNull::new(pointer.cast()).ok_or_else(allocation_error)?;
    allocations.push(ProcessAllocation(pointer));
    Ok(pointer)
}

#[cfg(not(target_os = "windows"))]
unsafe fn release_block(pointer: NonNull<NumericBlock>, bytes: usize) -> bool {
    let mut allocations = PROCESS_ALLOCATIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let index = allocations
        .iter()
        .position(|entry| entry.0 == pointer)
        .expect("FP12 block has one process owner");
    allocations.swap_remove(index);
    // SAFETY: the caller established exclusive release and supplies the same
    // layout as the allocation. The registry no longer retains the pointer.
    unsafe { std::alloc::dealloc(pointer.as_ptr().cast(), block_layout(bytes)) };
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PublishedTestPointer(*mut FP12);

    // SAFETY: tests only read fully published immutable process-owned buffers,
    // and never enter another epoch on their publishing threads after transfer.
    unsafe impl Send for PublishedTestPointer {}

    unsafe fn values<'a>(pointer: *mut FP12) -> &'a [f64] {
        // SAFETY: each test passes a live published FP12 from the active or
        // last completed epoch, with all flexible-array cells initialized.
        let rows = unsafe { (*pointer).rows as usize };
        // SAFETY: the same live initialized header contains the column count.
        let columns = unsafe { (*pointer).columns as usize };
        // SAFETY: derive from the allocation-wide exposed ABI pointer, without
        // creating a reference limited to the declared one-element member.
        let payload = unsafe { pointer.cast::<u8>().add(8).cast() };
        // SAFETY: the caller supplies all rows × columns initialized cells.
        unsafe { std::slice::from_raw_parts(payload, rows * columns) }
    }

    #[test]
    fn miri_numeric_builder_moves_preserve_flexible_array_and_finite_bits() {
        let numbers = [-0.0, 0.0, 1.5, -2.5, f64::MAX, f64::MIN];
        let mut outputs = vec![NumericArrayOutput::from_f64(2, 3, &numbers).unwrap()];
        let output = outputs.pop().unwrap();
        for (actual, expected) in output.as_slice().iter().zip(numbers) {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
        let call = Fp12CallGuard::enter();
        let pointer = output.publish(&call).unwrap();
        // SAFETY: this call still owns the current publication epoch.
        assert_eq!(unsafe { values(pointer) }, &numbers);
        drop(call);
        // SAFETY: dropping the guard never reclaims a published result.
        assert_eq!(unsafe { values(pointer) }, &numbers);
        drop(Fp12CallGuard::enter());
    }

    #[test]
    fn builder_rejects_nonfinite_extra_missing_and_oversize_cells() {
        let mut builder = NumericArrayBuilder::new(1, 2).unwrap();
        builder.push_f64(4.0).unwrap();
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(builder.push_f64(value).is_err());
        }
        assert_eq!(builder.initialized, 1);
        builder.push_f64(5.0).unwrap();
        assert!(builder.push_f64(6.0).is_err());
        assert_eq!(builder.finish().unwrap().as_slice(), &[4.0, 5.0]);
        assert!(NumericArrayBuilder::new(1, 1).unwrap().finish().is_err());
        assert!(NumericArrayBuilder::new(0, 1).is_err());
        assert!(NumericArrayBuilder::new(usize::MAX, 2).is_err());
        assert!(matches!(
            NumericArrayBuilder::new(1024, 2048),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: MAX_NUMERIC_RETURN_BYTES,
                    ..
                },
                ..
            })
        ));
        assert!(NumericArrayOutput::from_f64(1, 2, &[1.0]).is_err());
    }

    #[test]
    fn numeric_shape_uses_fp12_byte_limit_on_every_pointer_width() {
        // This exceeds the generic 32-bit XLOPER12 cell cap, while the FP12
        // allocation is about eight MiB and remains within its own budget.
        let builder = NumericArrayBuilder::new(1024, 1024).unwrap();
        assert_eq!((builder.rows, builder.columns), (1024, 1024));
        drop(builder);
        // Valid Excel axes reach the FP12 byte check before any allocation or
        // generic XLOPER12 element-count rejection.
        assert!(matches!(
            NumericArrayBuilder::new(1_048_576, 4),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: MAX_NUMERIC_RETURN_BYTES,
                    actual: 33_554_440,
                },
                ..
            })
        ));
        assert!(matches!(
            NumericArrayBuilder::new(1_048_577, 1),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 1_048_576,
                    ..
                },
                ..
            })
        ));
        assert!(matches!(
            NumericArrayBuilder::new(1, 16_385),
            Err(XllError::Input {
                reason: InputError::TooLarge { limit: 16_384, .. },
                ..
            })
        ));
    }

    #[test]
    fn miri_nested_publications_keep_every_previous_pointer_live_until_next_outer_entry() {
        let outer = Fp12CallGuard::enter();
        let first = NumericArrayOutput::from_f64(1, 2, &[1.0, 2.0])
            .unwrap()
            .publish(&outer)
            .unwrap();
        let inner = Fp12CallGuard::enter();
        let second = NumericArrayOutput::from_f64(2, 1, &[3.0, 4.0])
            .unwrap()
            .publish(&inner)
            .unwrap();
        drop(inner);
        let inner = Fp12CallGuard::enter();
        // SAFETY: entering another nested call never reclaims its siblings.
        assert_eq!(unsafe { values(first) }, &[1.0, 2.0]);
        // SAFETY: the completed sibling remains in the current outer epoch.
        assert_eq!(unsafe { values(second) }, &[3.0, 4.0]);
        drop(inner);
        let final_result = NumericArrayOutput::from_f64(1, 1, &[9.0])
            .unwrap()
            .publish(&outer)
            .unwrap();
        assert_eq!(
            numeric_return_memory_metrics()
                .unwrap()
                .thread_published_buffers,
            3
        );
        drop(outer);
        // SAFETY: Excel may still read the final result after the wrapper exits.
        assert_eq!(unsafe { values(final_result) }, &[9.0]);
        let next = Fp12CallGuard::enter();
        assert_eq!(
            numeric_return_memory_metrics()
                .unwrap()
                .thread_published_bytes,
            0
        );
        assert_eq!(
            numeric_return_memory_metrics()
                .unwrap()
                .thread_published_buffers,
            0
        );
        drop(next);
    }

    #[test]
    fn miri_parallel_threads_have_independent_epochs_and_final_results_survive_thread_exit() {
        let workers: Vec<_> = (0..4)
            .map(|index| {
                std::thread::spawn(move || {
                    let guard = Fp12CallGuard::enter();
                    let pointer = NumericArrayOutput::from_f64(1, 1, &[index as f64])
                        .unwrap()
                        .publish(&guard)
                        .unwrap();
                    assert_eq!(
                        numeric_return_memory_metrics()
                            .unwrap()
                            .thread_published_buffers,
                        1
                    );
                    drop(guard);
                    (PublishedTestPointer(pointer), index)
                })
            })
            .collect();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        for (pointer, index) in &results {
            // SAFETY: the process owner retains final results after thread exit.
            assert_eq!(unsafe { values(pointer.0) }, &[*index as f64]);
        }
        for left in 0..results.len() {
            for right in left + 1..results.len() {
                assert_ne!(results[left].0.0, results[right].0.0);
            }
        }
    }

    #[test]
    fn repeated_outer_calls_retain_only_one_epoch() {
        for index in 0..100 {
            let call = Fp12CallGuard::enter();
            assert_eq!(
                numeric_return_memory_metrics()
                    .unwrap()
                    .thread_published_buffers,
                0
            );
            NumericArrayOutput::from_f64(1, 1, &[index as f64])
                .unwrap()
                .publish(&call)
                .unwrap();
            assert_eq!(
                numeric_return_memory_metrics()
                    .unwrap()
                    .thread_published_buffers,
                1
            );
        }
        drop(Fp12CallGuard::enter());
        assert_eq!(
            numeric_return_memory_metrics()
                .unwrap()
                .thread_published_bytes,
            0
        );
    }

    #[test]
    fn numeric_publication_preserves_shapes_including_large_output() {
        for (rows, columns) in [(1, 1), (1, 7), (7, 1), (3, 4), (1000, 100)] {
            let numbers: Vec<_> = (0..rows * columns).map(|index| index as f64).collect();
            let call = Fp12CallGuard::enter();
            let pointer = NumericArrayOutput::from_f64(rows, columns, &numbers)
                .unwrap()
                .publish(&call)
                .unwrap();
            // SAFETY: the active epoch retains the initialized result.
            assert_eq!(unsafe { (*pointer).rows }, rows as i32);
            // SAFETY: the same live header exposes the column count.
            assert_eq!(unsafe { (*pointer).columns }, columns as i32);
            // SAFETY: every payload cell remains live in this epoch.
            assert_eq!(unsafe { values(pointer) }, &numbers);
            assert_eq!(
                numeric_return_memory_metrics()
                    .unwrap()
                    .thread_published_bytes,
                FP_OFFSET + 8 + rows * columns * 8
            );
        }
        drop(Fp12CallGuard::enter());
    }

    #[test]
    fn miri_thread_retention_rejection_leaves_previous_result_live() {
        let call = Fp12CallGuard::enter();
        let previous = NumericArrayOutput::from_f64(1, 1, &[7.0])
            .unwrap()
            .publish(&call)
            .unwrap();
        let original = THREAD_STATE.with(|slot| {
            let original = slot.get();
            slot.set(ThreadState {
                bytes: MAX_THREAD_NUMERIC_RETURN_BYTES,
                ..original
            });
            original
        });
        let rejected = NumericArrayOutput::from_f64(1, 1, &[9.0])
            .unwrap()
            .publish(&call);
        THREAD_STATE.with(|slot| slot.set(original));
        assert!(matches!(
            rejected,
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: MAX_THREAD_NUMERIC_RETURN_BYTES,
                    ..
                },
                ..
            })
        ));
        // SAFETY: rejecting a new result never changes the preceding result.
        assert_eq!(unsafe { values(previous) }, &[7.0]);
        drop(call);
        drop(Fp12CallGuard::enter());
    }

    #[test]
    fn retained_byte_budgets_check_exact_limit_and_overflow() {
        for limit in [
            MAX_THREAD_NUMERIC_RETURN_BYTES,
            MAX_MODULE_NUMERIC_RETURN_BYTES,
        ] {
            assert_eq!(
                retained_bytes_after(limit - 16, 16, limit, "test").unwrap(),
                limit
            );
            assert!(matches!(retained_bytes_after(limit, 1, limit, "test"),
                Err(XllError::Input { reason: InputError::TooLarge { actual, .. }, .. }) if actual == limit + 1));
        }
        assert!(matches!(
            retained_bytes_after(usize::MAX, 1, usize::MAX, "test"),
            Err(XllError::Domain {
                code: DomainErrorCode::Overflow
            })
        ));
    }
}
