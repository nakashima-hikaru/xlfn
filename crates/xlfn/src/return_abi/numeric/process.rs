//! Fixed-layout process accounting shared by cooperating XLL images.
//!
//! Windows uses a pagefile mapping independent of any DLL image. The canonical
//! view and one handle deliberately remain until process exit. No destructor,
//! allocator handle, or executable address is stored in the shared protocol.
use crate::error::InputError;
use crate::{ExcelError, XllError, XllResult};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_os = "windows")]
mod windows;

/// Provisional limit for all participating xlfn FP12 allocations in a process.
pub const MAX_PROCESS_NUMERIC_RETURN_BYTES: usize = 512 * 1024 * 1024;
const SCHEMA: u64 = 0x584c_4650_0001_0048;

// This is a cross-image protocol, not a Rust-owned object. Keep every field
// fixed-width and naturally aligned; never reset an existing mapping. Changing
// this layout requires rejecting old headers, not a new mapping name/quota.
#[repr(C, align(8))]
pub(super) struct ProcessState {
    schema: AtomicU64,
    canonical_view: AtomicU64,
    limit_bytes: AtomicU64,
    reserved_bytes: AtomicU64,
    reserved_buffers: AtomicU64,
    peak_reserved_bytes: AtomicU64,
    published_bytes: AtomicU64,
    published_buffers: AtomicU64,
    peak_published_bytes: AtomicU64,
}

const _: () = assert!(size_of::<ProcessState>() == 72);
const _: () = assert!(align_of::<ProcessState>() == 8);
const _: () = assert!(std::mem::offset_of!(ProcessState, peak_published_bytes) == 64);

impl ProcessState {
    #[cfg(any(not(target_os = "windows"), test))]
    const fn new() -> Self {
        Self {
            schema: AtomicU64::new(0),
            canonical_view: AtomicU64::new(0),
            limit_bytes: AtomicU64::new(0),
            reserved_bytes: AtomicU64::new(0),
            reserved_buffers: AtomicU64::new(0),
            peak_reserved_bytes: AtomicU64::new(0),
            published_bytes: AtomicU64::new(0),
            published_buffers: AtomicU64::new(0),
            peak_published_bytes: AtomicU64::new(0),
        }
    }

    fn initialize(&self) -> XllResult<()> {
        let schema = self
            .schema
            .compare_exchange(0, SCHEMA, Ordering::AcqRel, Ordering::Acquire)
            .unwrap_or_else(|found| found);
        if schema != 0 && schema != SCHEMA {
            return Err(protocol_error());
        }
        let limit = self
            .limit_bytes
            .compare_exchange(
                0,
                MAX_PROCESS_NUMERIC_RETURN_BYTES as u64,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .unwrap_or_else(|found| found);
        if limit != 0 && limit != MAX_PROCESS_NUMERIC_RETURN_BYTES as u64 {
            return Err(protocol_error());
        }
        Ok(())
    }

    fn reserve(&self, bytes: usize, limit: usize) -> XllResult<()> {
        let mut current = self.reserved_bytes.load(Ordering::Relaxed);
        loop {
            let next = current
                .checked_add(bytes as u64)
                .ok_or_else(protocol_error)?;
            if next > limit as u64 {
                return Err(XllError::input(
                    "<numeric process retention>",
                    InputError::TooLarge {
                        limit,
                        actual: usize::try_from(next).unwrap_or(usize::MAX),
                    },
                ));
            }
            match self.reserved_bytes.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.reserved_buffers.fetch_add(1, Ordering::Relaxed);
                    self.peak_reserved_bytes.fetch_max(next, Ordering::Relaxed);
                    return Ok(());
                }
                Err(observed) => current = observed,
            }
        }
    }

    fn release_reserved(&self, bytes: usize) {
        self.reserved_buffers.fetch_sub(1, Ordering::Relaxed);
        self.reserved_bytes
            .fetch_sub(bytes as u64, Ordering::AcqRel);
    }

    fn publish(&self, bytes: usize) {
        let next = self
            .published_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed)
            + bytes as u64;
        self.published_buffers.fetch_add(1, Ordering::Relaxed);
        self.peak_published_bytes.fetch_max(next, Ordering::Relaxed);
    }

    pub(super) fn release_published(&self, bytes: usize) {
        self.published_buffers.fetch_sub(1, Ordering::Relaxed);
        self.published_bytes
            .fetch_sub(bytes as u64, Ordering::Relaxed);
        self.release_reserved(bytes);
    }
}

#[cfg(not(target_os = "windows"))]
static PROCESS_STATE: ProcessState = ProcessState::new();
#[cfg(target_os = "windows")]
static PROCESS_STATE: std::sync::OnceLock<&'static ProcessState> = std::sync::OnceLock::new();

fn shared() -> XllResult<&'static ProcessState> {
    #[cfg(target_os = "windows")]
    {
        if let Some(state) = PROCESS_STATE.get() {
            return Ok(state);
        }
        let attached = windows::attach()?;
        Ok(PROCESS_STATE.get_or_init(|| attached))
    }
    #[cfg(not(target_os = "windows"))]
    {
        PROCESS_STATE.initialize()?;
        Ok(&PROCESS_STATE)
    }
}

fn protocol_error() -> XllError {
    XllError::custom(
        ExcelError::Number,
        "FP12 process accounting protocol is unavailable or incompatible",
    )
}

pub(super) struct Reservation {
    state: &'static ProcessState,
    bytes: usize,
}

impl Reservation {
    pub(super) fn reserve(bytes: usize) -> XllResult<Self> {
        let state = shared()?;
        state.reserve(bytes, MAX_PROCESS_NUMERIC_RETURN_BYTES)?;
        Ok(Self { state, bytes })
    }
    pub(super) const fn state(&self) -> &'static ProcessState {
        self.state
    }
    pub(super) fn publish(&self) {
        self.state.publish(self.bytes);
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.state.release_reserved(self.bytes);
    }
}

pub(super) fn read_metrics(metrics: &mut super::NumericReturnMemoryMetrics) -> XllResult<()> {
    let state = shared()?;
    metrics.process_reserved_bytes = state.reserved_bytes.load(Ordering::Relaxed) as usize;
    metrics.process_reserved_buffers = state.reserved_buffers.load(Ordering::Relaxed) as usize;
    metrics.process_peak_reserved_bytes =
        state.peak_reserved_bytes.load(Ordering::Relaxed) as usize;
    metrics.process_published_bytes = state.published_bytes.load(Ordering::Relaxed) as usize;
    metrics.process_published_buffers = state.published_buffers.load(Ordering::Relaxed) as usize;
    metrics.process_peak_published_bytes =
        state.peak_published_bytes.load(Ordering::Relaxed) as usize;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservations_are_bounded_and_rollback_before_publication() {
        let state = ProcessState::new();
        state.initialize().unwrap();
        state.reserve(64, 128).unwrap();
        state.reserve(64, 128).unwrap();
        assert!(state.reserve(1, 128).is_err());
        assert_eq!(state.reserved_bytes.load(Ordering::Relaxed), 128);
        state.release_reserved(64);
        assert_eq!(state.reserved_bytes.load(Ordering::Relaxed), 64);
        state.release_reserved(64);
        assert_eq!(state.reserved_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(state.reserved_buffers.load(Ordering::Relaxed), 0);
        assert_eq!(state.peak_reserved_bytes.load(Ordering::Relaxed), 128);
    }
    #[test]
    fn failed_allocation_rolls_back_the_owning_reservation() {
        static STATE: ProcessState = ProcessState::new();
        STATE.initialize().unwrap();
        let failed: XllResult<()> = (|| {
            STATE.reserve(41, 128)?;
            let _reservation = Reservation {
                state: &STATE,
                bytes: 41,
            };
            Err(protocol_error())
        })();
        assert!(failed.is_err());
        assert_eq!(STATE.reserved_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(STATE.reserved_buffers.load(Ordering::Relaxed), 0);
        assert_eq!(STATE.published_bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn incompatible_protocol_or_cap_never_resets_live_counters() {
        for invalid_schema in [true, false] {
            let state = ProcessState::new();
            state.initialize().unwrap();
            state.reserve(17, 128).unwrap();
            if invalid_schema {
                state.schema.store(SCHEMA + 1, Ordering::Release);
            } else {
                state.limit_bytes.store(1, Ordering::Release);
            }
            assert!(state.initialize().is_err());
            assert_eq!(state.reserved_bytes.load(Ordering::Relaxed), 17);
        }
    }
    #[test]
    fn reattached_images_keep_old_published_charges_and_share_cap() {
        let state = ProcessState::new();
        state.initialize().unwrap();
        state.reserve(48, 80).unwrap();
        state.publish(48);
        // A fresh image attaches without clearing the earlier image's counters.
        state.initialize().unwrap();
        assert_eq!(state.published_bytes.load(Ordering::Relaxed), 48);
        assert!(state.reserve(33, 80).is_err());
        state.reserve(32, 80).unwrap();
        state.release_reserved(32);
        state.release_published(48);
        assert_eq!(state.reserved_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(state.published_buffers.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn concurrent_reservations_cannot_overshoot_shared_limit() {
        let state = ProcessState::new();
        state.initialize().unwrap();
        std::thread::scope(|scope| {
            let state = &state;
            for _ in 0..8 {
                scope.spawn(move || {
                    for _ in 0..1000 {
                        if state.reserve(17, 64).is_ok() {
                            assert!(state.reserved_bytes.load(Ordering::Relaxed) <= 64);
                            state.release_reserved(17);
                        }
                    }
                });
            }
        });
        assert_eq!(state.reserved_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(state.reserved_buffers.load(Ordering::Relaxed), 0);
        assert!(state.peak_reserved_bytes.load(Ordering::Relaxed) <= 64);
    }
}
