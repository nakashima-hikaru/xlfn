#![cfg_attr(
    not(feature = "handles"),
    allow(
        dead_code,
        unreachable_pub,
        reason = "Optional RTD and handle configuration is hidden in core-only builds"
    )
)]

use crate::call::CallScope;
#[cfg(feature = "async")]
use crate::cancellation::{CancellationGuarantee, CancellationToken};
use crate::diagnostics::{AddinId, DiagnosticInitError, DiagnosticSink};
use crate::error::IntoXllError;
use crate::generation::RuntimeGeneration;
use crate::host_api::ExcelHost;
use crate::reference::ExcelReference;
#[cfg(any(feature = "rtd", test))]
use crate::rtd::RtdCallContext;
use crate::shutdown::CleanupReporter;
use crate::subscription::RtdLimits;
#[cfg(any(feature = "rtd", test))]
use crate::subscription::{RtdSource, RtdSourceHandle};
use crate::value::{ExcelValue, FromExcel, Matrix};
use crate::{XllError, XllResult};
use std::marker::PhantomData;
use std::num::NonZeroU32;
#[cfg(feature = "async")]
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct BuildInfo {
    addin_id: AddinId,
    version: &'static str,
    target: &'static str,
}

impl BuildInfo {
    pub(crate) const fn new(
        addin_id: AddinId,
        version: &'static str,
        target: &'static str,
    ) -> Self {
        Self {
            addin_id,
            version,
            target,
        }
    }

    #[must_use]
    pub const fn addin_id(&self) -> &AddinId {
        &self.addin_id
    }

    #[must_use]
    pub const fn version(&self) -> &str {
        self.version
    }

    #[must_use]
    pub const fn target(&self) -> &str {
        self.target
    }
}

#[derive(Debug)]
pub struct OpenContext {
    module_path: PathBuf,
    module_directory: PathBuf,
    build_info: BuildInfo,
    #[cfg(any(feature = "rtd", test))]
    source_allocator: crate::subscription::SourceHandleAllocator,
}

impl OpenContext {
    pub(crate) fn new(
        module_path: PathBuf,
        build_info: BuildInfo,
        generation: RuntimeGeneration,
    ) -> Self {
        let module_directory = module_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        #[cfg(not(any(feature = "rtd", test)))]
        let _ = generation;
        Self {
            module_path,
            module_directory,
            build_info,
            #[cfg(any(feature = "rtd", test))]
            source_allocator: crate::subscription::SourceHandleAllocator::new(generation),
        }
    }

    #[must_use]
    pub fn module_path(&self) -> &Path {
        &self.module_path
    }

    #[must_use]
    pub fn module_directory(&self) -> &Path {
        &self.module_directory
    }

    #[must_use]
    pub const fn build_info(&self) -> &BuildInfo {
        &self.build_info
    }

    #[must_use]
    pub fn diagnostics(&self) -> DiagnosticsSetup<'_> {
        DiagnosticsSetup { context: self }
    }

    /// Provides the RTD source-registration capability for this open.
    #[cfg(any(feature = "rtd", test))]
    #[must_use]
    pub fn rtd(&self) -> RtdOpenContext<'_> {
        RtdOpenContext {
            allocator: &self.source_allocator,
        }
    }
}

/// Capability for registering opaque RTD source identities during open.
#[derive(Clone, Copy, Debug)]
pub struct RtdOpenContext<'a> {
    allocator: &'a crate::subscription::SourceHandleAllocator,
}

#[cfg(any(feature = "rtd", test))]
impl RtdOpenContext<'_> {
    /// Registers one source and returns the handle used by subscriptions.
    pub fn register_source<S>(&self, source: S) -> XllResult<RtdSourceHandle<S>>
    where
        S: RtdSource,
    {
        self.allocator.allocate(source)
    }

    /// Registers one new source identity backed by shared source storage.
    ///
    /// The returned handle, rather than the `Arc`, is used by subscriptions.
    pub fn register_shared_source<S>(
        &self,
        source: std::sync::Arc<S>,
    ) -> XllResult<RtdSourceHandle<S>>
    where
        S: RtdSource,
    {
        self.allocator.allocate_shared(source)
    }
}

/// Diagnostic sink configuration capability available during add-in open.
#[derive(Clone, Copy, Debug)]
pub struct DiagnosticsSetup<'a> {
    context: &'a OpenContext,
}

impl DiagnosticsSetup<'_> {
    /// Installs a basic failure log at `%LOCALAPPDATA%/<addin-id>/logs/diagnostics.log`.
    pub fn install_file_sink(&self) -> Result<PathBuf, DiagnosticInitError> {
        crate::diagnostics::install_file_diagnostic_sink(&self.context.build_info.addin_id)
    }

    /// Installs or replaces the process-wide diagnostic sink with a custom implementation.
    pub fn set_sink<S>(&self, sink: S) -> Result<(), DiagnosticInitError>
    where
        S: DiagnosticSink,
    {
        crate::diagnostics::set_diagnostic_sink(sink)
    }
}

/// Handle-registry policy selected during one add-in open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandleBindingLimit(NonZeroU32);

impl HandleBindingLimit {
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        if value == 0 || value > HandleConfig::MAX_SUPPORTED_BINDINGS {
            None
        } else {
            Some(Self(
                NonZeroU32::new(value).expect("binding limit is non-zero"),
            ))
        }
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl TryFrom<u32> for HandleBindingLimit {
    type Error = crate::XllError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(crate::XllError::Domain {
            code: crate::error::DomainErrorCode::InvalidInput,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandleConfig {
    maximum_bindings: HandleBindingLimit,
}

impl HandleConfig {
    pub const DEFAULT_MAX_BINDINGS: u32 = 16_384;
    /// Upper bound for the dense immutable publication table.
    ///
    /// The table is allocated when a handle generation is initialized, so an
    /// unchecked `u32` would turn configuration input into an unbounded eager
    /// allocation. The bound keeps the dense lookup policy explicit.
    pub const MAX_SUPPORTED_BINDINGS: u32 = 1_048_576;

    #[must_use]
    pub const fn new() -> Self {
        Self {
            maximum_bindings: HandleBindingLimit::new(Self::DEFAULT_MAX_BINDINGS)
                .expect("default handle limit is supported"),
        }
    }

    #[must_use]
    pub const fn with_binding_limit(mut self, maximum_bindings: HandleBindingLimit) -> Self {
        self.maximum_bindings = maximum_bindings;
        self
    }

    pub(crate) const fn maximum_bindings(self) -> u32 {
        self.maximum_bindings.get()
    }
}

impl Default for HandleConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// RTD, handle, and asynchronous runtime policy selected during one add-in open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    rtd: RtdConfig,
    handles: HandleConfig,
    #[cfg(feature = "async")]
    async_runtime: AsyncRuntimeConfig,
}

impl RuntimeConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rtd: RtdConfig::new(),
            handles: HandleConfig::new(),
            #[cfg(feature = "async")]
            async_runtime: AsyncRuntimeConfig::new(),
        }
    }

    #[cfg(any(feature = "rtd", test))]
    #[must_use]
    pub const fn with_rtd_limits(mut self, limits: RtdLimits) -> Self {
        self.rtd = self.rtd.with_limits(limits);
        self
    }

    #[cfg(any(feature = "handles", test))]
    #[must_use]
    pub const fn with_handle_config(mut self, handles: HandleConfig) -> Self {
        self.handles = handles;
        self
    }

    #[cfg(feature = "async")]
    #[must_use]
    pub const fn with_async_worker_count(mut self, worker_count: AsyncWorkerCount) -> Self {
        self.async_runtime = self.async_runtime.with_worker_count(worker_count);
        self
    }

    pub(crate) const fn rtd_limits(self) -> RtdLimits {
        self.rtd.limits()
    }

    pub(crate) const fn handle_config(self) -> HandleConfig {
        self.handles
    }

    #[cfg(feature = "async")]
    pub(crate) const fn async_worker_count(self) -> usize {
        self.async_runtime.worker_count()
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// RTD-specific portion of [`RuntimeConfig`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtdConfig {
    limits: RtdLimits,
}

impl RtdConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            limits: RtdLimits::standard(),
        }
    }

    #[must_use]
    pub const fn with_limits(mut self, limits: RtdLimits) -> Self {
        self.limits = limits;
        self
    }

    pub(crate) const fn limits(self) -> RtdLimits {
        self.limits
    }
}

impl Default for RtdConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Bounded number of asynchronous executor workers.
#[cfg(feature = "async")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AsyncWorkerCount(NonZeroUsize);

#[cfg(feature = "async")]
impl AsyncWorkerCount {
    pub const MAX: usize = 32;
    pub const DEFAULT: Self = Self(NonZeroUsize::new(4).expect("default worker count is non-zero"));

    #[must_use]
    pub const fn new(worker_count: usize) -> Option<Self> {
        if worker_count == 0 || worker_count > Self::MAX {
            None
        } else {
            Some(Self(
                NonZeroUsize::new(worker_count).expect("worker count is non-zero"),
            ))
        }
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

#[cfg(feature = "async")]
impl TryFrom<usize> for AsyncWorkerCount {
    type Error = crate::XllError;

    fn try_from(worker_count: usize) -> Result<Self, Self::Error> {
        Self::new(worker_count).ok_or(crate::XllError::Domain {
            code: crate::error::DomainErrorCode::InvalidInput,
        })
    }
}

/// Async worker portion of [`RuntimeConfig`].
#[cfg(feature = "async")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AsyncRuntimeConfig {
    worker_count: AsyncWorkerCount,
}

#[cfg(feature = "async")]
impl AsyncRuntimeConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            worker_count: AsyncWorkerCount::DEFAULT,
        }
    }

    #[must_use]
    pub const fn with_worker_count(mut self, worker_count: AsyncWorkerCount) -> Self {
        self.worker_count = worker_count;
        self
    }

    pub(crate) const fn worker_count(self) -> usize {
        self.worker_count.get()
    }
}

#[cfg(feature = "async")]
impl Default for AsyncRuntimeConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// The result of a successful [`Addin::open`] transaction.
pub struct Opened<S, L, U> {
    shared_state: S,
    lifecycle_state: L,
    layers: U,
    runtime: RuntimeConfig,
}

impl<S, L, U> Opened<S, L, U> {
    #[must_use]
    pub const fn new(shared_state: S, lifecycle_state: L, layers: U) -> Self {
        Self {
            shared_state,
            lifecycle_state,
            layers,
            runtime: RuntimeConfig::new(),
        }
    }

    #[must_use]
    pub const fn with_runtime_config(mut self, runtime: RuntimeConfig) -> Self {
        self.runtime = runtime;
        self
    }

    pub(crate) fn into_parts(self) -> (S, L, U, RuntimeConfig) {
        (
            self.shared_state,
            self.lifecycle_state,
            self.layers,
            self.runtime,
        )
    }
}

/// Defines Add-in state and its Excel lifecycle hooks.
///
/// The framework invokes [`Self::open`] and [`Self::cleanup`] from Excel's main
/// lifecycle thread, and all lifecycle hooks for one open generation run on
/// that same thread. xlfn binds lifecycle-state access to that thread for the
/// generation and quarantines the runtime if a lifecycle boundary is entered
/// from another thread. `SharedState` is the state borrowed by UDF calls and
/// is therefore required to be `Send + Sync`. `LifecycleState` is owned by the
/// framework's thread-affine lifecycle slot and may be thread-affine or
/// otherwise non-`Send`.
///
/// [`Self::quiesce`] must synchronously stop every execution source before
/// best-effort lifecycle cleanup begins. The shared state is dropped only
/// after quiescence; lifecycle cleanup receives the dedicated lifecycle state.
pub trait Addin: Send + Sync + 'static {
    type SharedState: Send + Sync + 'static;
    type LifecycleState: 'static;
    type Error: IntoXllError;
    type Layers: crate::execution::UdfLayers;

    /// Opens one complete generation on Excel's main lifecycle thread.
    ///
    /// Shared state, lifecycle state, UDF layers, and runtime policy are
    /// returned together so the framework can stage them as one transaction.
    /// If a later registration step fails, none of the execution state is
    /// published as an open generation.
    #[allow(
        clippy::type_complexity,
        reason = "the associated state types are the public Addin open contract"
    )]
    fn open(
        context: &OpenContext,
    ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error>;

    /// Stops every Add-in-owned callback, worker, native module owner, and
    /// other source that could execute XLL code after unload.
    ///
    /// Returning `Ok(())` certifies that every such execution resource is
    /// quiescent. A panic or `Err` leaves teardown incomplete, so the runtime
    /// enters `Quarantined`, retains the module residency lease, and rejects
    /// further opens or UDF calls. The hook is terminal for that generation
    /// and is never retried.
    ///
    /// Handle values are call-scoped and cannot be stored in either state.
    /// Vendor operations must be canceled cooperatively; unload waits rather
    /// than abandoning in-process code.
    fn quiesce(
        _shared: &mut Self::SharedState,
        _lifecycle: &mut Self::LifecycleState,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Performs best-effort disposal after quiescence has been established.
    ///
    /// This hook must not start work or register callbacks. Disposal failures
    /// should be recorded with `reporter`; they do not make unload unsafe.
    /// `SharedState` is already quiesced and is dropped by the framework
    /// separately after this hook returns.
    fn cleanup(_lifecycle: &mut Self::LifecycleState, _reporter: &mut CleanupReporter<'_>) {}
}

impl Addin for () {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(
        _context: &OpenContext,
    ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new((), (), ()))
    }
}

impl<A: Addin> AsRef<A::SharedState> for ThreadSafeContext<'_, A> {
    fn as_ref(&self) -> &A::SharedState {
        self.state
    }
}

pub struct ThreadSafeContext<'call, A: Addin> {
    state: &'call A::SharedState,
}

impl<A: Addin> Clone for ThreadSafeContext<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A: Addin> Copy for ThreadSafeContext<'_, A> {}

/// Call-scoped Add-in state available to an asynchronous worksheet function.
///
/// The framework-owned async Future retains the open-generation lease and the
/// cancellation token. This context borrows both for the duration of that
/// Future, so a context cannot be moved into a detached task or thread that
/// outlives the invocation. Long-lived state must be owned explicitly by the
/// add-in rather than by escaping this capability.
#[cfg(feature = "async")]
pub struct AsyncContext<'call, A: Addin> {
    state: &'call A::SharedState,
    cancellation: &'call CancellationToken,
}

#[cfg(feature = "async")]
impl<'call, A: Addin> AsyncContext<'call, A> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new(state: &'call A::SharedState, cancellation: &'call CancellationToken) -> Self {
        Self {
            state,
            cancellation,
        }
    }

    #[must_use]
    pub const fn state(&self) -> &'call A::SharedState {
        self.state
    }

    #[must_use]
    pub const fn cancellation(&self) -> &'call CancellationToken {
        self.cancellation
    }

    pub fn check_cancelled(&self) -> XllResult<()> {
        if self.cancellation.is_cancelled() {
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub const fn cancellation_guarantee(&self) -> CancellationGuarantee {
        self.cancellation.guarantee()
    }
}

#[cfg(feature = "async")]
impl<A: Addin> AsRef<A::SharedState> for AsyncContext<'_, A> {
    fn as_ref(&self) -> &A::SharedState {
        self.state
    }
}

impl<A: Addin> AsRef<A::SharedState> for MainThreadContext<'_, A> {
    fn as_ref(&self) -> &A::SharedState {
        self.state
    }
}

impl<'call, A: Addin> ThreadSafeContext<'call, A> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new(state: &'call A::SharedState) -> Self {
        Self { state }
    }

    #[must_use]
    pub const fn state(&self) -> &'call A::SharedState {
        self.state
    }
}

/// Call-scoped state and host callbacks for a main-thread UDF.
///
/// The synchronous call guard already pins the published generation for the
/// duration of this lifetime. Keeping the existing state borrow here avoids a
/// second generation lease and makes the ownership relationship explicit.
pub struct MainThreadContext<'call, A: Addin> {
    state: &'call A::SharedState,
    #[cfg(any(feature = "rtd", test))]
    rtd: RtdCallContext<'call>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl<A: Addin> Clone for MainThreadContext<'_, A> {
    fn clone(&self) -> Self {
        Self {
            state: self.state,
            #[cfg(any(feature = "rtd", test))]
            rtd: self.rtd,
            _not_send_or_sync: PhantomData,
        }
    }
}

/// Call-scoped state and host callbacks for a macro-sheet UDF.
///
/// This context is borrowed entirely from the active call. Unlike
/// [`AsyncContext`], it cannot outlive the callback scope and therefore does
/// not need an owned generation lease.
pub struct MacroSheetContext<'call, A: Addin> {
    state: &'call A::SharedState,
    host: ExcelHost<'call>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl<A: Addin> Clone for MacroSheetContext<'_, A> {
    fn clone(&self) -> Self {
        Self {
            state: self.state,
            host: self.host,
            _not_send_or_sync: PhantomData,
        }
    }
}

impl<A: Addin> AsRef<A::SharedState> for MacroSheetContext<'_, A> {
    fn as_ref(&self) -> &A::SharedState {
        self.state
    }
}

impl<A: Addin> MacroSheetContext<'_, A> {
    #[doc(hidden)]
    #[must_use]
    pub fn new<'ctx>(
        state: &'ctx A::SharedState,
        scope: &'ctx CallScope<'ctx>,
    ) -> MacroSheetContext<'ctx, A> {
        MacroSheetContext {
            state,
            host: ExcelHost::new(scope.callbacks()),
            _not_send_or_sync: PhantomData,
        }
    }
}

impl<'call, A: Addin> MacroSheetContext<'call, A> {
    #[must_use]
    pub fn state(&self) -> &A::SharedState {
        self.state
    }

    pub fn coerce(&self, reference: &ExcelReference<'_>) -> XllResult<ExcelValue> {
        self.host.coerce(reference)
    }

    pub fn coerce_matrix<T>(&self, reference: &ExcelReference<'_>) -> XllResult<Matrix<T>>
    where
        T: for<'value> FromExcel<'value>,
    {
        self.host.coerce_matrix(reference)
    }

    pub fn sheet_name(&self, reference: &ExcelReference<'_>) -> XllResult<String> {
        self.host.sheet_name(reference)
    }
}

impl<A: Addin> MainThreadContext<'_, A> {
    #[doc(hidden)]
    #[must_use]
    #[cfg(any(feature = "rtd", test))]
    pub(crate) fn new<'ctx>(
        state: &'ctx A::SharedState,
        rtd: RtdCallContext<'ctx>,
    ) -> MainThreadContext<'ctx, A> {
        MainThreadContext {
            state,
            #[cfg(any(feature = "rtd", test))]
            rtd,
            _not_send_or_sync: PhantomData,
        }
    }

    #[doc(hidden)]
    #[must_use]
    #[cfg(all(not(feature = "rtd"), not(test)))]
    pub(crate) fn new<'ctx>(state: &'ctx A::SharedState) -> MainThreadContext<'ctx, A> {
        MainThreadContext {
            state,
            _not_send_or_sync: PhantomData,
        }
    }
}

impl<'call, A: Addin> MainThreadContext<'call, A> {
    #[must_use]
    pub fn state(&self) -> &A::SharedState {
        self.state
    }

    #[cfg(any(feature = "rtd", test))]
    #[must_use]
    pub fn rtd(&self) -> RtdCallContext<'call> {
        self.rtd
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "async")]
    use super::AsyncContext;
    use super::{
        DiagnosticsSetup, MacroSheetContext, MainThreadContext, Opened, ThreadSafeContext,
    };
    use static_assertions::{assert_impl_all, assert_not_impl_any};
    use std::rc::Rc;
    #[cfg(any(feature = "async", not(target_os = "windows")))]
    use std::sync::Arc;

    assert_impl_all!(ThreadSafeContext<'static, ()>: Copy, Clone, Send, Sync);
    assert_impl_all!(MainThreadContext<'static, ()>: Clone);
    assert_impl_all!(MacroSheetContext<'static, ()>: Clone);
    assert_impl_all!(DiagnosticsSetup<'static>: Copy, Clone, Send, Sync);
    assert_not_impl_any!(super::OpenContext: Clone);
    assert_not_impl_any!(MainThreadContext<'static, ()>: Send, Sync);
    assert_not_impl_any!(MacroSheetContext<'static, ()>: Send, Sync);

    struct NonSendLifecycleAddin;

    impl crate::Addin for NonSendLifecycleAddin {
        type SharedState = ();
        type LifecycleState = Rc<()>;
        type Error = crate::XllError;
        type Layers = ();

        fn open(
            _: &crate::OpenContext,
        ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error>
        {
            Ok(Opened::new((), Rc::new(()), ()))
        }
    }

    assert_impl_all!(crate::runtime::Runtime<NonSendLifecycleAddin>: Send, Sync);

    struct TestU32Addin;

    impl crate::Addin for TestU32Addin {
        type SharedState = u32;
        type LifecycleState = ();
        type Error = crate::XllError;
        type Layers = ();

        fn open(
            _: &crate::OpenContext,
        ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error>
        {
            unreachable!()
        }
    }

    #[test]
    fn synchronous_contexts_expose_their_state_by_value() {
        let state = 17_u32;
        let runtime = crate::runtime::Runtime::<TestU32Addin>::new();
        let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish(state, ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        let services = runtime.generation_services().unwrap();
        let thread_safe = ThreadSafeContext::<TestU32Addin>::new(&state);
        crate::call::with_excel_call_scope_and_state(&state, |state, scope| {
            let main_thread = MainThreadContext::<TestU32Addin>::new(
                state,
                crate::rtd::RtdCallContext::new(
                    services.as_ref(),
                    crate::host_api::ExcelHost::new(scope.callbacks()),
                ),
            );
            let macro_sheet = MacroSheetContext::<TestU32Addin>::new(state, scope);

            assert_eq!(thread_safe.state(), &17);
            assert_eq!(main_thread.state(), &17);
            assert_eq!(macro_sheet.state(), &17);

            let copied = main_thread.clone();
            assert_eq!(copied.state(), &17);
            assert_eq!(main_thread.state(), &17);
        });
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn one_call_scope_suppresses_later_macro_sheet_callbacks_after_abort() {
        use xlfn_sys::{
            XL_SHEET_NM, XLOPER12, XLOPER12SRef, XLOPER12Value, XLREF12, XLRET_ABORT, XLTYPE_SREF,
        };

        let _callback_guard = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
        crate::test_callback::set_terminal(XL_SHEET_NM, XLRET_ABORT);

        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                sref: XLOPER12SRef {
                    count: 1,
                    reference: XLREF12 {
                        rw_first: 0,
                        rw_last: 0,
                        col_first: 0,
                        col_last: 0,
                    },
                },
            },
            xltype: XLTYPE_SREF,
        };
        // SAFETY: `raw` remains live for the reference and callback scope.
        let reference: crate::reference::ExcelReference<'_> =
            unsafe { crate::reference::reference_from_raw("reference", &mut raw) }.unwrap();
        let state = ();
        crate::call::with_excel_call_scope_and_state(&state, |state, scope| {
            let runtime = crate::runtime::Runtime::<()>::new();
            let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
            runtime.publish((), ());
            runtime.finish_open(&mut opening, Vec::new()).unwrap();
            let context = MacroSheetContext::<()>::new(state, scope);
            assert!(context.sheet_name(&reference).is_err());
            let _ = context.coerce(&reference);
            assert_eq!(crate::test_callback::total_calls(), 2);
            assert_eq!(crate::test_callback::free_calls(), 1);
            assert_eq!(
                crate::test_callback::total_calls() - crate::test_callback::free_calls(),
                1
            );
        });
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn failed_rtd_observation_preserves_the_existing_shared_subscription() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct TestSubscription {
            disconnected: Arc<AtomicBool>,
        }

        impl crate::subscription::RtdSubscription for TestSubscription {
            fn cancellation(&self) -> std::sync::Arc<dyn crate::subscription::RtdCancellation> {
                std::sync::Arc::new(crate::subscription::RtdCancellationHandle::noop())
            }
            fn disconnect_and_wait(self: Box<Self>) -> crate::XllResult<()> {
                self.disconnected.store(true, Ordering::Release);
                Ok(())
            }
        }

        struct TestSource {
            disconnected: Arc<AtomicBool>,
        }

        impl crate::subscription::RtdSource for TestSource {
            type Value = f64;
            type Subscription = TestSubscription;

            fn subscribe(
                &self,
                _topic: &crate::subscription::RtdTopic,
                sink: crate::subscription::RtdSink<Self::Value>,
            ) -> crate::XllResult<Self::Subscription> {
                sink.publish(17.5)?;
                Ok(TestSubscription {
                    disconnected: Arc::clone(&self.disconnected),
                })
            }
        }

        let runtime = crate::runtime::Runtime::<()>::new();
        let mut opening = runtime.lifecycle_runtime().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        let subscriptions = runtime.subscriptions().unwrap();
        let subscriptions = subscriptions.as_arc();
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = crate::subscription::RtdSourceHandle::for_internal(
            runtime
                .last_committed_generation()
                .expect("test runtime has a generation"),
            TestSource {
                disconnected: Arc::clone(&disconnected),
            },
        )
        .unwrap();
        let topic = crate::subscription::RtdTopic::single("shared-observation").unwrap();
        let prepared = subscriptions.prepare(&source, topic.clone()).unwrap();
        let server = subscriptions
            .register_server(
                crate::subscription::ServerGeneration::new(51)
                    .expect("non-zero test server generation"),
            )
            .unwrap();
        let key_obj = *prepared.key();
        let conn = subscriptions
            .connect_transaction(&server, crate::subscription::TopicId(7), &key_obj)
            .unwrap();
        assert_eq!(
            conn.value(),
            &crate::subscription::StoredRtdValue::Number(17.5)
        );
        conn.commit().unwrap();

        let services = runtime.generation_services().unwrap();
        let repeated = subscriptions.prepare(&source, topic.clone()).unwrap();
        assert_eq!(repeated.key(), &key_obj);
        assert!(!repeated.has_reservation());
        repeated.rollback();

        let _state = ();
        crate::value::with_excel_call_scope(|scope| {
            let context = MainThreadContext::<()>::new(
                &_state,
                crate::rtd::RtdCallContext::new(
                    services.as_ref(),
                    crate::host_api::ExcelHost::new(scope.callbacks()),
                ),
            );
            assert!(matches!(
                context.rtd().subscribe(&source, topic),
                Err(crate::XllError::ExcelApi {
                    function: crate::ExcelApiFunction::Rtd,
                    failure: crate::ExcelApiFailure::Status(crate::ExcelCallbackStatus::Failed(
                        xlfn_sys::XLRET_FAILED,
                    )),
                })
            ));
        });

        let batch = server.begin_refresh().unwrap();
        assert!(batch.updates.is_empty());
        batch
            .complete(crate::subscription::RefreshOutcome::Delivered)
            .unwrap();

        assert!(!disconnected.load(Ordering::Acquire));

        let _ = server.disconnect(crate::subscription::TopicId(7));
        assert!(disconnected.load(Ordering::Acquire));
    }

    #[cfg(feature = "async")]
    struct AsyncTestAddin;

    #[cfg(feature = "async")]
    impl crate::Addin for AsyncTestAddin {
        type SharedState = u32;
        type LifecycleState = ();
        type Error = crate::XllError;
        type Layers = ();

        fn open(
            _context: &crate::OpenContext,
        ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error>
        {
            Ok(Opened::new(23, (), ()))
        }
    }

    #[cfg(feature = "async")]
    #[test]
    fn async_context_checks_and_exposes_cancellation() {
        let (source, token) = crate::cancellation::CancellationSource::new(
            crate::cancellation::CancellationGuarantee::CalculationScoped,
        );
        let generation = Arc::new(crate::runtime::ExecutionGeneration::<AsyncTestAddin> {
            id: crate::generation::RuntimeGeneration::new(1).unwrap(),
            shared_state: 23_u32,
            layers: (),
        });
        let context: AsyncContext<'_, AsyncTestAddin> =
            AsyncContext::new(&generation.shared_state, &token);

        assert_eq!(context.state(), &23);
        assert!(!context.cancellation().is_cancelled());
        assert!(context.check_cancelled().is_ok());

        source.cancel();
        assert!(context.cancellation().is_cancelled());
        assert!(matches!(
            context.check_cancelled(),
            Err(crate::XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
    }

    #[test]
    fn open_context_exposes_diagnostics_setup() {
        let build_info = crate::addin::BuildInfo::new(
            crate::diagnostics::AddinId::parse("test-addin").unwrap(),
            "0.1.0",
            "x86_64-pc-windows-msvc",
        );
        let context = crate::OpenContext::new(
            std::path::PathBuf::from("/test/module.xll"),
            build_info,
            super::RuntimeGeneration::new(1).expect("test generation is non-zero"),
        );
        let diag = context.diagnostics();
        let copied = diag;
        assert_eq!(
            diag.context.build_info().version(),
            copied.context.build_info().version()
        );
    }
}
