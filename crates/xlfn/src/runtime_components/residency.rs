//! Physical module residency ownership.

use parking_lot::Mutex;

use crate::module_residency::ModuleResidencyLease;

/// Physical DLL residency is deliberately separate from logical lifecycle
/// state. A quarantined or logically closed runtime can retain this lease.
pub(crate) struct ModuleResidency {
    pub(crate) lease: Mutex<Option<ModuleResidencyLease>>,
}

impl ModuleResidency {
    pub(crate) const fn new() -> Self {
        Self {
            lease: Mutex::new(None),
        }
    }
}
