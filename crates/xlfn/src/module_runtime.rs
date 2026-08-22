use crate::callback_gate::{CallbackGate, CallbackGateLifecycle};
use crate::ingress::{ExportIngress, ExportsDrained};
use crate::rtd::RtdModuleState;
use std::sync::LazyLock;

#[cfg(target_os = "windows")]
use crate::rtd::windows::ComModuleLifetime;

/// The module-wide ownership root for protocols that must move together
/// across an open/close epoch.
///
/// The individual components retain their own synchronization and state
/// machines.  This root owns their lifetime and provides the small set of
/// cross-component transitions used by the runtime, so a new protocol cannot
/// accidentally introduce a second module epoch owner.
pub(crate) struct ModuleRuntime {
    ingress: ExportIngress,
    callback_gate: CallbackGate,
    rtd: RtdModuleState,
    #[cfg(target_os = "windows")]
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

impl std::fmt::Debug for ModuleEpochLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModuleEpochLease")
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
}

impl ModuleRuntime {
    fn new() -> Self {
        Self {
            ingress: ExportIngress::new(),
            callback_gate: CallbackGate::new(CallbackGateLifecycle::Closed),
            rtd: RtdModuleState::new(),
            #[cfg(target_os = "windows")]
            com: ComModuleLifetime::new(),
        }
    }

    pub(crate) fn ingress(&'static self) -> &'static ExportIngress {
        &self.ingress
    }

    pub(crate) fn callback_gate(&'static self) -> &'static CallbackGate {
        &self.callback_gate
    }

    pub(crate) fn reset_callbacks(&'static self) {
        self.callback_gate.reset();
    }

    pub(crate) fn rtd(&'static self) -> &'static RtdModuleState {
        &self.rtd
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn com_module_lifetime(&'static self) -> &'static ComModuleLifetime {
        &self.com
    }

    /// Starts the module components for one opening epoch in their canonical
    /// order.  Runtime admission is reopened by `Runtime` immediately before
    /// this transition; this method owns the module-local portion.
    pub(crate) fn begin_open(&'static self) -> ModuleOpening {
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
    pub(crate) fn begin_close<F>(&'static self, on_closed: F)
    where
        F: FnOnce(),
    {
        self.ingress.begin_close_with(on_closed);
        self.rtd.begin_close();
    }

    pub(crate) fn close_callbacks(&'static self) {
        self.callback_gate.close();
    }

    pub(crate) fn seal_and_drain(&'static self) -> ExportsDrained {
        self.ingress.seal_and_drain()
    }

    pub(crate) fn certify_logical_quiescence(&'static self) {
        self.rtd.certify_logical_quiescence();
    }
}

static MODULE_RUNTIME: LazyLock<ModuleRuntime> = LazyLock::new(ModuleRuntime::new);

pub(crate) fn global() -> &'static ModuleRuntime {
    &MODULE_RUNTIME
}

/// Read/admission capability for the module ingress owned by the singleton
/// module runtime.
pub(crate) fn ingress() -> &'static ExportIngress {
    global().ingress()
}
