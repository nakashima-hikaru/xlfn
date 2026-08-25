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

/// The module epoch that is established while the framework is opening.
///
/// It is deliberately not cloneable.  The opening transaction is the only
/// owner until publication consumes it into a `ModuleEpochLease`.
pub(crate) struct ModuleOpening {
    module: &'static ModuleRuntime,
    epoch: u64,
}

/// Linear proof that the published runtime belongs to one module admission
/// epoch.  The lease is consumed by terminal certification.
pub(crate) struct ModuleEpochLease {
    module: &'static ModuleRuntime,
    epoch: u64,
}

/// Affine module capability after the module close has been linearized.
///
/// The capability owns the right to drain the module ingress.  Callers must
/// advance it to [`ModuleExportsDrained`] before they can certify module
/// quiescence.
pub(crate) struct ModuleClosing {
    module: &'static ModuleRuntime,
    epoch: u64,
}

/// Affine module capability after all exports have drained.
pub(crate) struct ModuleExportsDrained {
    module: &'static ModuleRuntime,
    epoch: u64,
    exports: ExportsDrained,
}

/// Terminal module capability.  Constructing this value is the only normal
/// path that certifies module-wide logical quiescence.
pub(crate) struct ModuleQuiescent {
    epoch: u64,
}

impl std::fmt::Debug for ModuleEpochLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModuleEpochLease")
            .field("epoch", &self.epoch)
            .finish()
    }
}

impl std::fmt::Debug for ModuleQuiescent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModuleQuiescent")
            .field("epoch", &self.epoch)
            .finish()
    }
}

impl ModuleOpening {
    pub(crate) fn commit(self) -> ModuleEpochLease {
        ModuleEpochLease {
            module: self.module,
            epoch: self.epoch,
        }
    }
}

impl ModuleEpochLease {
    pub(crate) fn is_current(&self) -> bool {
        self.module.ingress().epoch() == self.epoch
    }

    pub(crate) fn begin_close<F>(&self, on_closed: F) -> ModuleClosing
    where
        F: FnOnce(),
    {
        self.module.begin_close_internal(on_closed)
    }
}

impl ModuleClosing {
    pub(crate) fn seal_and_drain(self) -> ModuleExportsDrained {
        let exports = self.module.seal_and_drain_internal();
        ModuleExportsDrained {
            module: self.module,
            epoch: self.epoch,
            exports,
        }
    }
}

impl ModuleExportsDrained {
    /// Closes callback admission after the final host callback has completed.
    /// The operation is intentionally available only through this affine
    /// module capability, rather than through `global()`.
    pub(crate) fn close_callbacks(&self) {
        self.module.close_callbacks_internal();
    }

    pub(crate) fn certify(self) -> (ModuleQuiescent, ExportsDrained) {
        self.close_callbacks();
        self.module.certify_logical_quiescence_internal();
        (ModuleQuiescent { epoch: self.epoch }, self.exports)
    }
}

impl ModuleQuiescent {
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let module = global();
        Self {
            epoch: module.ingress().epoch(),
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
            epoch: self.ingress.epoch(),
        }
    }

    /// Begins module close after the caller has claimed the runtime removal
    /// owner.  RTD logical state changes only after ingress close has been
    /// linearized, matching the module quiescence proof.
    fn begin_close_internal<F>(&'static self, on_closed: F) -> ModuleClosing
    where
        F: FnOnce(),
    {
        self.ingress.begin_close_with(on_closed);
        #[cfg(any(feature = "rtd", test))]
        self.rtd.begin_close();
        ModuleClosing {
            module: self,
            epoch: self.ingress.epoch(),
        }
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

/// Starts module close when no committed module epoch is present.  This is
/// used only for uncommitted rollback/quarantine paths; committed removal
/// obtains its capability from `ModuleEpochLease::begin_close`.
pub(crate) fn begin_close_without_epoch<F>(on_closed: F) -> ModuleClosing
where
    F: FnOnce(),
{
    MODULE_RUNTIME.begin_close_internal(on_closed)
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
    let closing = begin_close_without_epoch(|| {});
    let drained = closing.seal_and_drain();
    let _ = drained.certify();
}

/// Read/admission capability for the module ingress owned by the singleton
/// module runtime.
pub(crate) fn ingress() -> &'static ExportIngress {
    global().ingress()
}
