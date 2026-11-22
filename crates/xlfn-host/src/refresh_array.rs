//! Ownership of an Automation refresh array, independent of RTD batches.
//! The host adapter prepares and unlocks it; the facade separately owns all
//! batch-delivery authority and determines when output writes have committed.

/// Adapter for native allocation, access and release. The facade's test
/// backend uses this same ownership path to exercise interrupted handoffs.
///
/// # Safety
/// Handles must be non-owning bitwise copies. Successful creation returns a
/// fresh, unlocked array for exactly the requested number of topic/value
/// pairs. A returned access error must leave that array unlocked; successful
/// access transfers one lock. Only a successful unaccess proves unlocked
/// ownership. Implementations must respect the documented destruction contract.
pub unsafe trait ArrayApi {
    type Update;
    type Array: Copy;
    type Data;
    fn create(count: u32) -> Result<Self::Array, i32>;
    /// # Safety
    /// The array must be live, unlocked and uniquely owned.
    unsafe fn access(array: Self::Array) -> Result<Self::Data, i32>;
    /// # Safety
    /// Data must be the live exclusive access of a fresh array created for
    /// this exact slice length. The caller must release the access afterward.
    unsafe fn fill(data: Self::Data, updates: &[Self::Update]) -> Result<(), i32>;
    /// # Safety
    /// The caller must own the data access and stop using the data before release.
    unsafe fn unaccess(array: Self::Array) -> Result<(), i32>;
    /// # Safety
    /// The array must be live, unlocked and untransferred. Do not reuse it.
    unsafe fn destroy(array: Self::Array);
    fn retained(array: Self::Array, status: i32);
}

#[derive(Debug, PartialEq, Eq)]
pub enum MarshalFailure {
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
        // SAFETY: the guard retained exactly one access and no data borrow.
        if let Err(status) = unsafe { A::unaccess(array) } {
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
            // SAFETY: any unresolved lock disarmed ownership before the call.
            unsafe { A::destroy(array) };
        }
    }
}

#[must_use = "dropping the unpublished array destroys it"]
pub struct ReadyArray<A: ArrayApi> {
    array: Option<A::Array>,
    count: i32,
}

impl<A: ArrayApi> ReadyArray<A> {
    pub fn prepare(updates: &[A::Update]) -> Result<Self, MarshalFailure> {
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
        // SAFETY: create supplied an unlocked array exclusively owned here.
        let accessed = unsafe { A::access(array) };
        construction.array = Some(array);
        let data = accessed.map_err(MarshalFailure::Backend)?;
        construction.locked = true;
        // SAFETY: this is the data access of the array allocated for updates.
        unsafe { A::fill(data, updates) }.map_err(MarshalFailure::Backend)?;
        construction.unlock().map_err(MarshalFailure::Backend)?;
        Ok(Self {
            array: construction.array.take(),
            count,
        })
    }

    pub fn count(&self) -> i32 {
        self.count
    }

    /// Raw handle while this value still owns it. Do not release or mutate it.
    pub fn array(&self) -> Option<A::Array> {
        self.array
    }

    /// Disarm cleanup after ownership is written to the caller's output.
    pub fn relinquish(mut self) {
        self.array.take();
    }
}

impl<A: ArrayApi> Drop for ReadyArray<A> {
    fn drop(&mut self) {
        if let Some(array) = self.array.take() {
            // SAFETY: any unresolved lock disarmed ownership before the call.
            unsafe { A::destroy(array) };
        }
    }
}
