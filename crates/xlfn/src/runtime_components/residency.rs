//! Physical module residency ownership.

use parking_lot::Mutex;

use crate::module_residency::ModuleResidencyLease;

/// Physical DLL residency is deliberately separate from logical lifecycle
/// state. A quarantined or logically closed runtime can retain this lease.
pub(crate) struct ModuleResidency {
    lease: Mutex<Option<ModuleResidencyLease>>,
}

impl ModuleResidency {
    pub(crate) const fn new() -> Self {
        Self {
            lease: Mutex::new(None),
        }
    }

    pub(crate) fn ensure(&self, anchor: *const ()) -> crate::XllResult<bool> {
        let mut lease = self.lease.lock();
        if lease.is_some() {
            return Ok(false);
        }
        *lease = Some(ModuleResidencyLease::acquire(anchor)?);
        Ok(true)
    }

    pub(crate) fn release(&self) -> crate::XllResult<()> {
        let mut lease = self.lease.lock();
        let Some(residency) = lease.as_mut() else {
            return Ok(());
        };
        residency.try_release()?;
        drop(lease.take());
        Ok(())
    }

    pub(crate) fn is_held(&self) -> bool {
        self.lease.lock().is_some()
    }
}
