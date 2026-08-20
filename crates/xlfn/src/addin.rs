use crate::host_callback::HostCallbackSession;
use crate::{
    AddinId, CallScope, CleanupReporter, ExcelCallbackStatus, ExcelReference, ExcelValue,
    FromExcel, IntoXllError, Matrix, XllError, XllResult,
};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use xlfn_sys::{XL_COERCE, XL_SHEET_NM};

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

#[derive(Clone, Debug)]
pub struct OpenContext {
    module_path: PathBuf,
    module_directory: PathBuf,
    build_info: BuildInfo,
}

impl OpenContext {
    pub(crate) fn new(module_path: PathBuf, build_info: BuildInfo) -> Self {
        let module_directory = module_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Self {
            module_path,
            module_directory,
            build_info,
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
    #[must_use]
    pub fn rtd(&self) -> RtdOpenContext<'_> {
        RtdOpenContext { context: self }
    }
}

/// Capability for registering opaque RTD source identities during open.
#[derive(Clone, Copy, Debug)]
pub struct RtdOpenContext<'a> {
    context: &'a OpenContext,
}

impl RtdOpenContext<'_> {
    /// Registers one source and returns the handle used by subscriptions.
    pub fn register_source<S>(&self, source: S) -> XllResult<crate::RtdSourceHandle<S>>
    where
        S: crate::RtdSource,
    {
        let _ = self.context;
        crate::RtdSourceHandle::new(source)
    }

    /// Registers one shared source and returns the handle used by subscriptions.
    pub fn register_shared_source<S>(
        &self,
        source: std::sync::Arc<S>,
    ) -> XllResult<crate::RtdSourceHandle<S>>
    where
        S: crate::RtdSource,
    {
        let _ = self.context;
        crate::RtdSourceHandle::from_arc(source)
    }
}

/// Diagnostic sink configuration capability available during add-in open.
#[derive(Clone, Copy, Debug)]
pub struct DiagnosticsSetup<'a> {
    context: &'a OpenContext,
}

impl DiagnosticsSetup<'_> {
    /// Installs a basic failure log at `%LOCALAPPDATA%/<addin-id>/logs/diagnostics.log`.
    pub fn install_file_sink(&self) -> Result<PathBuf, crate::DiagnosticInitError> {
        crate::diagnostics::install_file_diagnostic_sink(&self.context.build_info.addin_id)
    }

    /// Installs or replaces the process-wide diagnostic sink with a custom implementation.
    pub fn set_sink<S>(&self, sink: S) -> Result<(), crate::DiagnosticInitError>
    where
        S: crate::DiagnosticSink,
    {
        crate::diagnostics::set_diagnostic_sink(sink)
    }
}

/// RTD and asynchronous runtime policy selected during one add-in open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    rtd: RtdConfig,
    async_runtime: AsyncRuntimeConfig,
}

impl RuntimeConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rtd: RtdConfig::new(),
            async_runtime: AsyncRuntimeConfig::new(),
        }
    }

    #[must_use]
    pub const fn with_rtd_limits(mut self, limits: crate::RtdLimits) -> Self {
        self.rtd = self.rtd.with_limits(limits);
        self
    }

    #[must_use]
    pub const fn with_async_worker_count(mut self, worker_count: usize) -> Self {
        self.async_runtime = self.async_runtime.with_worker_count(worker_count);
        self
    }

    pub(crate) const fn rtd_limits(self) -> crate::RtdLimits {
        self.rtd.limits()
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
    limits: crate::RtdLimits,
}

impl RtdConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            limits: crate::RtdLimits::standard(),
        }
    }

    #[must_use]
    pub const fn with_limits(mut self, limits: crate::RtdLimits) -> Self {
        self.limits = limits;
        self
    }

    pub(crate) const fn limits(self) -> crate::RtdLimits {
        self.limits
    }
}

impl Default for RtdConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Async worker portion of [`RuntimeConfig`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AsyncRuntimeConfig {
    worker_count: usize,
}

impl AsyncRuntimeConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self { worker_count: 4 }
    }

    #[must_use]
    pub const fn with_worker_count(mut self, worker_count: usize) -> Self {
        self.worker_count = if worker_count < 1 {
            1
        } else if worker_count > 32 {
            32
        } else {
            worker_count
        };
        self
    }

    #[cfg(feature = "async")]
    pub(crate) const fn worker_count(self) -> usize {
        self.worker_count
    }
}

impl Default for AsyncRuntimeConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// The result of a successful [`Addin::open`] transaction.
pub struct Opened<S, L> {
    state: S,
    layers: L,
    runtime: RuntimeConfig,
}

impl<S, L> Opened<S, L> {
    #[must_use]
    pub const fn new(state: S, layers: L) -> Self {
        Self {
            state,
            layers,
            runtime: RuntimeConfig::new(),
        }
    }

    #[must_use]
    pub const fn with_runtime_config(mut self, runtime: RuntimeConfig) -> Self {
        self.runtime = runtime;
        self
    }

    pub(crate) fn into_parts(self) -> (S, L, RuntimeConfig) {
        (self.state, self.layers, self.runtime)
    }
}

/// Defines Add-in state and its Excel lifecycle hooks.
///
/// The framework invokes [`Self::open`] and [`Self::cleanup`] from Excel's main
/// lifecycle thread, and both hooks for one open generation run on that same
/// thread. Implementations may therefore keep non-`Send` lifecycle owners in
/// thread-local storage while placing only their `Send + Sync` handles in
/// [`Self::State`]. [`Self::quiesce`] must synchronously stop every execution
/// source before best-effort cleanup begins.
pub trait Addin: Send + Sync + 'static {
    type State: Send + Sync + 'static;
    type Error: IntoXllError;
    type Layers: crate::execution::UdfLayers;

    /// Opens one complete generation on Excel's main lifecycle thread.
    ///
    /// State, UDF layers, and runtime policy are returned together so the
    /// framework can stage them as one transaction. If a later registration
    /// step fails, none of the three is published as an open generation.
    fn open(context: &OpenContext) -> Result<Opened<Self::State, Self::Layers>, Self::Error>;

    /// Stops every Add-in-owned callback, worker, native module owner, and
    /// other source that could execute XLL code after unload.
    ///
    /// Returning `Ok(())` certifies that every such execution resource is
    /// quiescent. A panic or `Err` leaves teardown incomplete, so the runtime
    /// enters `Quarantined`, retains the module residency lease, and rejects
    /// further opens or UDF calls. The hook is terminal for that generation
    /// and is never retried.
    ///
    /// Handle values are call-scoped and cannot be stored in `State`. Vendor
    /// operations must be canceled cooperatively; unload waits rather than
    /// abandoning in-process code.
    fn quiesce(_state: &mut Self::State) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Performs best-effort disposal after quiescence has been established.
    ///
    /// This hook must not start work or register callbacks. Disposal failures
    /// should be recorded with `reporter`; they do not make unload unsafe.
    fn cleanup(_state: &mut Self::State, _reporter: &mut CleanupReporter<'_>) {}
}

impl Addin for () {
    type State = ();
    type Error = XllError;
    type Layers = ();

    fn open(_context: &OpenContext) -> Result<Opened<Self::State, Self::Layers>, Self::Error> {
        Ok(Opened::new((), ()))
    }
}

/// Static metadata and lifecycle configuration supplied by `#[excel_addin]`.
#[doc(hidden)]
pub trait AddinMetadata {
    const ID: &'static str;
    const DISPLAY_NAME: &'static str;
    const DEFAULT_CATEGORY: &'static str;
}

impl<A: Addin> AsRef<A::State> for ThreadSafeContext<'_, A> {
    fn as_ref(&self) -> &A::State {
        self.state
    }
}

pub struct ThreadSafeContext<'call, A: Addin> {
    state: &'call A::State,
}

impl<A: Addin> Clone for ThreadSafeContext<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A: Addin> Copy for ThreadSafeContext<'_, A> {}

/// Owned Add-in state available to an asynchronous worksheet function.
///
/// The context holds an explicit open-generation lifetime lease. Moving it
/// into a detached thread or task that can outlive the returned future violates
/// the XLL shutdown contract. The runtime detects an escaped lease during
/// explicit removal, enters `Quarantined`, and keeps the DLL resident rather
/// than returning with executable XLL code still reachable.
#[cfg(feature = "async")]
pub struct AsyncContext<A: Addin> {
    lease: crate::runtime::GenerationLease<A>,
    cancellation: crate::CancellationToken,
}

#[cfg(feature = "async")]
impl<A: Addin> AsyncContext<A> {
    #[doc(hidden)]
    #[must_use]
    pub fn new(
        lease: crate::runtime::GenerationLease<A>,
        cancellation: crate::CancellationToken,
    ) -> Self {
        Self {
            lease,
            cancellation,
        }
    }

    #[must_use]
    pub fn state(&self) -> &A::State {
        self.lease.state()
    }

    #[must_use]
    pub const fn cancellation(&self) -> &crate::CancellationToken {
        &self.cancellation
    }

    pub fn check_cancelled(&self) -> XllResult<()> {
        if self.cancellation.is_cancelled() {
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub const fn cancellation_guarantee(&self) -> crate::CancellationGuarantee {
        self.cancellation.guarantee()
    }
}

#[cfg(feature = "async")]
impl<A: Addin> AsRef<A::State> for AsyncContext<A> {
    fn as_ref(&self) -> &A::State {
        self.lease.state()
    }
}

impl<A: Addin> AsRef<A::State> for MainThreadContext<'_, A> {
    fn as_ref(&self) -> &A::State {
        self.state.state()
    }
}

impl<'call, A: Addin> ThreadSafeContext<'call, A> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new(state: &'call A::State) -> Self {
        Self { state }
    }

    #[must_use]
    pub const fn state(&self) -> &'call A::State {
        self.state
    }
}

pub struct MainThreadContext<'call, A: Addin> {
    state: crate::runtime::GenerationLease<A>,
    runtime: &'call crate::Runtime<A>,
    callbacks: &'call HostCallbackSession,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl<A: Addin> Clone for MainThreadContext<'_, A> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            runtime: self.runtime,
            callbacks: self.callbacks,
            _not_send_or_sync: PhantomData,
        }
    }
}

pub struct MacroSheetContext<'call, A: Addin> {
    state: crate::runtime::GenerationLease<A>,
    callbacks: &'call HostCallbackSession,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl<A: Addin> Clone for MacroSheetContext<'_, A> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            callbacks: self.callbacks,
            _not_send_or_sync: PhantomData,
        }
    }
}

impl<A: Addin> AsRef<A::State> for MacroSheetContext<'_, A> {
    fn as_ref(&self) -> &A::State {
        self.state.state()
    }
}

impl<A: Addin> MacroSheetContext<'_, A> {
    #[doc(hidden)]
    #[must_use]
    pub fn new<'ctx, 'scope>(
        runtime: &'ctx crate::Runtime<A>,
        scope: &'ctx CallScope<'scope>,
    ) -> MacroSheetContext<'ctx, A> {
        MacroSheetContext {
            state: runtime.current_lease(),
            callbacks: scope.callbacks(),
            _not_send_or_sync: PhantomData,
        }
    }
}

impl<'call, A: Addin> MacroSheetContext<'call, A> {
    #[must_use]
    pub fn state(&self) -> &A::State {
        self.state.state()
    }

    pub fn coerce(&self, reference: &ExcelReference<'_>) -> XllResult<ExcelValue> {
        let arguments = [reference.raw_pointer()];
        // SAFETY: the reference and argument array remain live for the callback.
        let (status, mut result) = unsafe {
            self.callbacks
                .call(XL_COERCE, &arguments)
                .map_err(|suppressed| XllError::ExcelApi {
                    function: "xlCoerce(suppressed)",
                    code: suppressed.status.raw_code(),
                })?
        };
        if status != ExcelCallbackStatus::Success {
            return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlCoerce",
                code: status.raw_code(),
            }));
        }
        let converted = <ExcelValue as crate::FromExcel>::from_excel(result.borrow()?, "reference");
        result.try_release()?;
        converted
    }

    pub fn coerce_matrix<T>(&self, reference: &ExcelReference<'_>) -> XllResult<Matrix<T>>
    where
        T: for<'value> FromExcel<'value>,
    {
        let arguments = [reference.raw_pointer()];
        // SAFETY: the reference and argument array remain live for the callback.
        let (status, mut result) = unsafe {
            self.callbacks
                .call(XL_COERCE, &arguments)
                .map_err(|suppressed| XllError::ExcelApi {
                    function: "xlCoerce(suppressed)",
                    code: suppressed.status.raw_code(),
                })?
        };
        if status != ExcelCallbackStatus::Success {
            return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlCoerce",
                code: status.raw_code(),
            }));
        }
        let converted = <Matrix<T> as FromExcel>::from_excel(result.borrow()?, "reference");
        result.try_release()?;
        converted
    }

    pub fn sheet_name(&self, reference: &ExcelReference<'_>) -> XllResult<String> {
        let arguments = [reference.raw_pointer()];
        // SAFETY: the reference and argument array remain live for the callback.
        let (status, mut result) = unsafe {
            self.callbacks
                .call(XL_SHEET_NM, &arguments)
                .map_err(|suppressed| XllError::ExcelApi {
                    function: "xlSheetNm(suppressed)",
                    code: suppressed.status.raw_code(),
                })?
        };
        if status != ExcelCallbackStatus::Success {
            return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlSheetNm",
                code: status.raw_code(),
            }));
        }
        let converted = <String as FromExcel>::from_excel(result.borrow()?, "reference");
        result.try_release()?;
        converted
    }
}

impl<A: Addin> MainThreadContext<'_, A> {
    #[doc(hidden)]
    #[must_use]
    pub fn new<'ctx, 'scope>(
        runtime: &'ctx crate::Runtime<A>,
        scope: &'ctx CallScope<'scope>,
    ) -> MainThreadContext<'ctx, A> {
        MainThreadContext {
            state: runtime.current_lease(),
            runtime,
            callbacks: scope.callbacks(),
            _not_send_or_sync: PhantomData,
        }
    }
}

impl<'call, A: Addin> MainThreadContext<'call, A> {
    #[must_use]
    pub fn state(&self) -> &A::State {
        self.state.state()
    }

    pub fn subscribe<Source>(
        &self,
        source: &crate::RtdSourceHandle<Source>,
        topic: crate::RtdTopic,
    ) -> XllResult<crate::RtdValue>
    where
        Source: crate::RtdSource,
    {
        let subscriptions = self.runtime.subscriptions();
        let subscriptions = subscriptions.as_arc();
        let prepared = subscriptions.prepare(source, topic)?;
        match crate::rtd::observe_subscription(subscriptions, prepared.key(), self.callbacks) {
            Ok(value) => {
                prepared.commit();
                Ok(value)
            }
            Err(error) => {
                prepared.rollback();
                Err(error)
            }
        }
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
    #[cfg(any(feature = "async", not(target_os = "windows")))]
    use std::sync::Arc;

    assert_impl_all!(ThreadSafeContext<'static, ()>: Copy, Clone, Send, Sync);
    assert_impl_all!(MainThreadContext<'static, ()>: Clone);
    assert_impl_all!(MacroSheetContext<'static, ()>: Clone);
    assert_impl_all!(DiagnosticsSetup<'static>: Copy, Clone, Send, Sync);
    assert_not_impl_any!(MainThreadContext<'static, ()>: Send, Sync);
    assert_not_impl_any!(MacroSheetContext<'static, ()>: Send, Sync);

    struct TestU32Addin;

    impl crate::Addin for TestU32Addin {
        type State = u32;
        type Error = crate::XllError;
        type Layers = ();

        fn open(_: &crate::OpenContext) -> Result<Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }
    }

    #[test]
    fn synchronous_contexts_expose_their_state_by_value() {
        let state = 17_u32;
        let runtime = crate::Runtime::<TestU32Addin>::new();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish(state, ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        let thread_safe = ThreadSafeContext::<TestU32Addin>::new(&state);
        crate::with_excel_call_scope(|scope| {
            let main_thread = MainThreadContext::<TestU32Addin>::new(&runtime, scope);
            let macro_sheet = MacroSheetContext::<TestU32Addin>::new(&runtime, scope);

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
        let reference: crate::ExcelReference<'_> =
            unsafe { crate::reference_from_raw("reference", &mut raw) }.unwrap();
        crate::with_excel_call_scope(|scope| {
            let runtime = crate::Runtime::<()>::new();
            let mut opening = runtime.begin_open().unwrap();
            runtime.publish((), ());
            runtime.finish_open(&mut opening, Vec::new()).unwrap();
            let context = MacroSheetContext::<()>::new(&runtime, scope);
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

        // SAFETY: disconnect_and_wait ensures safety
        unsafe impl crate::RtdSubscription for TestSubscription {
            fn request_cancel(&self) {}
            fn disconnect_and_wait(self: Box<Self>) -> crate::XllResult<()> {
                self.disconnected.store(true, Ordering::Release);
                Ok(())
            }
        }

        struct TestSource {
            disconnected: Arc<AtomicBool>,
        }

        impl crate::RtdSource for TestSource {
            type Value = f64;
            type Subscription = TestSubscription;

            fn subscribe(
                &self,
                _topic: &crate::RtdTopic,
                sink: crate::RtdSink<Self::Value>,
            ) -> crate::XllResult<Self::Subscription> {
                sink.publish(17.5)?;
                Ok(TestSubscription {
                    disconnected: Arc::clone(&self.disconnected),
                })
            }
        }

        let runtime = crate::Runtime::<()>::new();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        let subscriptions = runtime.subscriptions();
        let subscriptions = subscriptions.as_arc();
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = crate::RtdSourceHandle::new(TestSource {
            disconnected: Arc::clone(&disconnected),
        })
        .unwrap();
        let topic = crate::RtdTopic::single("shared-observation").unwrap();
        let prepared = subscriptions.prepare(&source, topic.clone()).unwrap();
        let server = subscriptions
            .register_server(crate::subscription::ServerGeneration(51))
            .unwrap();
        let key_obj = prepared.key().clone();
        let conn = subscriptions
            .connect_transaction(&server, crate::subscription::TopicId(7), &key_obj)
            .unwrap();
        assert_eq!(
            conn.value(),
            &crate::subscription::StoredRtdValue::Number(17.5)
        );
        conn.commit().unwrap();

        let repeated = subscriptions.prepare(&source, topic.clone()).unwrap();
        assert_eq!(repeated.key(), &key_obj);
        assert!(!repeated.has_reservation());
        repeated.rollback();

        let _state = ();
        crate::with_excel_call_scope(|scope| {
            let context = MainThreadContext::new(&runtime, scope);
            assert!(matches!(
                context.subscribe(&source, topic),
                Err(crate::XllError::ExcelApi {
                    function: "xlfRtd",
                    code: xlfn_sys::XLRET_FAILED,
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
        type State = u32;
        type Error = crate::XllError;
        type Layers = ();

        fn open(
            _context: &crate::OpenContext,
        ) -> Result<Opened<Self::State, Self::Layers>, Self::Error> {
            Ok(Opened::new(23, ()))
        }
    }

    #[cfg(feature = "async")]
    #[test]
    fn async_context_checks_and_exposes_cancellation() {
        let (source, token) = crate::cancellation::CancellationSource::new(
            crate::CancellationGuarantee::CalculationScoped,
        );
        let lease = crate::runtime::GenerationLease::<AsyncTestAddin> {
            generation: Arc::new(crate::runtime::OpenGeneration {
                state: 23_u32,
                layers: (),
            }),
        };
        let context = AsyncContext::new(lease, token);

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
        let build_info = crate::BuildInfo::new(
            crate::AddinId::parse("test-addin").unwrap(),
            "0.1.0",
            "x86_64-pc-windows-msvc",
        );
        let context =
            crate::OpenContext::new(std::path::PathBuf::from("/test/module.xll"), build_info);
        let diag = context.diagnostics();
        let copied = diag;
        assert_eq!(
            diag.context.build_info().version(),
            copied.context.build_info().version()
        );
    }
}
