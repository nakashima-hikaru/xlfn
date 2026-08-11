use super::*;

/// Marker implemented by `#[derive(ExcelHandleObject)]`.
///
/// A handle-producing UDF is memoized by its formula identity.
/// For one live formula identity, the producer is evaluated at most once
/// and the resulting handle token identifies that object for the token's
/// entire lifetime.
///
/// Producers must therefore depend only on their Excel-visible inputs and
/// stable application state explicitly represented by those inputs.
pub trait ExcelHandleObject: Any + Send + Sync + 'static {}

/// A typed, call-safe reference to an object owned by an Excel handle topic.
pub struct Handle<T: ExcelHandleObject> {
    // Options let Drop release the value under a panic boundary before
    // returning the runtime lease. This records a destructor failure even when
    // a formula topic was already removed and the Handle owns the final Arc.
    pub(crate) value: Option<Arc<T>>,
    pub(crate) lease: Option<HandleLease>,
}

impl<T: ExcelHandleObject> Handle<T> {
    pub(crate) fn into_arc(mut self) -> Arc<T> {
        let value = self
            .value
            .take()
            .expect("a live Handle contains its object reference");
        // The caller immediately republishes this Arc while its UDF CallGuard
        // still prevents terminal handle shutdown.
        drop(self.lease.take());
        value
    }
}

impl<T: ExcelHandleObject> Clone for Handle<T> {
    fn clone(&self) -> Self {
        Self {
            value: Some(Arc::clone(
                self.value
                    .as_ref()
                    .expect("a live Handle contains its object reference"),
            )),
            lease: Some(
                self.lease
                    .as_ref()
                    .expect("a live Handle contains its runtime lease")
                    .clone(),
            ),
        }
    }
}

impl<T: ExcelHandleObject> Deref for Handle<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value
            .as_deref()
            .expect("a live Handle contains its object reference")
    }
}

impl<T: ExcelHandleObject> Drop for Handle<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take()
            && catch_unwind(AssertUnwindSafe(|| drop(value))).is_err()
        {
            let error = XllError::Panic;
            crate::diagnostics::report_no_unwind("handle lease drop", &error);
            if let Some(lease) = self.lease.as_ref() {
                lease.record_cleanup_failure(error);
            }
        }
        // A cleanup failure must be recorded before the lease count reaches
        // zero and wakes terminal shutdown.
        drop(self.lease.take());
    }
}

impl<T: ExcelHandleObject> crate::ExcelReturn for Handle<T> {
    type Output = String;

    fn into_excel(self, context: &mut ReturnContext<'_, '_>) -> XllResult<Self::Output> {
        context.publish_existing_handle(|| Ok(self))
    }

    fn invoke(
        context: &mut ReturnContext<'_, '_>,
        operation: impl FnOnce() -> XllResult<Self>,
    ) -> XllResult<String> {
        context.publish_existing_handle(operation)
    }
}

impl<T: ExcelHandleObject> crate::value::MainThreadReturn for Handle<T> {}
