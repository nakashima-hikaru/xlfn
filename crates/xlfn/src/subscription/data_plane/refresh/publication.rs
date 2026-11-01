//! Exact-batch marshalling and local output handoff.
//!
//! The backend traits and constructors are private. Ordinary callers can only
//! marshal their consumed batch with the concrete Windows Automation backend.
use super::*;

trait ArrayApi {
    type Array: Copy;
    type Data;
    fn create(count: u32) -> Result<Self::Array, i32>;
    fn access(array: Self::Array) -> Result<Self::Data, i32>;
    fn fill(data: Self::Data, updates: &[RtdUpdate]) -> Result<(), i32>;
    fn unaccess(array: Self::Array) -> Result<(), i32>;
    fn destroy(array: Self::Array);
    fn retained(array: Self::Array, status: i32);
}

enum MarshalFailure {
    InvalidCount,
    Backend(i32),
}

struct BuildingArray<A: ArrayApi> {
    array: Option<A::Array>,
    locked: bool,
}

impl<A: ArrayApi> BuildingArray<A> {
    fn unlock(&mut self) -> Result<(), i32> {
        if !self.locked {
            return Ok(());
        }
        // Disarm before invoking the API: an indeterminate failure must never
        // be retried by Drop, nor followed by destroying a still-locked array.
        self.locked = false;
        // Move the allocation out before the call too: if the backend unwinds,
        // self's Drop cannot mistake an indeterminate lock for an unlocked one.
        let array = self
            .array
            .take()
            .expect("locked construction owns an array");
        // A::Array is a raw, non-dropping handle. Until the call returns Ok,
        // it is indeterminate ownership, not a ReadyArray; unwind retains it.
        if let Err(status) = A::unaccess(array) {
            A::retained(array, status);
            return Err(status);
        }
        self.array = Some(array);
        Ok(())
    }
}

impl<A: ArrayApi> Drop for BuildingArray<A> {
    fn drop(&mut self) {
        let _ = self.unlock();
        if let Some(array) = self.array.take() {
            A::destroy(array);
        }
    }
}

struct ReadyArray<A: ArrayApi> {
    array: Option<A::Array>,
    count: i32,
}

impl<A: ArrayApi> ReadyArray<A> {
    fn prepare(updates: &[RtdUpdate]) -> Result<Self, MarshalFailure> {
        let count = i32::try_from(updates.len()).map_err(|_| MarshalFailure::InvalidCount)?;
        if updates.is_empty() {
            return Ok(Self { array: None, count });
        }
        updates
            .len()
            .checked_mul(2)
            .ok_or(MarshalFailure::InvalidCount)?;
        let array = A::create(count as u32).map_err(MarshalFailure::Backend)?;
        let mut construction = BuildingArray::<A> {
            array: Some(array),
            locked: false,
        };
        // An unwind from an access with indeterminate side effects must retain
        // the allocation too. Only a returned error confirms no lock acquired.
        construction.array = None;
        // The local raw handle has no Drop. An unwind retains/leaks it without
        // pretending that access has completed or an unlocked array is ready.
        let accessed = A::access(array);
        construction.array = Some(array);
        let data = accessed.map_err(MarshalFailure::Backend)?;
        construction.locked = true;
        A::fill(data, updates).map_err(MarshalFailure::Backend)?;
        construction.unlock().map_err(MarshalFailure::Backend)?;
        Ok(Self {
            array: construction.array.take(),
            count,
        })
    }

    fn relinquish(mut self) {
        self.array.take();
    }
}

impl<A: ArrayApi> Drop for ReadyArray<A> {
    fn drop(&mut self) {
        if let Some(array) = self.array.take() {
            A::destroy(array);
        }
    }
}

trait OutputSlots<A: ArrayApi> {
    fn write_count(&mut self, count: i32) -> Result<(), i32>;
    fn write_array(&mut self, array: Option<A::Array>) -> Result<(), i32>;
    fn reset(&mut self);
}

/// Lifetime-scoped write responsibility over host-observable output storage.
/// Raw pointers intentionally avoid protecting that storage with Rust &mut
/// references while completion can reenter a host that reads the handoff.
pub(crate) struct RawOutputSlots<'out, T> {
    count: std::ptr::NonNull<i32>,
    array: std::ptr::NonNull<*mut T>,
    _lifetime: std::marker::PhantomData<&'out mut (i32, *mut T)>,
}

impl<T> RawOutputSlots<'_, T> {
    unsafe fn initialize(count: *mut i32, array: *mut *mut T) -> Option<Self> {
        if count.is_null() || array.is_null() {
            // SAFETY: each non-null pointer satisfies the boundary's writable
            // storage contract even if its other output argument is missing.
            if !count.is_null() {
                // SAFETY: this non-null output satisfies the caller contract.
                unsafe { count.write(0) };
            }
            if !array.is_null() {
                // SAFETY: this non-null output satisfies the caller contract.
                unsafe { array.write(std::ptr::null_mut()) };
            }
            return None;
        }
        let count_end = count.addr().checked_add(std::mem::size_of::<i32>())?;
        let array_end = array.addr().checked_add(std::mem::size_of::<*mut T>())?;
        if count.addr() < array_end && array.addr() < count_end {
            return None;
        }
        let mut output = Self {
            count: std::ptr::NonNull::new(count)?,
            array: std::ptr::NonNull::new(array)?,
            _lifetime: std::marker::PhantomData,
        };
        output.clear();
        Some(output)
    }

    fn clear(&mut self) {
        // SAFETY: boundary validation retains both disjoint writable locations.
        unsafe {
            self.count.as_ptr().write(0);
        }
        // SAFETY: the separately validated array output remains writable too.
        unsafe {
            self.array.as_ptr().write(std::ptr::null_mut());
        }
    }
}

impl<T, A: ArrayApi<Array = std::ptr::NonNull<T>>> OutputSlots<A> for RawOutputSlots<'_, T> {
    fn write_count(&mut self, count: i32) -> Result<(), i32> {
        // SAFETY: this owner controls writes to the retained output location.
        unsafe {
            self.count.as_ptr().write(count);
        }
        Ok(())
    }
    fn write_array(&mut self, array: Option<std::ptr::NonNull<T>>) -> Result<(), i32> {
        // SAFETY: no long-lived Rust reference is created for this handoff.
        unsafe {
            self.array
                .as_ptr()
                .write(array.map_or(std::ptr::null_mut(), std::ptr::NonNull::as_ptr));
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.clear();
    }
}

struct StagedOutputs<A: ArrayApi, O: OutputSlots<A>> {
    output: O,
    armed: bool,
    _array: std::marker::PhantomData<fn(A) -> A>,
}

impl<A: ArrayApi, O: OutputSlots<A>> Drop for StagedOutputs<A, O> {
    fn drop(&mut self) {
        if self.armed {
            self.output.reset();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum HandoffStage {
    CountWritten,
    PointerWritten,
    Committed,
}

/// Its private fields bind this exact array to the batch used to build it.
#[allow(
    private_bounds,
    reason = "only the private audited backend can construct publication authority"
)]
#[must_use = "dropping an unpublished response destroys its array and restores its batch"]
pub(crate) struct RefreshPublication<'a, H: SubscriptionHost, A: ArrayApi> {
    // Field order also governs ordinary Drop before handoff.
    array: ReadyArray<A>,
    batch: RtdRefreshBatch<'a, H>,
}

#[allow(
    private_bounds,
    reason = "backend and construction remain private rather than mintable by callers"
)]
impl<'a, H: SubscriptionHost, A: ArrayApi> RefreshPublication<'a, H, A> {
    fn prepare(batch: RtdRefreshBatch<'a, H>) -> Result<Self, MarshalFailure> {
        let array = match ReadyArray::<A>::prepare(batch.updates()) {
            Ok(array) => array,
            Err(error) => {
                // Array construction has already unlocked/destroyed or retained
                // its allocation before rollback can notify/reenter the core.
                let _ = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| batch.abort()));
                return Err(error);
            }
        };
        Ok(Self { array, batch })
    }

    fn handoff_with<O: OutputSlots<A>>(
        self,
        output: O,
        mut after: impl FnMut(HandoffStage),
    ) -> Result<RefreshHandoff, i32> {
        // Local drop order is output reset, array destruction, batch rollback.
        // Thus even a reentrant rollback notifier sees empty failed outputs.
        let Self { batch, array } = self;
        let mut staged = StagedOutputs::<A, O> {
            output,
            armed: true,
            _array: std::marker::PhantomData,
        };
        staged.output.write_count(array.count)?;
        after(HandoffStage::CountWritten);
        staged.output.write_array(array.array)?;
        after(HandoffStage::PointerWritten);
        array.relinquish();
        staged.armed = false;
        let committed = CommittedRefresh { batch: Some(batch) };
        // End the output borrow before completion can call a notifier/reenter.
        // The committed guard already owns recovery if an output Drop unwinds.
        drop(staged);
        after(HandoffStage::Committed);
        Ok(RefreshHandoff {
            completion: committed.finish(),
        })
    }
}

/// The array has transferred into local COM output storage. This result says
/// nothing about Excel displaying or consuming it.
pub(crate) struct RefreshHandoff {
    completion: XllResult<()>,
}

impl RefreshHandoff {
    pub(crate) fn completion(self) -> XllResult<()> {
        self.completion
    }
}

struct CommittedRefresh<'a, H: SubscriptionHost> {
    batch: Option<RtdRefreshBatch<'a, H>>,
}

impl<H: SubscriptionHost> CommittedRefresh<'_, H> {
    fn finish(mut self) -> XllResult<()> {
        self.finish_pending()
    }

    fn finish_pending(&mut self) -> XllResult<()> {
        let Some(mut batch) = self.batch.take() else {
            return Ok(());
        };
        let mut result = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
            batch.finish_protocol(RefreshOutcome::Delivered)
        }))
        .unwrap_or(Err(XllError::Panic));
        // Admission is still held while the issuer's cleanup ledger is touched.
        if result.is_err() {
            let _ = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
                batch
                    .publish
                    .services()
                    .record_cleanup_result(result.clone());
            }));
        }
        // Once handoff committed, a guard Drop panic cannot invalidate or free
        // the transferred array. Do not touch runtime services after release.
        if crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| drop(batch))).is_err() {
            result = Err(XllError::Panic);
        }
        result
    }
}

impl<H: SubscriptionHost> Drop for CommittedRefresh<'_, H> {
    fn drop(&mut self) {
        let _ = self.finish_pending();
    }
}

#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
mod windows {
    use super::*;
    use crate::excel_rtd::WindowsRefreshArrayApi as WindowsApi;
    #[cfg(test)]
    use crate::win32::S_OK;
    use crate::win32::{E_INVALIDARG, E_POINTER, SAFEARRAY, VARIANT};
    use std::ptr::NonNull;

    impl ArrayApi for WindowsApi {
        type Array = NonNull<SAFEARRAY>;
        type Data = *mut VARIANT;
        fn create(count: u32) -> Result<Self::Array, i32> {
            Self::create(count)
        }
        fn access(array: Self::Array) -> Result<Self::Data, i32> {
            // SAFETY: only the private core supplies its fresh retained array.
            unsafe { Self::access(array) }
        }
        fn fill(data: Self::Data, updates: &[RtdUpdate]) -> Result<(), i32> {
            // SAFETY: the private construction owner retains the successful
            // lock and the create-time VARIANT layout validation.
            unsafe { Self::fill(data, updates) }
        }
        fn unaccess(array: Self::Array) -> Result<(), i32> {
            // SAFETY: the construction owns the one successful data access.
            unsafe { Self::unaccess(array) }
        }
        fn destroy(array: Self::Array) {
            // SAFETY: only an unlocked, untransferred owner reaches destruction.
            unsafe { Self::destroy(array) }
        }
        fn retained(array: Self::Array, status: i32) {
            Self::retained(array, status);
        }
    }

    fn marshal_status(error: MarshalFailure) -> i32 {
        match error {
            MarshalFailure::InvalidCount => E_INVALIDARG,
            MarshalFailure::Backend(status) => status,
        }
    }

    /// Concrete Windows output adapter for the shared raw-output control.
    pub(crate) type RefreshOutputSlots<'out> = RawOutputSlots<'out, SAFEARRAY>;

    impl<'out> RefreshOutputSlots<'out> {
        /// # Safety
        /// Every non-null pointer must remain writable for this call. The
        /// caller grants writes through staging/reset, then may observe the
        /// transferred outputs during completion's reentrant notifications.
        pub(crate) unsafe fn from_raw(
            count: *mut i32,
            array: *mut *mut SAFEARRAY,
        ) -> Result<Self, i32> {
            // SAFETY: forwarded boundary contract; initialize checks null and
            // overlapping ranges without borrowing host memory as Rust &mut.
            unsafe { Self::initialize(count, array) }.ok_or(E_POINTER)
        }
    }

    impl<'a, H: SubscriptionHost> RtdRefreshBatch<'a, H> {
        pub(crate) fn marshal(self) -> Result<RefreshPublication<'a, H, WindowsApi>, i32> {
            RefreshPublication::prepare(self).map_err(marshal_status)
        }
    }

    impl<H: SubscriptionHost> RefreshPublication<'_, H, WindowsApi> {
        /// A successful return contains the completion result separately from
        /// the already committed local output ownership transfer.
        pub(crate) fn handoff(self, output: RefreshOutputSlots<'_>) -> Result<RefreshHandoff, i32> {
            self.handoff_with(output, |_| {})
        }
    }

    #[cfg(test)]
    pub(crate) unsafe fn write_refresh_data_for_test(
        count: *mut i32,
        array: *mut *mut SAFEARRAY,
        updates: &[RtdUpdate],
    ) -> i32 {
        // SAFETY: forwarded test caller retains writable output storage.
        let output = match unsafe { RefreshOutputSlots::from_raw(count, array) } {
            Ok(output) => output,
            Err(status) => return status,
        };
        let ready = match ReadyArray::<WindowsApi>::prepare(updates) {
            Ok(ready) => ready,
            Err(error) => return marshal_status(error),
        };
        let mut staged = StagedOutputs::<WindowsApi, _> {
            output,
            armed: true,
            _array: std::marker::PhantomData,
        };
        if let Err(status) = <RefreshOutputSlots<'_> as OutputSlots<WindowsApi>>::write_count(
            &mut staged.output,
            ready.count,
        )
        .and_then(|()| {
            <RefreshOutputSlots<'_> as OutputSlots<WindowsApi>>::write_array(
                &mut staged.output,
                ready.array,
            )
        }) {
            return status;
        }
        ready.relinquish();
        staged.armed = false;
        S_OK
    }
}

#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
pub(crate) use windows::RefreshOutputSlots;
#[cfg(all(test, target_os = "windows", any(feature = "rtd", feature = "handles")))]
pub(crate) use windows::write_refresh_data_for_test;

#[cfg(all(test, feature = "rtd", not(target_os = "windows")))]
mod tests;
