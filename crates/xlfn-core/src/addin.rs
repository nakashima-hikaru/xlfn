use crate::host_callback::HostCallbackSession;
use crate::{
    AddinId, CallScope, CleanupReporter, ExcelCallbackStatus, ExcelReference, FromExcel,
    IntoXllError, Matrix, OwnedExcelValue, UdfLayer, XllError, XllResult,
};
use std::marker::PhantomData;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use xlfn_sys::{XL_COERCE, XL_SHEET_NM};

#[derive(Clone, Debug)]
pub struct BuildInfo {
    pub addin_id: AddinId,
    pub version: &'static str,
    pub target: &'static str,
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

    /// Creates Add-in state on Excel's main lifecycle thread.
    fn open(context: &OpenContext) -> Result<Self::State, Self::Error>;

    /// Stops every Add-in-owned callback, worker, native module owner, and
    /// other source that could execute XLL code after unload.
    ///
    /// Returning `Ok(())` certifies that every such execution resource is
    /// quiescent. A panic or `Err` leaves unload safety unknown, so the
    /// framework fail-stops rather than returning from `xlAutoClose` while code
    /// from this XLL may still run. The hook is terminal and is never retried.
    ///
    /// Implementations must also release any `Handle<T>` instances stored in
    /// `State` so that the framework registry can reach lease zero. Vendor
    /// operations must be canceled cooperatively; unload waits rather than
    /// abandoning in-process code.
    fn quiesce(_state: &mut Self::State) -> Result<(), Self::Error> {
        Ok(())
    }

    fn udf_layers(_state: &Self::State) -> Vec<Arc<dyn UdfLayer>> {
        Vec::new()
    }

    /// Number of worker threads used by native async worksheet functions.
    /// Values are clamped to `1..=32`; the default is at most four threads.
    #[cfg(feature = "async")]
    fn async_worker_count(_state: &Self::State) -> usize {
        std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(4)
    }

    /// Performs best-effort disposal after quiescence has been established.
    ///
    /// This hook must not start work or register callbacks. Disposal failures
    /// should be recorded with `reporter`; they do not make unload unsafe.
    fn cleanup(_state: &mut Self::State, _reporter: &mut CleanupReporter<'_>) {}
}

/// Static metadata and lifecycle configuration supplied by `#[excel_addin]`.
pub trait AddinMetadata {
    const ID: &'static str;
    const DISPLAY_NAME: &'static str;
    const DEFAULT_CATEGORY: &'static str;
}

impl<S> Deref for ThreadSafeContext<'_, S> {
    type Target = S;
    fn deref(&self) -> &Self::Target {
        self.state
    }
}

impl<S> AsRef<S> for ThreadSafeContext<'_, S> {
    fn as_ref(&self) -> &S {
        self.state
    }
}

#[derive(Clone, Copy)]
pub struct ThreadSafeContext<'call, S> {
    state: &'call S,
}

/// Owned Add-in state available to an asynchronous worksheet function.
///
/// The context must remain owned by the generated async invocation. Moving it
/// into a detached thread or task that can outlive the returned future violates
/// the XLL shutdown contract: Excel may unload the module immediately after
/// `xlAutoClose`. The runtime detects an escaped state lease during shutdown
/// and terminates the process rather than returning with executable XLL code
/// still reachable.
#[cfg(feature = "async")]
pub struct AsyncContext<S> {
    state: Arc<S>,
    cancellation: crate::CancellationToken,
}

#[cfg(feature = "async")]
impl<S> AsyncContext<S> {
    #[doc(hidden)]
    #[must_use]
    pub fn new(state: Arc<S>, cancellation: crate::CancellationToken) -> Self {
        Self {
            state,
            cancellation,
        }
    }

    #[must_use]
    pub fn state(&self) -> &S {
        &self.state
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
impl<S> Deref for AsyncContext<S> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

#[cfg(feature = "async")]
impl<S> AsRef<S> for AsyncContext<S> {
    fn as_ref(&self) -> &S {
        &self.state
    }
}

impl<S> Deref for MainThreadContext<'_, '_, S> {
    type Target = S;
    fn deref(&self) -> &Self::Target {
        self.state
    }
}

impl<S> AsRef<S> for MainThreadContext<'_, '_, S> {
    fn as_ref(&self) -> &S {
        self.state
    }
}

impl<'call, S> ThreadSafeContext<'call, S> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new(state: &'call S) -> Self {
        Self { state }
    }

    #[must_use]
    pub const fn state(&self) -> &'call S {
        self.state
    }
}

#[derive(Clone)]
pub struct MainThreadContext<'state, 'scope, S> {
    state: &'state S,
    runtime: &'state crate::Runtime<S>,
    callbacks: &'scope HostCallbackSession,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

#[derive(Clone)]
pub struct MacroSheetContext<'state, 'scope, S> {
    state: &'state S,
    callbacks: &'scope HostCallbackSession,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl<S> Deref for MacroSheetContext<'_, '_, S> {
    type Target = S;
    fn deref(&self) -> &Self::Target {
        self.state
    }
}

impl<S> AsRef<S> for MacroSheetContext<'_, '_, S> {
    fn as_ref(&self) -> &S {
        self.state
    }
}

impl<'state, 'scope, S> MacroSheetContext<'state, 'scope, S> {
    #[doc(hidden)]
    #[must_use]
    pub fn new(state: &'state S, scope: &'scope CallScope<'scope>) -> Self {
        Self {
            state,
            callbacks: scope.callbacks(),
            _not_send_or_sync: PhantomData,
        }
    }

    #[must_use]
    pub const fn state(&self) -> &'state S {
        self.state
    }

    pub fn coerce(&self, reference: &ExcelReference<'_>) -> XllResult<OwnedExcelValue> {
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
        let converted = OwnedExcelValue::from_excel(
            result.borrow()?,
            "reference",
            &crate::CallContext::without_runtime(),
        );
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
        let converted = Matrix::<T>::from_excel(
            result.borrow()?,
            "reference",
            &crate::CallContext::without_runtime(),
        );
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
        let converted = String::from_excel(
            result.borrow()?,
            "reference",
            &crate::CallContext::without_runtime(),
        );
        result.try_release()?;
        converted
    }
}

impl<'state, 'scope, S> MainThreadContext<'state, 'scope, S> {
    #[doc(hidden)]
    #[must_use]
    pub fn new(
        state: &'state S,
        runtime: &'state crate::Runtime<S>,
        scope: &'scope CallScope<'scope>,
    ) -> Self {
        Self {
            state,
            runtime,
            callbacks: scope.callbacks(),
            _not_send_or_sync: PhantomData,
        }
    }

    #[must_use]
    pub const fn state(&self) -> &'state S {
        self.state
    }

    pub fn subscribe<Source>(
        &self,
        source: Arc<Source>,
        topic: crate::RtdTopic,
    ) -> XllResult<crate::RtdValue>
    where
        Source: crate::RtdSource,
    {
        let subscriptions = self.runtime.subscriptions();
        let prepared = subscriptions.prepare(source, topic)?;
        match crate::rtd::observe_subscription(
            Arc::clone(&subscriptions),
            prepared.key(),
            self.callbacks,
        ) {
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
    use super::{MacroSheetContext, MainThreadContext, ThreadSafeContext};
    use static_assertions::{assert_impl_all, assert_not_impl_any};
    #[cfg(any(feature = "async", not(target_os = "windows")))]
    use std::sync::Arc;

    assert_impl_all!(ThreadSafeContext<'static, ()>: Copy, Clone, Send, Sync);
    assert_impl_all!(MainThreadContext<'static, 'static, ()>: Clone);
    assert_impl_all!(MacroSheetContext<'static, 'static, ()>: Clone);
    assert_not_impl_any!(MainThreadContext<'static, 'static, ()>: Send, Sync);
    assert_not_impl_any!(MacroSheetContext<'static, 'static, ()>: Send, Sync);

    #[test]
    fn synchronous_contexts_expose_their_state_by_value() {
        let state = 17_u32;
        let runtime = crate::Runtime::new();
        let thread_safe = ThreadSafeContext::new(&state);
        crate::with_excel_call_scope(|scope| {
            let main_thread = MainThreadContext::new(&state, &runtime, scope);
            let macro_sheet = MacroSheetContext::new(&state, scope);

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
        let state = ();

        crate::with_excel_call_scope(|scope| {
            let context = MacroSheetContext::new(&state, scope);
            assert!(context.sheet_name(&reference).is_err());
            let _ = context.coerce(&reference);
            assert_eq!(crate::test_callback::total_calls(), 1);
            assert_eq!(crate::test_callback::free_calls(), 0);
        });
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn failed_rtd_observation_preserves_the_existing_shared_subscription() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct TestSubscription(Arc<AtomicBool>);

        // SAFETY: disconnect_and_wait ensures no background work accesses module code.
        unsafe impl crate::RtdSubscription for TestSubscription {
            fn request_cancel(&self) {}

            fn disconnect_and_wait(self: Box<Self>) -> crate::XllResult<()> {
                self.0.store(true, Ordering::Release);
                Ok(())
            }
        }

        struct TestSource {
            disconnected: Arc<AtomicBool>,
        }

        impl crate::RtdSource for TestSource {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &crate::RtdTopic,
                sink: crate::RtdSink<Self::Value>,
            ) -> crate::XllResult<Box<dyn crate::RtdSubscription>> {
                sink.publish(17.5)?;
                Ok(Box::new(TestSubscription(Arc::clone(&self.disconnected))))
            }
        }

        let runtime = crate::Runtime::<()>::new();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        let subscriptions = runtime.subscriptions();
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = Arc::new(TestSource {
            disconnected: Arc::clone(&disconnected),
        });
        let topic = crate::RtdTopic::single("shared-observation").unwrap();
        let prepared = subscriptions
            .prepare(Arc::clone(&source), topic.clone())
            .unwrap();
        assert_eq!(
            subscriptions.connect(51, 7, prepared.key()).unwrap(),
            crate::RtdValue::Number(17.5)
        );

        let state = ();
        crate::with_excel_call_scope(|scope| {
            let context = MainThreadContext::new(&state, &runtime, scope);
            assert!(matches!(
                context.subscribe(source, topic),
                Err(crate::XllError::ExcelApi {
                    function: "xlfRtd",
                    code: xlfn_sys::XLRET_FAILED,
                })
            ));
        });

        let updates = subscriptions.snapshot_updates(51);
        assert!(updates.updates.is_empty());
        assert_eq!(
            subscriptions.connect(51, 7, prepared.key()).unwrap(),
            crate::RtdValue::Number(17.5),
            "the original topic owner remains installed"
        );
        assert!(!disconnected.load(Ordering::Acquire));

        subscriptions.disconnect(51, 7);
        assert!(disconnected.load(Ordering::Acquire));
    }

    #[cfg(feature = "async")]
    #[test]
    fn async_context_checks_and_exposes_cancellation() {
        let (source, token) = crate::cancellation::CancellationSource::new(
            crate::CancellationGuarantee::CalculationScoped,
        );
        let context = AsyncContext::new(Arc::new(23_u32), token);

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
}
