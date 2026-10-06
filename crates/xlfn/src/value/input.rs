//! Worksheet-input conversion and call-boundary state.

use crate::call::CallScope;
use crate::input_identity::{InputFingerprintBuilder, InputIdentityEncoder};
use crate::{XllError, XllResult};
use xlfn_sys::XLOPER12;

use super::{XlValueRef, XlValueType};

pub(crate) mod sealed {
    pub trait InputModeSealed {}

    pub trait ExcelParameterSealed<'call, M: super::InputMode> {}
}

/// Input conversion mode selected by the return type of the UDF.
#[doc(hidden)]
pub trait InputMode: sealed::InputModeSealed + Sized {
    const RECORDS_IDENTITY: bool;
    type Identity;
    type Fingerprint;

    #[doc(hidden)]
    fn new_fingerprint(argument_count: usize) -> Self::Fingerprint;

    #[doc(hidden)]
    fn with_argument<R>(
        fingerprint: &mut Self::Fingerprint,
        index: usize,
        argument: &'static str,
        encode: impl FnOnce(&mut Self::Identity) -> XllResult<R>,
    ) -> XllResult<R>;

    #[doc(hidden)]
    fn finish(fingerprint: Self::Fingerprint) -> XllResult<Option<[u8; 32]>>;

    #[doc(hidden)]
    fn tag(identity: &mut Self::Identity, value: u8);

    #[doc(hidden)]
    fn bool(identity: &mut Self::Identity, value: bool);

    #[doc(hidden)]
    fn f64(identity: &mut Self::Identity, value: f64);

    #[doc(hidden)]
    fn i64(identity: &mut Self::Identity, value: i64);

    #[doc(hidden)]
    fn u64(identity: &mut Self::Identity, value: u64);

    #[doc(hidden)]
    fn string(identity: &mut Self::Identity, value: &str);
}

/// Plain worksheet conversion, without formula-revision identity recording.
#[doc(hidden)]
pub struct PlainInputMode;

/// Formula-revision worksheet conversion with semantic identity recording.
#[doc(hidden)]
pub struct FormulaInputMode;

impl sealed::InputModeSealed for PlainInputMode {}
impl sealed::InputModeSealed for FormulaInputMode {}

impl InputMode for PlainInputMode {
    const RECORDS_IDENTITY: bool = false;
    type Identity = ();
    type Fingerprint = ();

    fn new_fingerprint(_: usize) -> Self::Fingerprint {}

    fn with_argument<R>(
        _: &mut Self::Fingerprint,
        _: usize,
        _: &'static str,
        encode: impl FnOnce(&mut Self::Identity) -> XllResult<R>,
    ) -> XllResult<R> {
        let mut identity = ();
        encode(&mut identity)
    }

    fn finish(_: Self::Fingerprint) -> XllResult<Option<[u8; 32]>> {
        Ok(None)
    }

    fn tag(_: &mut Self::Identity, _: u8) {}
    fn bool(_: &mut Self::Identity, _: bool) {}
    fn f64(_: &mut Self::Identity, _: f64) {}
    fn i64(_: &mut Self::Identity, _: i64) {}
    fn u64(_: &mut Self::Identity, _: u64) {}
    fn string(_: &mut Self::Identity, _: &str) {}
}

impl InputMode for FormulaInputMode {
    const RECORDS_IDENTITY: bool = true;
    type Identity = InputIdentityEncoder;
    type Fingerprint = InputFingerprintBuilder;

    fn new_fingerprint(argument_count: usize) -> Self::Fingerprint {
        InputFingerprintBuilder::new(argument_count)
    }

    fn with_argument<R>(
        fingerprint: &mut Self::Fingerprint,
        index: usize,
        argument: &'static str,
        encode: impl FnOnce(&mut Self::Identity) -> XllResult<R>,
    ) -> XllResult<R> {
        fingerprint.with_argument(index, argument, encode)
    }

    fn finish(fingerprint: Self::Fingerprint) -> XllResult<Option<[u8; 32]>> {
        fingerprint
            .finish()
            .map(|fingerprint| Some(*fingerprint.as_bytes()))
    }

    fn tag(identity: &mut Self::Identity, value: u8) {
        identity.tag(value);
    }

    fn bool(identity: &mut Self::Identity, value: bool) {
        identity.bool(value);
    }

    fn f64(identity: &mut Self::Identity, value: f64) {
        identity.f64(value);
    }

    fn i64(identity: &mut Self::Identity, value: i64) {
        identity.i64(value);
    }

    fn u64(identity: &mut Self::Identity, value: u64) {
        identity.u64(value);
    }

    fn string(identity: &mut Self::Identity, value: &str) {
        identity.string(value);
    }
}

/// Converts a call-scoped Excel value into owned Rust data.
///
/// Custom collection types can compose the standard presence, shape, and
/// allocation policies through [`crate::value::convert`].
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be converted from an Excel argument",
    label = "`{Self}` does not implement `FromExcel`",
    note = "implement `FromExcel` for this argument type or use a supported argument type"
)]
pub trait FromExcel<'call>: Sized {
    /// Converts a validated input view using `argument` for error context.
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self>;

    /// Converts an owned input while recording its semantic identity.
    ///
    /// The default composes conversion and identity encoding. Collection
    /// implementations can override it to record identity during conversion,
    /// avoiding a second traversal. An override must produce the same value,
    /// errors, and encoded identity as `from_excel` followed by
    /// [`ExcelInputIdentity::encode_input_identity`].
    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self>
    where
        Self: ExcelInputIdentity,
    {
        let converted = Self::from_excel(value, argument)?;
        converted.encode_input_identity(identity);
        Ok(converted)
    }
}

/// Encodes the semantic value observed by a formula-revision UDF.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be hashed as a formula-revision input identity",
    label = "`{Self}` does not implement `ExcelInputIdentity`",
    note = "implement `ExcelInputIdentity` for `{Self}` to support formula revision tracking"
)]
pub trait ExcelInputIdentity {
    /// Records exactly the converted semantic value, including shape/presence.
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder);
}

/// Preparation contract for semantic inputs of handle-producing functions.
/// Preparation validates and records the complete identity before lookup.
/// The returned state owns or borrows everything materialization needs; it is
/// consumed only on a miss. Its value and errors must match `FromExcel` and
/// `ExcelInputIdentity`. Neither stage may depend on changing external state.
/// All input-dependent validation must finish in `prepare`: a warm hit skips
/// `materialize` entirely. Prepared state may be dropped without materializing
/// on a hit or a later argument error. Materialization consumes it at most once.
/// The supported extension points are `Prepared`, `prepare`, and `materialize`;
/// hidden dispatch hooks are framework implementation details.
pub trait PrepareExcel<'call>: FromExcel<'call> + ExcelInputIdentity {
    /// Validated owned or borrowed state retained until a cache miss.
    type Prepared;
    // Built-ins with borrowed collection state can defer typed scratch copies
    // without retaining a per-cell prepared allocation. Custom preparation
    // keeps its existing eager MatrixRef policy.
    #[doc(hidden)]
    const __BORROWED_ELEMENTS: bool = false;
    /// Validates the entire input and records its identity before lookup.
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self::Prepared>;
    /// Consumes prepared state once on a miss to produce the user argument.
    fn materialize(prepared: Self::Prepared) -> XllResult<Self>;

    // Internal dispatch hook: stable Rust cannot specialize the blanket
    // ExcelParameter impl for built-ins. The private argument/result types
    // and hidden method keep this optimization outside the extension contract.
    #[doc(hidden)]
    fn __prepare_elements(
        cells: ExcelInputCells<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedExcelSequence<'call, Self, Self::Prepared>> {
        cells.retain(
            |value, argument| Self::prepare(value, argument, identity),
            Self::materialize,
        )
    }
}

/// Validated collection shape and borrowed cells supplied by the framework.
/// Iteration applies the same cell and allocation budgets as ordinary input.
#[doc(hidden)]
pub struct ExcelInputCells<'call> {
    pub(crate) grid: super::GridView<'call>,
    pub(crate) argument: &'static str,
}
impl<'call> ExcelInputCells<'call> {
    /// Retains typed preparation results, including owned custom state.
    pub fn retain<T, P>(
        self,
        mut prepare: impl FnMut(XlValueRef<'call>, &'static str) -> XllResult<P>,
        materialize: fn(P) -> XllResult<T>,
    ) -> XllResult<PreparedExcelSequence<'call, T, P>> {
        let mut budget = super::ArrayInputBudget::new::<T>(self.grid.cells().len(), self.argument)?;
        let _prepared_budget =
            super::ArrayInputBudget::new::<P>(self.grid.cells().len(), self.argument)?;
        let mut values = Vec::with_capacity(self.grid.cells().len());
        for cell in self.grid.cells() {
            let value = XlValueRef::from_array_cell(cell)?;
            budget.include(value)?;
            values.push(prepare(value, self.argument)?);
        }
        Ok(PreparedExcelSequence {
            state: SequenceState::Retained(values, materialize),
        })
    }

    pub(crate) fn borrowed<T: PrepareExcel<'call>>(
        self,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedExcelSequence<'call, T, T::Prepared>> {
        let mut budget = super::ArrayInputBudget::new::<T>(self.grid.cells().len(), self.argument)?;
        for cell in self.grid.cells() {
            let value = XlValueRef::from_array_cell(cell)?;
            budget.include(value)?;
            // Built-in preparations have no owned resource or side effect.
            T::prepare(value, self.argument, identity)?;
        }
        Ok(PreparedExcelSequence {
            state: SequenceState::Borrowed(self, T::from_excel),
        })
    }

    pub(crate) fn borrowed_f64(
        self,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedExcelSequence<'call, f64, f64>> {
        let mut budget =
            super::ArrayInputBudget::new::<f64>(self.grid.cells().len(), self.argument)?;
        let argument = self.argument;
        identity.f64_sequence(self.grid.cells().iter().map(|cell| {
            let value = XlValueRef::from_array_cell(cell)?;
            budget.include(value)?;
            f64::from_excel(value, argument)
        }))?;
        Ok(PreparedExcelSequence {
            state: SequenceState::Borrowed(self, f64::from_excel),
        })
    }
}

/// Prepared collection with opaque storage; callers cannot forge validation.
#[doc(hidden)]
pub struct PreparedExcelSequence<'call, T, P> {
    state: SequenceState<'call, T, P>,
}
enum SequenceState<'call, T, P> {
    Retained(Vec<P>, fn(P) -> XllResult<T>),
    Borrowed(
        ExcelInputCells<'call>,
        fn(XlValueRef<'call>, &'static str) -> XllResult<T>,
    ),
}
impl<T, P> PreparedExcelSequence<'_, T, P> {
    pub fn materialize(self) -> XllResult<Vec<T>> {
        match self.state {
            SequenceState::Retained(values, materialize) => {
                values.into_iter().map(materialize).collect()
            }
            SequenceState::Borrowed(cells, materialize) => {
                let mut values = Vec::with_capacity(cells.grid.cells().len());
                for cell in cells.grid.cells() {
                    values.push(materialize(
                        XlValueRef::from_array_cell(cell)?,
                        cells.argument,
                    )?);
                }
                Ok(values)
            }
        }
    }

    fn materialize_borrowed<'call>(self, scope: &'call CallScope<'call>) -> XllResult<&'call [T]>
    where
        T: Copy,
    {
        match self.state {
            SequenceState::Retained(values, materialize) => {
                let length = values.len();
                let mut values = values.into_iter();
                scope.scratch().collect_copy(length, |_| {
                    materialize(values.next().expect("prepared sequence keeps its length"))
                })
            }
            SequenceState::Borrowed(cells, materialize) => {
                scope
                    .scratch()
                    .collect_copy(cells.grid.cells().len(), |index| {
                        materialize(
                            XlValueRef::from_array_cell(&cells.grid.cells()[index])?,
                            cells.argument,
                        )
                    })
            }
        }
    }
}

/// Internal wrapper also retaining explicitly supplied default values.
#[doc(hidden)]
pub enum PreparedArgument<T, P> {
    Ready(T),
    Prepared {
        value: P,
        materialize: fn(P) -> XllResult<T>,
    },
}
impl<T, P> PreparedArgument<T, P> {
    pub fn materialize(self) -> XllResult<T> {
        match self {
            Self::Ready(value) => Ok(value),
            Self::Prepared { value, materialize } => materialize(value),
        }
    }
}

/// Framework-side argument dispatch used by generated ABI wrappers.
#[doc(hidden)]
pub trait ExcelParameter<'call, M: InputMode>:
    sealed::ExcelParameterSealed<'call, M> + Sized
{
    const DEFER_BORROWED_MATRIX: bool = false;
    type Prepared;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedArgument<Self, Self::Prepared>> {
        Self::decode(value, argument, context, identity).map(PreparedArgument::Ready)
    }
    type Elements;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements>;
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>>;

    fn materialize_elements_borrowed(
        elements: Self::Elements,
        scope: &'call CallScope<'call>,
    ) -> XllResult<&'call [Self]>
    where
        Self: Copy,
    {
        let values = Self::materialize_elements(elements)?;
        scope
            .scratch()
            .collect_copy(values.len(), |index| Ok(values[index]))
    }

    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self>;

    fn encode_decoded(&self, identity: &mut M::Identity);
}

impl<'call, T: FromExcel<'call>> sealed::ExcelParameterSealed<'call, PlainInputMode> for T {}

impl<'call, T: FromExcel<'call>> ExcelParameter<'call, PlainInputMode> for T {
    type Prepared = ();
    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut (),
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| Self::prepare(value, argument, context, identity),
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }

    // Keep this forwarding layer transparent in per-cell conversion loops.
    // The concrete FromExcel implementation still controls its own inlining.
    #[inline(always)]
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        _: &mut (),
    ) -> XllResult<Self> {
        T::from_excel(value, argument)
    }

    fn encode_decoded(&self, _: &mut ()) {}
}

impl<'call, T> sealed::ExcelParameterSealed<'call, FormulaInputMode> for T where
    T: PrepareExcel<'call>
{
}

impl<'call, T> ExcelParameter<'call, FormulaInputMode> for T
where
    T: PrepareExcel<'call>,
{
    const DEFER_BORROWED_MATRIX: bool = T::__BORROWED_ELEMENTS;
    type Prepared = T::Prepared;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        _: &CallContext<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedArgument<Self, Self::Prepared>> {
        Ok(PreparedArgument::Prepared {
            value: T::prepare(value, argument, identity)?,
            materialize: T::materialize,
        })
    }
    type Elements = PreparedExcelSequence<'call, T, T::Prepared>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        _: &CallContext<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self::Elements> {
        T::__prepare_elements(cells, identity)
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }

    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        T::from_excel_with_identity(value, argument, identity)
    }

    fn materialize_elements_borrowed(
        elements: Self::Elements,
        scope: &'call CallScope<'call>,
    ) -> XllResult<&'call [Self]>
    where
        Self: Copy,
    {
        elements.materialize_borrowed(scope)
    }

    fn encode_decoded(&self, identity: &mut InputIdentityEncoder) {
        self.encode_input_identity(identity);
    }
}

/// Runtime services that travel together through one Excel-visible call.
#[cfg(feature = "handles")]
pub(crate) struct HandleCallAccess<'call> {
    pub(crate) runtime: crate::handle::FormulaHandleServiceResolver<'call>,
}

/// Runtime services available to one admitted Excel-visible call.
///
/// Generation services are independent from the optional formula-handle
/// capability. Keeping them as separate fields means RTD access does not
/// acquire a handle resolver, and a core-only build has no handle access path.
struct RuntimeCallAccess<'call> {
    scope: &'call CallScope<'call>,
    #[cfg(feature = "handles")]
    handles: crate::handle::FormulaHandleServiceResolver<'call>,
    #[cfg(feature = "rtd")]
    rtd: crate::rtd::RtdGenerationAccess<'call>,
}

/// Runtime services available while converting one Excel-visible argument.
#[doc(hidden)]
pub struct CallContext<'call> {
    access: CallAccess<'call>,
}

/// The call either has plain conversion access or the runtime services for an
/// admitted generation.
enum CallAccess<'call> {
    Plain(&'call CallScope<'call>),
    Runtime(RuntimeCallAccess<'call>),
    #[cfg(all(test, feature = "handles"))]
    HandleOnly {
        scope: &'call CallScope<'call>,
        handles: crate::handle::FormulaHandleServiceResolver<'call>,
    },
}

impl<'call> CallContext<'call> {
    pub(crate) fn plain(scope: &'call CallScope<'call>) -> Self {
        Self {
            access: CallAccess::Plain(scope),
        }
    }

    pub(crate) fn with_call<A: crate::Addin>(
        call: &'call crate::runtime::CallGuard<'_, A>,
        scope: &'call CallScope<'call>,
    ) -> Self {
        #[cfg(all(not(feature = "handles"), not(feature = "rtd")))]
        let _ = call;
        Self {
            access: CallAccess::Runtime(RuntimeCallAccess {
                scope,
                #[cfg(feature = "handles")]
                handles: call.handle_call_access(),
                #[cfg(feature = "rtd")]
                rtd: call.rtd_call_access(),
            }),
        }
    }

    #[cfg(all(test, feature = "handles"))]
    pub(crate) fn from_handle_access(
        scope: &'call CallScope<'call>,
        handles: crate::handle::FormulaHandleServiceResolver<'call>,
    ) -> Self {
        Self {
            access: CallAccess::HandleOnly { scope, handles },
        }
    }

    #[cfg(test)]
    pub(crate) fn from_scope(scope: &'call CallScope<'call>) -> Self {
        Self {
            access: CallAccess::Plain(scope),
        }
    }

    #[cfg(feature = "rtd")]
    pub(crate) fn rtd_access(&self) -> crate::rtd::RtdGenerationAccess<'call> {
        match &self.access {
            CallAccess::Runtime(access) => access.rtd,
            CallAccess::Plain(_) => {
                panic!("plain conversion context has no RTD access")
            }
            #[cfg(all(test, feature = "handles"))]
            CallAccess::HandleOnly { .. } => {
                panic!("handle-only conversion context has no RTD access")
            }
        }
    }

    pub(crate) fn scratch(&self) -> &'call crate::call::CallScratch {
        self.scope().scratch()
    }

    pub(crate) fn scope(&self) -> &'call CallScope<'call> {
        match &self.access {
            CallAccess::Plain(scope) => scope,
            CallAccess::Runtime(access) => access.scope,
            #[cfg(all(test, feature = "handles"))]
            CallAccess::HandleOnly { scope, .. } => scope,
        }
    }

    #[cfg(feature = "handles")]
    pub(crate) fn take_handle_access(&mut self) -> Option<HandleCallAccess<'call>> {
        let scope = match &self.access {
            CallAccess::Plain(scope) => *scope,
            CallAccess::Runtime(access) => access.scope,
            #[cfg(all(test, feature = "handles"))]
            CallAccess::HandleOnly { scope, .. } => *scope,
        };
        match std::mem::replace(&mut self.access, CallAccess::Plain(scope)) {
            CallAccess::Runtime(access) => Some(HandleCallAccess {
                runtime: access.handles,
            }),
            #[cfg(all(test, feature = "handles"))]
            CallAccess::HandleOnly { handles, .. } => Some(HandleCallAccess { runtime: handles }),
            CallAccess::Plain(_) => None,
        }
    }

    #[cfg(feature = "handles")]
    pub(crate) fn resolve_handle<T: crate::handle::ExcelHandleObject>(
        &self,
        token: &str,
    ) -> XllResult<crate::handle::Handle<'call, T>> {
        let (handles, scope) = match &self.access {
            CallAccess::Runtime(access) => (&access.handles, access.scope),
            #[cfg(all(test, feature = "handles"))]
            CallAccess::HandleOnly { handles, scope } => (handles, *scope),
            CallAccess::Plain(_) => {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_NO_CONTEXT,
                });
            }
        };
        handles.get()?.lookup(scope, token)
    }

    /// Resolves and pins a handle during async-UDF argument decoding. The
    /// pending value is not exposed to user code; the generated boundary
    /// brands it immediately before committing the task.
    #[cfg(all(feature = "async", feature = "handles"))]
    pub(crate) fn resolve_pending_handle<T: crate::handle::ExcelHandleObject>(
        &self,
        token: &str,
        generation: crate::generation::RuntimeGeneration,
    ) -> XllResult<crate::handle::PendingHandleLease<T>> {
        let handle = self.resolve_handle::<T>(token)?;
        handle.into_pending(generation)
    }
}

/// Call-scoped argument conversion and formula-revision identity collection.
#[doc(hidden)]
pub struct ArgumentContext<'call, M: InputMode> {
    pub(crate) call: CallContext<'call>,
    pub(crate) inputs: Option<M::Fingerprint>,
}

impl<'call, M: InputMode> ArgumentContext<'call, M> {
    pub fn new<A: crate::Addin>(
        call: &'call crate::runtime::CallGuard<'_, A>,
        scope: &'call CallScope<'call>,
        argument_count: usize,
    ) -> Self {
        Self {
            call: CallContext::with_call(call, scope),
            inputs: Some(M::new_fingerprint(argument_count)),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_scope(scope: &'call CallScope<'call>, argument_count: usize) -> Self {
        Self {
            call: CallContext::from_scope(scope),
            inputs: Some(M::new_fingerprint(argument_count)),
        }
    }

    #[cfg(all(test, feature = "handles"))]
    pub(crate) fn from_handle_access(
        scope: &'call CallScope<'call>,
        handles: crate::handle::FormulaHandleServiceResolver<'call>,
        argument_count: usize,
    ) -> Self {
        Self {
            call: CallContext::from_handle_access(scope, handles),
            inputs: Some(M::new_fingerprint(argument_count)),
        }
    }

    #[cfg(feature = "rtd")]
    pub(crate) fn rtd_access(&self) -> crate::rtd::RtdGenerationAccess<'call> {
        self.call.rtd_access()
    }

    #[cfg(feature = "handles")]
    pub(crate) fn take_handle_access(&mut self) -> HandleCallAccess<'call> {
        self.call
            .take_handle_access()
            .expect("formula argument context must retain handle access")
    }

    pub fn finish(&mut self) -> XllResult<Option<[u8; 32]>> {
        self.inputs.take().map_or(Ok(None), M::finish)
    }

    pub(crate) fn prepare<T: ExcelParameter<'call, M>>(
        &mut self,
        index: usize,
        argument: &'static str,
        value: XlValueRef<'call>,
    ) -> XllResult<PreparedArgument<T, T::Prepared>> {
        let fingerprint = self.inputs.as_mut().ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
        })?;
        M::with_argument(fingerprint, index, argument, |identity| {
            T::prepare(value, argument, &self.call, identity)
        })
    }

    pub(crate) fn decode<T>(
        &mut self,
        index: usize,
        argument: &'static str,
        value: XlValueRef<'call>,
    ) -> XllResult<T>
    where
        T: ExcelParameter<'call, M>,
    {
        let fingerprint = self.inputs.as_mut().ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
        })?;
        let call = &self.call;
        M::with_argument(fingerprint, index, argument, |identity| {
            T::decode(value, argument, call, identity)
        })
    }

    #[cfg(all(feature = "async", feature = "handles"))]
    pub(crate) fn decode_pending_handle<T: crate::handle::ExcelHandleObject>(
        &mut self,
        index: usize,
        argument: &'static str,
        value: XlValueRef<'call>,
        generation: crate::generation::RuntimeGeneration,
    ) -> XllResult<crate::handle::PendingHandleLease<T>> {
        let fingerprint = self.inputs.as_mut().ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
        })?;
        let call = &self.call;
        M::with_argument(fingerprint, index, argument, |identity| {
            let pending = crate::handle::with_utf16_handle_token(
                value.utf16(argument)?,
                argument,
                |token| call.resolve_pending_handle::<T>(token, generation),
            )?;
            M::u64(identity, pending.object_id.session());
            M::u64(identity, pending.object_id.sequence());
            Ok(pending)
        })
    }

    pub(crate) fn record_decoded<T>(
        &mut self,
        index: usize,
        argument: &'static str,
        value: &T,
    ) -> XllResult<()>
    where
        T: ExcelParameter<'call, M>,
    {
        let fingerprint = self.inputs.as_mut().ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
        })?;
        M::with_argument(fingerprint, index, argument, |identity| {
            T::encode_decoded(value, identity);
            Ok(())
        })
    }
}

/// Converts one raw Excel argument at the generated ABI boundary.
///
/// # Safety
///
/// The pointer must satisfy `XlValueRef::from_raw` for the duration of the
/// conversion.
#[doc(hidden)]
pub unsafe fn argument_from_raw<'call, T>(
    scope: &'call CallScope<'call>,
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<T>
where
    T: ExcelParameter<'call, PlainInputMode>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    T::decode(borrowed, argument, &CallContext::plain(scope), &mut ())
}

#[doc(hidden)]
#[cfg(all(test, feature = "handles"))]
pub(crate) unsafe fn argument_from_raw_with_context<'call, T>(
    scope: &'call CallScope<'call>,
    slot: &'call crate::handle::FormulaHandleServiceSlot,
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<T>
where
    T: ExcelParameter<'call, PlainInputMode>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    T::decode(
        borrowed,
        argument,
        &CallContext::from_handle_access(
            scope,
            crate::handle::FormulaHandleServiceResolver::new(slot),
        ),
        &mut (),
    )
}

/// Converts one raw Excel argument and records its framework identity.
#[doc(hidden)]
pub unsafe fn argument_from_raw_with_arguments<'call, M, T>(
    arguments: &mut ArgumentContext<'call, M>,
    index: usize,
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    arguments.decode(index, argument, borrowed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub enum CellPresence {
    Value,
    Blank,
    Missing,
}

/// Reads only Excel's presence marker without converting the contained value.
#[doc(hidden)]
pub unsafe fn cell_presence_from_raw(
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<CellPresence> {
    // SAFETY: this function forwards its caller's raw-value contract.
    let value = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    Ok(match value.value_type() {
        XlValueType::Nil => CellPresence::Blank,
        XlValueType::Missing => CellPresence::Missing,
        _ => CellPresence::Value,
    })
}

#[cfg(test)]
mod preparation_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn array_root(cells: &mut [XLOPER12]) -> XLOPER12 {
        XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                array: xlfn_sys::XLOPER12Array {
                    rows: cells.len() as i32,
                    columns: 1,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: xlfn_sys::XLTYPE_MULTI,
        }
    }

    #[test]
    fn miri_prepared_borrowed_strings_preserve_identity_and_survive_chunk_growth() {
        for length in [1, 16, if cfg!(miri) { 65 } else { 4_096 }] {
            let source = (0..length)
                .map(|index| match index % 4 {
                    0 => String::new(),
                    1 => "ASCII".into(),
                    2 => "価格💡école".into(),
                    _ => "long".repeat(256),
                })
                .collect::<Vec<_>>();
            let mut strings = source
                .iter()
                .map(|text| {
                    std::iter::once(text.encode_utf16().count() as u16)
                        .chain(text.encode_utf16())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let mut cells = strings
                .iter_mut()
                .map(|units| XLOPER12 {
                    value: xlfn_sys::XLOPER12Value {
                        string: units.as_mut_ptr(),
                    },
                    xltype: xlfn_sys::XLTYPE_STR,
                })
                .collect::<Vec<_>>();
            let root = array_root(&mut cells);
            crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
                let value = XlValueRef::from_array_cell(root).unwrap();
                let mut eager = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let expected = eager.decode::<Vec<&str>>(0, "values", value).unwrap();
                let identity = eager.finish().unwrap();
                let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let warm = arguments.prepare::<Vec<&str>>(0, "values", value).unwrap();
                assert_eq!(arguments.finish().unwrap(), identity);
                drop(warm);

                let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let cold = arguments.prepare::<Vec<&str>>(0, "values", value).unwrap();
                assert_eq!(arguments.finish().unwrap(), identity);
                let extra = vec![b'z' as u16; 32_767];
                assert_eq!(
                    scope.scratch().decode_utf16(&extra, "extra").unwrap().len(),
                    extra.len()
                );
                let materialized = cold.materialize().unwrap();
                assert_eq!(materialized.as_slice(), expected);
                assert_eq!(
                    materialized.as_slice(),
                    source.iter().map(String::as_str).collect::<Vec<_>>()
                );
            });
        }
    }

    #[test]
    fn prepared_borrowed_strings_keep_validation_order_and_poison_identity() {
        let mut malformed = [1_u16, 0xd800];
        let text = XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                string: malformed.as_mut_ptr(),
            },
            xltype: xlfn_sys::XLTYPE_STR,
        };
        for cells in [[text, XLOPER12::number(1.0)], [XLOPER12::number(1.0), text]] {
            let mut cells = cells;
            let root = array_root(&mut cells);
            crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
                let value = XlValueRef::from_array_cell(root).unwrap();
                let mut eager = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let expected = eager.decode::<Vec<&str>>(0, "values", value).unwrap_err();
                let mut prepared = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let error = match prepared.prepare::<Vec<&str>>(0, "values", value) {
                    Ok(_) => panic!("invalid input must fail during preparation"),
                    Err(error) => error,
                };
                assert_eq!(error.to_string(), expected.to_string());
                assert!(prepared.finish().is_err());
            });
        }
    }

    #[test]
    fn custom_prepared_resources_drop_on_warm_cold_and_partial_error_paths() {
        use std::sync::Arc;
        struct Resource {
            value: f64,
            drops: Arc<AtomicUsize>,
            _owned: Box<[u8; 32]>,
        }
        impl Drop for Resource {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }
        fn materialize(value: Resource) -> XllResult<f64> {
            Ok(value.value)
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let mut cells = [
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
        ];
        let root = array_root(&mut cells);
        for cold in [false, true] {
            crate::call::with_excel_call_scope_and_state(&root, |root, _scope| {
                let grid = super::super::GridView::from_value(
                    XlValueRef::from_array_cell(root).unwrap(),
                    "values",
                )
                .unwrap();
                let prepared = ExcelInputCells {
                    grid,
                    argument: "values",
                }
                .retain(
                    |value, argument| {
                        Ok(Resource {
                            value: f64::from_excel(value, argument)?,
                            drops: Arc::clone(&drops),
                            _owned: Box::new([0; 32]),
                        })
                    },
                    materialize,
                )
                .unwrap();
                if cold {
                    assert_eq!(prepared.materialize().unwrap(), [1.0, 2.0, 3.0]);
                } else {
                    drop(prepared);
                }
            });
        }
        assert_eq!(drops.load(Ordering::Relaxed), 6);
        cells[2] = XLOPER12::error(crate::ExcelError::Value.code());
        let root = array_root(&mut cells);
        crate::call::with_excel_call_scope_and_state(&root, |root, _scope| {
            let grid = super::super::GridView::from_value(
                XlValueRef::from_array_cell(root).unwrap(),
                "values",
            )
            .unwrap();
            let result = ExcelInputCells {
                grid,
                argument: "values",
            }
            .retain(
                |value, argument| {
                    Ok(Resource {
                        value: f64::from_excel(value, argument)?,
                        drops: Arc::clone(&drops),
                        _owned: Box::new([0; 32]),
                    })
                },
                materialize,
            );
            assert!(result.is_err());
        });
        assert_eq!(drops.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn miri_prepared_containers_preserve_identity_and_materialized_values() {
        fn check<T>(raw: &XLOPER12)
        where
            T: for<'a> ExcelParameter<'a, FormulaInputMode>,
        {
            crate::call::with_excel_call_scope_and_state(raw, |raw, scope| {
                let value = XlValueRef::from_array_cell(raw).unwrap();
                let mut eager = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let _ = eager.decode::<T>(0, "values", value).unwrap();
                let expected = eager.finish().unwrap();
                let mut prepared = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let state = prepared.prepare::<T>(0, "values", value).unwrap();
                assert_eq!(prepared.finish().unwrap(), expected);
                let result = state.materialize().unwrap();
                let mut observed = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                observed.record_decoded(0, "values", &result).unwrap();
                assert_eq!(observed.finish().unwrap(), expected);
            });
        }
        let mut cells = vec![XLOPER12::number(-0.0), XLOPER12::number(2.0)];
        let mut raw = XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                array: xlfn_sys::XLOPER12Array {
                    rows: 1,
                    columns: 2,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: xlfn_sys::XLTYPE_MULTI,
        };
        check::<Option<crate::value::Matrix<f64>>>(&raw);
        check::<crate::value::OptionalExcelValue<crate::value::Matrix<f64>>>(&raw);
        check::<Vec<f64>>(&raw);
        check::<crate::value::Row<f64>>(&raw);
        check::<crate::value::BoundedVarArgs<f64, 2>>(&raw);
        raw.value = xlfn_sys::XLOPER12Value {
            array: xlfn_sys::XLOPER12Array {
                rows: 2,
                columns: 1,
                values: cells.as_mut_ptr(),
            },
        };
        check::<crate::value::Column<f64>>(&raw);
    }

    #[test]
    fn custom_conversion_is_retained_once_before_revision_lookup() {
        static CONVERSIONS: AtomicUsize = AtomicUsize::new(0);
        struct Observed(f64);
        impl<'call> FromExcel<'call> for Observed {
            fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
                CONVERSIONS.fetch_add(1, Ordering::Relaxed);
                f64::from_excel(value, argument).map(Self)
            }
        }
        impl<'call> PrepareExcel<'call> for Observed {
            type Prepared = Box<f64>;
            fn prepare(
                value: XlValueRef<'call>,
                argument: &'static str,
                identity: &mut InputIdentityEncoder,
            ) -> XllResult<Self::Prepared> {
                Self::from_excel_with_identity(value, argument, identity)
                    .map(|value| Box::new(value.0))
            }
            fn materialize(value: Self::Prepared) -> XllResult<Self> {
                Ok(Self(*value))
            }
        }
        impl ExcelInputIdentity for Observed {
            fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
                encoder.f64(self.0);
            }
        }
        let raw = XLOPER12::number(42.0);
        crate::call::with_excel_call_scope_and_state(&raw, |raw, scope| {
            let value = XlValueRef::from_array_cell(raw).unwrap();
            let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let prepared = arguments.prepare::<Observed>(0, "arg", value).unwrap();
            assert!(matches!(prepared, PreparedArgument::Prepared { .. }));
            assert_eq!(CONVERSIONS.load(Ordering::Relaxed), 1);
            assert!(arguments.finish().unwrap().is_some());
            assert_eq!(prepared.materialize().unwrap().0, 42.0);
            assert_eq!(CONVERSIONS.load(Ordering::Relaxed), 1);
            let mut nested = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let prepared = nested
                .prepare::<Option<Vec<Observed>>>(0, "arg", value)
                .unwrap();
            assert_eq!(CONVERSIONS.load(Ordering::Relaxed), 2);
            assert_eq!(prepared.materialize().unwrap().unwrap()[0].0, 42.0);
            assert_eq!(CONVERSIONS.load(Ordering::Relaxed), 2);
        });
    }
}
