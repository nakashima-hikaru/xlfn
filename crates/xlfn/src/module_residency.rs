//! Physical DLL residency lease for generated Excel entry points.
//!
//! The logical lifecycle is allowed to reach `Closed` while this lease is
//! still held. Safe Add-ins retain it after terminal removal because the
//! framework cannot account for arbitrary user-created executable sources.
//! An explicit `PhysicallyUnloadableAddin` opt-in permits release after
//! `xlAutoClose` completes logical teardown only when no native calculation
//! event registration slot or COM lifetime hold remains. Confirmed, in-flight,
//! and indeterminate registrations, as well as externally retained terminal
//! RTD objects, keep native entry points resident across close and reopen.
//! Logical close can reclaim generation resources with this
//! reference retained; it does not certify physical DLL unload.

use crate::XllResult;
use crate::error::XllError;
use xlfn_host::{ModuleReferenceError, NativeModuleReference};

#[derive(Debug)]
pub(crate) struct ModuleResidencyLease(NativeModuleReference);

fn residency_error(_: ModuleReferenceError) -> XllError {
    crate::error::construct::internal(crate::diagnostics::catalog::MODULE_RESIDENCY)
}

impl ModuleResidencyLease {
    pub(crate) fn acquire(anchor: *const ()) -> XllResult<Self> {
        NativeModuleReference::acquire(anchor)
            .map(Self)
            .map_err(residency_error)
    }

    pub(crate) fn try_release(&mut self) -> XllResult<()> {
        // SAFETY: release is confined to the existing residency protocol.
        // Explicit release follows logical quiescence and host-close evidence;
        // dropping a staged acquisition leaves the host's loading reference.
        unsafe { self.0.try_release() }.map_err(residency_error)
    }
}

impl Drop for ModuleResidencyLease {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            if self.try_release().is_err() {
                let _ =
                    crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                        tracing::error!("failed to release the xlfn module residency lease");
                    }));
            }
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::ModuleResidencyLease;

    #[inline(never)]
    fn residency_probe_anchor() {}

    #[test]
    fn windows_address_probe_acquires_and_releases_a_residency_reference() {
        let mut lease = ModuleResidencyLease::acquire(residency_probe_anchor as *const ())
            .expect("GetModuleHandleExW failed for an in-module address");
        assert!(lease.0.is_held());
        lease
            .try_release()
            .expect("FreeLibrary failed to release the reference");
        assert!(!lease.0.is_held());
        lease
            .try_release()
            .expect("releasing an empty lease must be a no-op");
        drop(lease);
    }
}
