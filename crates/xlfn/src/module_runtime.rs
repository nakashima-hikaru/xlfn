use crate::callback_gate::{ModuleCallbackAdmission, ModuleCallbackLifecycle};
use crate::ingress::{ExportIngress, ExportsDrained};
#[cfg(any(feature = "rtd", test))]
use crate::rtd::RtdModuleState;
use std::sync::LazyLock;

#[cfg(all(feature = "rtd", target_os = "windows"))]
use crate::rtd::ComModuleLifetime;

/// The module-wide ownership root for protocols that must move together
/// across an open/close epoch.
///
/// The individual components retain their own synchronization and state
/// machines.  This root owns their lifetime and provides the small set of
/// cross-component transitions used by the runtime, so a new protocol cannot
/// accidentally introduce a second module epoch owner.
pub(crate) struct ModuleRuntime {
    ingress: ExportIngress,
    callback_admission: ModuleCallbackAdmission,
    #[cfg(any(feature = "rtd", test))]
    rtd: RtdModuleState,
    #[cfg(all(feature = "rtd", target_os = "windows"))]
    com: ComModuleLifetime,
}

/// Stable identity for one module admission epoch.
///
/// The identity is copyable for diagnostics and validation, while the
/// corresponding open/close authority remains affine in the token types
/// below.
#[derive(Clone, Copy)]
pub(crate) struct ModuleEpochId {
    module: &'static ModuleRuntime,
    epoch: u64,
}

impl PartialEq for ModuleEpochId {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.module, other.module) && self.epoch == other.epoch
    }
}

impl Eq for ModuleEpochId {}

impl std::fmt::Debug for ModuleEpochId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModuleEpochId")
            .field("epoch", &self.epoch)
            .finish()
    }
}

impl ModuleEpochId {
    pub(crate) fn is_current(self) -> bool {
        self.module.ingress().epoch() == self.epoch
    }
}

/// The module epoch that is established while the framework is opening.
///
/// It is deliberately not cloneable.  The opening transaction is the only
/// owner until publication consumes it into a `ModuleEpochLease`.
pub(crate) struct ModuleOpening {
    module: &'static ModuleRuntime,
    id: ModuleEpochId,
}

/// Linear proof that the published runtime belongs to one module admission
/// epoch.  The lease is consumed by terminal certification.
pub(crate) struct ModuleEpochLease {
    module: &'static ModuleRuntime,
    id: ModuleEpochId,
}

/// The single owner-side module authority retained by the runtime lifecycle.
///
/// The epoch identity is copied into generation payloads for validation, but
/// this value is the only place where the affine mutation authority lives
/// while the runtime is not actively tearing down through a removal owner.
pub(crate) enum ModuleAuthority {
    Open(ModuleEpochLease),
    Closing(ModuleClosing),
    Drained(ModuleExportsDrained),
}

/// The cleanup authority retained after a close transition has begun.
///
/// `Drained` is deliberately a distinct state from `Closing`: the ingress
/// close operation has already consumed the latter and must never be
/// reconstructed merely because a later teardown step failed.
pub(crate) enum ModuleCleanupAuthority {
    Closing(ModuleClosing),
    Drained(ModuleExportsDrained),
}

/// Affine module capability after the module close has been linearized.
///
/// The capability owns the right to drain the module ingress.  Callers must
/// advance it to [`ModuleExportsDrained`] before they can certify module
/// quiescence.  It is constructed only by consuming the preceding
/// [`ModuleOpening`] or [`ModuleEpochLease`] authority.
pub(crate) struct ModuleClosing {
    module: &'static ModuleRuntime,
    id: ModuleEpochId,
}

/// Affine module capability after all exports have drained.
pub(crate) struct ModuleExportsDrained {
    module: &'static ModuleRuntime,
    id: ModuleEpochId,
    exports: ExportsDrained,
}

/// Terminal module capability.  Constructing this value is the only normal
/// path that certifies module-wide logical quiescence.
pub(crate) struct ModuleQuiescent {
    id: ModuleEpochId,
}

impl std::fmt::Debug for ModuleEpochLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModuleEpochLease")
            .field("epoch", &self.id.epoch)
            .finish()
    }
}

impl std::fmt::Debug for ModuleQuiescent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModuleQuiescent")
            .field("epoch", &self.id.epoch)
            .finish()
    }
}

impl ModuleOpening {
    pub(crate) fn commit(self) -> ModuleEpochLease {
        ModuleEpochLease {
            module: self.module,
            id: self.id,
        }
    }

    /// Rolls an uncommitted opening epoch into the same affine close chain
    /// used by committed removal.
    pub(crate) fn rollback(self, on_closed: impl FnOnce()) -> ModuleClosing {
        let Self { module, id } = self;
        module.begin_close_effects(id, on_closed);
        ModuleClosing { module, id }
    }
}

impl ModuleEpochLease {
    pub(crate) fn id(&self) -> ModuleEpochId {
        self.id
    }

    pub(crate) fn begin_close<F>(self, on_closed: F) -> ModuleClosing
    where
        F: FnOnce(),
    {
        let Self { module, id } = self;
        module.begin_close_effects(id, on_closed);
        ModuleClosing { module, id }
    }
}

impl ModuleAuthority {
    pub(crate) fn id(&self) -> ModuleEpochId {
        match self {
            Self::Open(lease) => lease.id(),
            Self::Closing(closing) => closing.id(),
            Self::Drained(drained) => drained.id(),
        }
    }

    pub(crate) fn into_closing(self) -> ModuleClosing {
        match self {
            Self::Open(lease) => lease.begin_close(|| {}),
            Self::Closing(closing) => closing,
            Self::Drained(_) => xlfn_kernel::invariant::fail_stop(),
        }
    }

    pub(crate) fn into_cleanup(self) -> ModuleCleanupAuthority {
        match self {
            Self::Open(lease) => ModuleCleanupAuthority::Closing(lease.begin_close(|| {})),
            Self::Closing(closing) => ModuleCleanupAuthority::Closing(closing),
            Self::Drained(drained) => ModuleCleanupAuthority::Drained(drained),
        }
    }
}

impl ModuleCleanupAuthority {
    /// Completes the module-side cleanup without minting a predecessor
    /// capability. A progressed `Drained` authority can only close callback
    /// admission; it cannot be converted back into `ModuleClosing`.
    pub(crate) fn finish(self) {
        match self {
            Self::Closing(closing) => {
                let drained = closing.seal_and_drain();
                drained.close_callbacks();
            }
            Self::Drained(drained) => drained.close_callbacks(),
        }
    }
}

impl ModuleClosing {
    pub(crate) fn id(&self) -> ModuleEpochId {
        self.id
    }

    pub(crate) fn seal_and_drain(self) -> ModuleExportsDrained {
        let exports = self.module.seal_and_drain_internal();
        ModuleExportsDrained {
            module: self.module,
            id: self.id,
            exports,
        }
    }
}

impl ModuleExportsDrained {
    pub(crate) fn id(&self) -> ModuleEpochId {
        self.id
    }

    /// Closes callback admission after the final host callback has completed.
    /// The operation is intentionally available only through this affine
    /// module capability, rather than through `global()`.
    pub(crate) fn close_callbacks(&self) {
        self.module.close_callbacks_internal();
    }

    pub(crate) fn certify(self) -> (ModuleQuiescent, ExportsDrained) {
        self.close_callbacks();
        self.module.certify_logical_quiescence_internal();
        (ModuleQuiescent { id: self.id }, self.exports)
    }
}

impl ModuleQuiescent {
    pub(crate) fn id(&self) -> ModuleEpochId {
        self.id
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let module = global();
        Self {
            id: ModuleEpochId {
                module,
                epoch: module.ingress().epoch(),
            },
        }
    }
}

impl ModuleRuntime {
    fn new() -> Self {
        Self {
            ingress: ExportIngress::new(),
            callback_admission: ModuleCallbackAdmission::new(ModuleCallbackLifecycle::Closed),
            #[cfg(any(feature = "rtd", test))]
            rtd: RtdModuleState::new(),
            #[cfg(all(feature = "rtd", target_os = "windows"))]
            com: ComModuleLifetime::new(),
        }
    }

    pub(crate) fn ingress(&'static self) -> &'static ExportIngress {
        &self.ingress
    }

    pub(crate) fn callback_admission(&'static self) -> &'static ModuleCallbackAdmission {
        &self.callback_admission
    }

    fn reset_callbacks(&'static self) {
        self.callback_admission.reset();
    }

    pub(crate) fn rtd(&'static self) -> Option<&'static crate::rtd::RtdModuleState> {
        #[cfg(any(feature = "rtd", test))]
        {
            Some(&self.rtd)
        }
        #[cfg(not(any(feature = "rtd", test)))]
        None
    }

    #[cfg(all(feature = "rtd", target_os = "windows"))]
    pub(crate) fn com_module_lifetime(&'static self) -> &'static ComModuleLifetime {
        &self.com
    }

    /// Starts the module components for one opening epoch in their canonical
    /// order.  Runtime admission is reopened by `Runtime` immediately before
    /// this transition; this method owns the module-local portion.
    fn begin_open_internal(&'static self) -> ModuleOpening {
        #[cfg(any(feature = "rtd", test))]
        self.rtd.begin_open();
        self.reset_callbacks();
        self.ingress.begin_opening();
        ModuleOpening {
            module: self,
            id: ModuleEpochId {
                module: self,
                epoch: self.ingress.epoch(),
            },
        }
    }

    /// Applies the operational side effects of module close after the caller
    /// has consumed the unique predecessor authority.  This method never
    /// constructs or returns a close capability; the predecessor transition
    /// owns that construction.
    fn begin_close_effects<F>(&'static self, id: ModuleEpochId, on_closed: F)
    where
        F: FnOnce(),
    {
        if !std::ptr::eq(id.module, self) || self.ingress.epoch() != id.epoch {
            xlfn_kernel::invariant::fail_stop();
        }
        self.ingress.begin_close_with(on_closed);
        #[cfg(any(feature = "rtd", test))]
        self.rtd.begin_close();
    }

    fn close_callbacks_internal(&'static self) {
        self.callback_admission.close();
    }

    fn seal_and_drain_internal(&'static self) -> ExportsDrained {
        self.ingress.seal_and_drain()
    }

    fn certify_logical_quiescence_internal(&'static self) {
        #[cfg(any(feature = "rtd", test))]
        self.rtd.certify_logical_quiescence();
    }
}

static MODULE_RUNTIME: LazyLock<ModuleRuntime> = LazyLock::new(ModuleRuntime::new);

pub(crate) fn global() -> &'static ModuleRuntime {
    &MODULE_RUNTIME
}

/// Starts one module opening epoch.  The returned token, rather than the
/// singleton itself, is the mutation authority for the opening transaction.
pub(crate) fn begin_open() -> ModuleOpening {
    MODULE_RUNTIME.begin_open_internal()
}

#[cfg(any(test, feature = "bench-internals"))]
pub(crate) fn reset_callbacks_for_test() {
    MODULE_RUNTIME.reset_callbacks();
}

#[cfg(test)]
pub(crate) fn close_callbacks_for_test() {
    MODULE_RUNTIME.close_callbacks_internal();
}

#[cfg(all(test, target_os = "windows"))]
pub(crate) fn certify_quiescence_for_test() {
    let closing = begin_open().rollback(|| {});
    let drained = closing.seal_and_drain();
    let _ = drained.certify();
}

/// Read/admission capability for the module ingress owned by the singleton
/// module runtime.
pub(crate) fn ingress() -> &'static ExportIngress {
    global().ingress()
}
