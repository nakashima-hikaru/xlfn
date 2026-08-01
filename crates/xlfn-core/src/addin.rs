use crate::{
    ExcelCallbackValue, ExcelReference, FromExcel, IntoXllError, Matrix, OwnedExcelValue, UdfLayer,
    XllError, XllResult,
};
use std::marker::PhantomData;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use xlfn_sys::{XL_COERCE, XL_SHEET_NM, XLRET_SUCCESS};

#[derive(Clone, Debug)]
pub struct BuildInfo {
    pub addin_id: &'static str,
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
/// The framework invokes [`Self::open`] and [`Self::close`] from Excel's main
/// lifecycle thread, and both hooks for one open generation run on that same
/// thread. Implementations may therefore keep non-`Send` lifecycle owners in
/// thread-local storage while placing only their `Send + Sync` handles in
/// [`Self::State`]. `close` must synchronously recover and release every such
/// owner before returning.
pub trait Addin: Send + Sync + 'static {
    type State: Send + Sync + 'static;
    type Error: IntoXllError;

    /// Creates Add-in state on Excel's main lifecycle thread.
    fn open(context: &OpenContext) -> Result<Self::State, Self::Error>;

    /// Optional hook called prior to framework handle registry shutdown.
    ///
    /// Implementations should release any `Handle<T>` instances or framework
    /// resources stored inside `State` here so that the handle registry can wait
    /// for lease zero without deadlocking.
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

    /// Releases Add-in state on the same Excel main lifecycle thread used by
    /// [`Self::open`], after active framework calls and async tasks have
    /// drained.
    ///
    /// Returning `Ok(())` certifies that every Add-in-owned callback, worker,
    /// native module owner, and other execution resource is quiescent. A panic
    /// or `Err` leaves unload safety unknown, so the framework fail-stops rather
    /// than returning from `xlAutoClose` while code from this XLL may still run.
    /// The hook is therefore terminal and is never retried after failure.
    ///
    /// Vendor operations that can block indefinitely must be canceled
    /// cooperatively. Otherwise Excel unload waits for the operation rather
    /// than returning while code from this XLL can still execute.
    fn close(_state: &mut Self::State) -> Result<(), Self::Error> {
        Ok(())
    }
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

impl<S> Deref for MainThreadContext<'_, S> {
    type Target = S;
    fn deref(&self) -> &Self::Target {
        self.state
    }
}

impl<S> AsRef<S> for MainThreadContext<'_, S> {
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

#[derive(Clone, Copy)]
pub struct MainThreadContext<'call, S> {
    state: &'call S,
    runtime: &'call crate::Runtime<S>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

#[derive(Clone, Copy)]
pub struct MacroSheetContext<'call, S> {
    state: &'call S,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl<S> Deref for MacroSheetContext<'_, S> {
    type Target = S;
    fn deref(&self) -> &Self::Target {
        self.state
    }
}

impl<S> AsRef<S> for MacroSheetContext<'_, S> {
    fn as_ref(&self) -> &S {
        self.state
    }
}

impl<'call, S> MacroSheetContext<'call, S> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new(state: &'call S) -> Self {
        Self {
            state,
            _not_send_or_sync: PhantomData,
        }
    }

    #[must_use]
    pub const fn state(&self) -> &'call S {
        self.state
    }

    pub fn coerce(&self, reference: &ExcelReference<'_>) -> XllResult<OwnedExcelValue> {
        let arguments = [reference.raw_pointer()];
        // SAFETY: the reference and argument array remain live for the callback.
        let (status, mut result) = unsafe { ExcelCallbackValue::call(XL_COERCE, &arguments) };
        if status != XLRET_SUCCESS {
            return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlCoerce",
                code: status,
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
        T: FromExcel,
    {
        let arguments = [reference.raw_pointer()];
        // SAFETY: the reference and argument array remain live for the callback.
        let (status, mut result) = unsafe { ExcelCallbackValue::call(XL_COERCE, &arguments) };
        if status != XLRET_SUCCESS {
            return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlCoerce",
                code: status,
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
        let (status, mut result) = unsafe { ExcelCallbackValue::call(XL_SHEET_NM, &arguments) };
        if status != XLRET_SUCCESS {
            return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlSheetNm",
                code: status,
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

impl<'call, S> MainThreadContext<'call, S> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new(state: &'call S, runtime: &'call crate::Runtime<S>) -> Self {
        Self {
            state,
            runtime,
            _not_send_or_sync: PhantomData,
        }
    }

    #[must_use]
    pub const fn state(&self) -> &'call S {
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
        match crate::rtd::observe_subscription(Arc::clone(&subscriptions), prepared.key()) {
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
    assert_impl_all!(MainThreadContext<'static, ()>: Copy, Clone);
    assert_impl_all!(MacroSheetContext<'static, ()>: Copy, Clone);
    assert_not_impl_any!(MainThreadContext<'static, ()>: Send, Sync);
    assert_not_impl_any!(MacroSheetContext<'static, ()>: Send, Sync);

    #[test]
    fn synchronous_contexts_expose_their_state_by_value() {
        let state = 17_u32;
        let runtime = crate::Runtime::new();
        let thread_safe = ThreadSafeContext::new(&state);
        let main_thread = MainThreadContext::new(&state, &runtime);
        let macro_sheet = MacroSheetContext::new(&state);

        assert_eq!(thread_safe.state(), &17);
        assert_eq!(main_thread.state(), &17);
        assert_eq!(macro_sheet.state(), &17);

        let copied = main_thread;
        assert_eq!(copied.state(), &17);
        assert_eq!(main_thread.state(), &17);
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
        let context = MainThreadContext::new(&state, &runtime);
        assert!(matches!(
            context.subscribe(source, topic),
            Err(crate::XllError::ExcelApi {
                function: "xlfRtd",
                code: xlfn_sys::XLRET_FAILED,
            })
        ));

        let updates = subscriptions.snapshot_updates(51);
        assert_eq!(updates.updates.len(), 1);
        assert_eq!(updates.updates[0].topic_id, 7);
        assert_eq!(updates.updates[0].value, crate::RtdValue::Number(17.5));
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
