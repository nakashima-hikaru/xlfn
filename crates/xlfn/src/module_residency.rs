//! Physical DLL residency lease for generated Excel entry points.
//!
//! The logical lifecycle is allowed to reach `Closed` while this lease is
//! still held. That distinction is what makes an ordinary `xlAutoClose` host
//! hint harmless: Excel can return from the hint without unloading code that
//! still owns the runtime. The lease is released only after an explicit
//! `xlAutoRemove` teardown has completed and the following `xlAutoClose` is
//! observed.

use crate::XllResult;
#[cfg(target_os = "windows")]
use crate::error::{DiagnosticId, XllError};

#[derive(Debug)]
pub(crate) struct ModuleResidencyLease {
    #[cfg(target_os = "windows")]
    module: usize,
}

impl ModuleResidencyLease {
    pub(crate) fn acquire(anchor: *const ()) -> XllResult<Self> {
        #[cfg(target_os = "windows")]
        {
            if anchor.is_null() {
                return Err(XllError::Internal {
                    diagnostic_id: DiagnosticId::MODULE_RESIDENCY,
                });
            }

            let mut module = core::ptr::null_mut();
            // SAFETY: `anchor` is the address of a generated entry point in
            // this DLL. With FROM_ADDRESS, Windows interprets the value as
            // an address rather than dereferencing it as a UTF-16 string.
            let acquired = unsafe {
                crate::win32::GetModuleHandleExW(
                    crate::win32::GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                    anchor.cast(),
                    &mut module,
                )
            } != 0;
            if !acquired || module.is_null() {
                return Err(XllError::Internal {
                    diagnostic_id: DiagnosticId::MODULE_RESIDENCY,
                });
            }

            Ok(Self {
                module: module as usize,
            })
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = anchor;
            Ok(Self {})
        }
    }

    pub(crate) fn try_release(&mut self) -> XllResult<()> {
        #[cfg(target_os = "windows")]
        {
            if self.module == 0 {
                return Ok(());
            }
            // SAFETY: the handle was returned by GetModuleHandleExW and has
            // not been released while it remains non-zero.
            let released = unsafe { crate::win32::FreeLibrary(self.module as _) } != 0;
            if !released {
                return Err(XllError::Internal {
                    diagnostic_id: DiagnosticId::MODULE_RESIDENCY,
                });
            }
            self.module = 0;
        }

        Ok(())
    }
}

impl Drop for ModuleResidencyLease {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            if self.try_release().is_err() {
                tracing::error!("failed to release the xlfn module residency lease");
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
        let lease = ModuleResidencyLease::acquire(residency_probe_anchor as *const ());
        assert!(
            lease.is_ok(),
            "GetModuleHandleExW failed for an in-module address"
        );
        drop(lease);
    }
}
