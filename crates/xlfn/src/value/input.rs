//! Worksheet-input conversion and call-boundary state.

use crate::call::CallScope;
#[cfg(any(feature = "handles", test))]
use crate::input_identity::InputFingerprintBuilder;
use crate::input_identity::InputIdentityEncoder;
use crate::{XllError, XllResult};
use std::convert::Infallible;
use std::marker::PhantomData;
use xlfn_sys::XLOPER12;

use super::{XlValueRef, XlValueType};

pub(crate) mod sealed {
    pub trait InputModeSealed: Sized {
        fn finish_return<'call, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>(
            arguments: &mut super::ArgumentContext<'call, Self>,
            access: crate::call_return::ReturnFrameAccess<'call>,
        ) -> crate::XllResult<crate::call_return::ReturnContext<'call, 'call, Self, E, C>>
        where
            Self: super::InputMode;
    }

    pub trait ExcelParameterSealed<'call, M: super::InputMode> {}
}

/// Input conversion mode selected by the return type of the UDF.
#[doc(hidden)]
pub trait InputMode: sealed::InputModeSealed + Sized {
    const RECORDS_IDENTITY: bool;
    type Identity;
    type Fingerprint;
    type FinishedFingerprint;
    type ReturnState<'call, 'scope, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>;

    fn finish_typed(fingerprint: Self::Fingerprint) -> XllResult<Self::FinishedFingerprint>;

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
    fn f64_slice(identity: &mut Self::Identity, values: &[f64]) -> XllResult<()>;

    #[doc(hidden)]
    fn i64(identity: &mut Self::Identity, value: i64);

    #[doc(hidden)]
    fn u64(identity: &mut Self::Identity, value: u64);

    #[doc(hidden)]
    fn string(identity: &mut Self::Identity, value: &str);

    #[doc(hidden)]
    fn prepare_native_string<'call>(
        identity: &mut Self::Identity,
        units: &'call [u16],
        argument: &'static str,
    ) -> XllResult<PreparedArgument<String, super::PreparedString<'call>, OwnedStringMaterializer>>;

    #[doc(hidden)]
    fn prepare_borrowed_string<'call>(
        identity: &mut Self::Identity,
        value: super::PreparedBorrowedString<'call>,
    ) -> XllResult<
        PreparedArgument<
            &'call str,
            super::PreparedBorrowedString<'call>,
            BorrowedStringMaterializer,
        >,
    >;
}

/// Plain worksheet conversion, without formula-revision identity recording.
#[doc(hidden)]
pub struct PlainInputMode;

/// Formula-revision worksheet conversion with semantic identity recording.
#[doc(hidden)]
#[cfg(any(feature = "handles", test))]
pub struct FormulaInputMode;

impl sealed::InputModeSealed for PlainInputMode {
    fn finish_return<'call, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>(
        arguments: &mut ArgumentContext<'call, Self>,
        access: crate::call_return::ReturnFrameAccess<'call>,
    ) -> XllResult<crate::call_return::ReturnContext<'call, 'call, Self, E, C>> {
        let _ = (access.udf_id, access.callbacks);
        arguments.finish_typed()?;
        Ok(crate::call_return::ReturnContext::from_state(()))
    }
}
#[cfg(any(feature = "handles", test))]
impl sealed::InputModeSealed for FormulaInputMode {
    fn finish_return<'call, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>(
        arguments: &mut ArgumentContext<'call, Self>,
        access: crate::call_return::ReturnFrameAccess<'call>,
    ) -> XllResult<crate::call_return::ReturnContext<'call, 'call, Self, E, C>> {
        let inputs = arguments.finish_typed()?;
        #[cfg(feature = "handles")]
        let state = crate::call_return::FormulaPublisher::new(
            arguments.take_handle_access(),
            access.udf_id,
            inputs,
            access.callbacks,
        );
        #[cfg(not(feature = "handles"))]
        let state = {
            let _ = (inputs, access);
        };
        Ok(crate::call_return::ReturnContext::from_state(state))
    }
}

impl InputMode for PlainInputMode {
    const RECORDS_IDENTITY: bool = false;
    type Identity = ();
    type Fingerprint = ();
    type FinishedFingerprint = ();
    type ReturnState<'call, 'scope, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>> = ();
    fn finish_typed(_: ()) -> XllResult<()> {
        Ok(())
    }

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
    fn f64_slice(_: &mut Self::Identity, _: &[f64]) -> XllResult<()> {
        Ok(())
    }
    fn i64(_: &mut Self::Identity, _: i64) {}
    fn u64(_: &mut Self::Identity, _: u64) {}
    fn string(_: &mut Self::Identity, _: &str) {}

    fn prepare_native_string<'call>(
        _: &mut Self::Identity,
        units: &'call [u16],
        argument: &'static str,
    ) -> XllResult<PreparedArgument<String, super::PreparedString<'call>, OwnedStringMaterializer>>
    {
        crate::utf16::decode_owned(units, argument).map(PreparedArgument::Ready)
    }

    fn prepare_borrowed_string<'call>(
        _: &mut Self::Identity,
        value: super::PreparedBorrowedString<'call>,
    ) -> XllResult<
        PreparedArgument<
            &'call str,
            super::PreparedBorrowedString<'call>,
            BorrowedStringMaterializer,
        >,
    > {
        value.materialize().map(PreparedArgument::Ready)
    }
}

#[cfg(any(feature = "handles", test))]
impl InputMode for FormulaInputMode {
    const RECORDS_IDENTITY: bool = true;
    type Identity = InputIdentityEncoder;
    type Fingerprint = InputFingerprintBuilder;
    type FinishedFingerprint = [u8; 32];
    #[cfg(feature = "handles")]
    type ReturnState<'call, 'scope, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>> =
        crate::call_return::FormulaPublisher<'call, 'scope, E, C>;
    #[cfg(not(feature = "handles"))]
    type ReturnState<'call, 'scope, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>> = ();
    fn finish_typed(fingerprint: Self::Fingerprint) -> XllResult<[u8; 32]> {
        fingerprint.finish().map(|value| *value.as_bytes())
    }

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

    fn f64_slice(identity: &mut Self::Identity, values: &[f64]) -> XllResult<()> {
        identity.f64_sequence(values.iter().copied().map(Ok))
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

    fn prepare_native_string<'call>(
        identity: &mut Self::Identity,
        units: &'call [u16],
        argument: &'static str,
    ) -> XllResult<PreparedArgument<String, super::PreparedString<'call>, OwnedStringMaterializer>>
    {
        identity.semantic_utf16(units)?;
        Ok(PreparedArgument::prepared(super::PreparedString {
            units,
            argument,
        }))
    }

    fn prepare_borrowed_string<'call>(
        identity: &mut Self::Identity,
        value: super::PreparedBorrowedString<'call>,
    ) -> XllResult<
        PreparedArgument<
            &'call str,
            super::PreparedBorrowedString<'call>,
            BorrowedStringMaterializer,
        >,
    > {
        identity.semantic_utf16(value.units)?;
        Ok(PreparedArgument::prepared(value))
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
    #[inline(always)]
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
        cells.retain(|value, argument| Self::prepare(value, argument, identity))
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
    pub fn retain<T, P, D>(
        self,
        mut prepare: impl FnMut(XlValueRef<'call>, &'static str) -> XllResult<P>,
    ) -> XllResult<PreparedExcelSequence<'call, T, P, D>> {
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
            state: SequenceState::Retained(values),
            _materializer: PhantomData,
        })
    }

    fn wrapped_budget<T, P, const RETAINED: bool>(
        length: usize,
        argument: &'static str,
    ) -> XllResult<super::ArrayInputBudget> {
        let budget = super::ArrayInputBudget::new::<T>(length, argument)?;
        if RETAINED {
            super::ArrayInputBudget::new::<P>(length, argument)?;
        }
        Ok(budget)
    }

    // Owned preparation preserves its former prepared-size admission check;
    // MatrixRef preserves its eager path's output-only budget instead.
    #[inline(always)]
    pub(super) fn borrowed_wrapped<T, P, D, const RETAINED_BUDGET: bool>(
        self,
        mut validate: impl FnMut(XlValueRef<'call>, &'static str) -> XllResult<()>,
    ) -> XllResult<PreparedExcelSequence<'call, T, P, D>> {
        let mut budget =
            Self::wrapped_budget::<T, P, RETAINED_BUDGET>(self.grid.cells().len(), self.argument)?;
        for cell in self.grid.cells() {
            let value = XlValueRef::from_array_cell(cell)?;
            budget.include(value)?;
            validate(value, self.argument)?;
        }
        Ok(PreparedExcelSequence {
            state: SequenceState::Borrowed(self),
            _materializer: PhantomData,
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
            state: SequenceState::Borrowed(self),
            _materializer: PhantomData,
        })
    }

    #[inline(always)]
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
            state: SequenceState::Borrowed(self),
            _materializer: PhantomData,
        })
    }
}

/// A statically selected conversion for one owned preparation state.
#[doc(hidden)]
pub trait ArgumentMaterializer<T, P> {
    fn materialize(value: P) -> XllResult<T>;
}

/// No deferred state can be constructed for an already converted argument.
#[doc(hidden)]
pub struct NoMaterialization;
impl<T> ArgumentMaterializer<T, Infallible> for NoMaterialization {
    fn materialize(value: Infallible) -> XllResult<T> {
        match value {}
    }
}

#[doc(hidden)]
pub struct OwnedStringMaterializer;
impl<'call> ArgumentMaterializer<String, super::PreparedString<'call>> for OwnedStringMaterializer {
    fn materialize(value: super::PreparedString<'call>) -> XllResult<String> {
        <String as PrepareExcel<'call>>::materialize(value)
    }
}

#[doc(hidden)]
pub struct BorrowedStringMaterializer;
impl<'call> ArgumentMaterializer<&'call str, super::PreparedBorrowedString<'call>>
    for BorrowedStringMaterializer
{
    fn materialize(value: super::PreparedBorrowedString<'call>) -> XllResult<&'call str> {
        value.materialize()
    }
}

// Conversion strategy identity must not change through payload subtyping.
// The call borrow may still narrow without changing the payload or mode.
type MaterializerMarker<'call, T, M = ()> = PhantomData<(&'call (), fn(T, M) -> (T, M))>;

#[doc(hidden)]
pub struct ParameterMaterializer<'call, T, M>(MaterializerMarker<'call, T, M>);
impl<'call, T: ExcelParameter<'call, M>, M: InputMode> ArgumentMaterializer<T, T::Prepared>
    for ParameterMaterializer<'call, T, M>
{
    fn materialize(value: T::Prepared) -> XllResult<T> {
        T::materialize_prepared(value)
    }
}

#[doc(hidden)]
pub struct ElementMaterializer<'call, T, M>(MaterializerMarker<'call, T, M>);
impl<'call, T: ExcelParameter<'call, M>, M: InputMode> ArgumentMaterializer<T, T::ElementPrepared>
    for ElementMaterializer<'call, T, M>
{
    fn materialize(value: T::ElementPrepared) -> XllResult<T> {
        T::materialize_element(value)
    }
}

#[doc(hidden)]
pub type PreparedParameter<'call, T, M> = PreparedArgument<
    T,
    <T as ExcelParameter<'call, M>>::Prepared,
    ParameterMaterializer<'call, T, M>,
>;
#[doc(hidden)]
pub type PreparedElement<'call, T, M> = PreparedArgument<
    T,
    <T as ExcelParameter<'call, M>>::ElementPrepared,
    ElementMaterializer<'call, T, M>,
>;
#[doc(hidden)]
pub type PreparedParameterSequence<'call, T, M> =
    PreparedExcelSequence<'call, T, PreparedElement<'call, T, M>, ParameterSequence<'call, T, M>>;
#[doc(hidden)]
pub type ReadyArgument<T> = PreparedArgument<T, Infallible>;

/// Static conversions for retained preparation and validated borrowed cells.
#[doc(hidden)]
pub trait SequenceMaterializer<'call, T, P> {
    fn retained(value: P) -> XllResult<T>;
    fn borrowed(value: XlValueRef<'call>, argument: &'static str) -> XllResult<T>;
}
#[doc(hidden)]
pub struct PrepareSequence<'call, T>(MaterializerMarker<'call, T>);
impl<'call, T: PrepareExcel<'call>> SequenceMaterializer<'call, T, T::Prepared>
    for PrepareSequence<'call, T>
{
    fn retained(value: T::Prepared) -> XllResult<T> {
        T::materialize(value)
    }
    fn borrowed(value: XlValueRef<'call>, argument: &'static str) -> XllResult<T> {
        T::from_excel(value, argument)
    }
}
#[doc(hidden)]
pub struct ParameterSequence<'call, T, M>(MaterializerMarker<'call, T, M>);
impl<'call, T: ExcelParameter<'call, M>, M: InputMode>
    SequenceMaterializer<'call, T, PreparedElement<'call, T, M>>
    for ParameterSequence<'call, T, M>
{
    fn retained(value: PreparedElement<'call, T, M>) -> XllResult<T> {
        value.materialize()
    }
    fn borrowed(value: XlValueRef<'call>, argument: &'static str) -> XllResult<T> {
        T::decode_borrowed_element(value, argument)
    }
}

/// Prepared collection with opaque storage; callers cannot forge validation.
#[doc(hidden)]
pub struct PreparedExcelSequence<'call, T, P, D = PrepareSequence<'call, T>> {
    state: SequenceState<'call, P>,
    _materializer: PhantomData<fn() -> (T, D)>,
}
enum SequenceState<'call, P> {
    Retained(Vec<P>),
    Borrowed(ExcelInputCells<'call>),
}
impl<'source, T, P, D: SequenceMaterializer<'source, T, P>>
    PreparedExcelSequence<'source, T, P, D>
{
    pub fn materialize(self) -> XllResult<Vec<T>> {
        match self.state {
            SequenceState::Retained(values) => values.into_iter().map(D::retained).collect(),
            SequenceState::Borrowed(cells) => {
                let mut values = Vec::with_capacity(cells.grid.cells().len());
                for cell in cells.grid.cells() {
                    values.push(D::borrowed(
                        XlValueRef::from_array_cell(cell)?,
                        cells.argument,
                    )?);
                }
                Ok(values)
            }
        }
    }

    pub(super) fn materialize_borrowed<'call>(
        self,
        scope: &'call CallScope<'call>,
    ) -> XllResult<&'call [T]>
    where
        T: Copy,
        'source: 'call,
    {
        match self.state {
            SequenceState::Retained(values) => {
                let length = values.len();
                let mut values = values.into_iter();
                scope.scratch().collect_copy(length, |_| {
                    D::retained(values.next().expect("prepared sequence keeps its length"))
                })
            }
            SequenceState::Borrowed(cells) => {
                scope
                    .scratch()
                    .collect_copy(cells.grid.cells().len(), |index| {
                        D::borrowed(
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
pub enum PreparedArgument<T, P = Infallible, D = NoMaterialization> {
    Ready(T),
    Prepared {
        value: P,
        _materializer: PhantomData<fn() -> D>,
    },
}
impl<T, P, D> PreparedArgument<T, P, D> {
    pub(super) fn prepared(value: P) -> Self {
        Self::Prepared {
            value,
            _materializer: PhantomData,
        }
    }
    pub(crate) fn with_materializer<N>(self) -> PreparedArgument<T, P, N> {
        match self {
            Self::Ready(value) => PreparedArgument::Ready(value),
            Self::Prepared { value, .. } => PreparedArgument::prepared(value),
        }
    }
}
impl<T, P, D: ArgumentMaterializer<T, P>> PreparedArgument<T, P, D> {
    pub fn materialize(self) -> XllResult<T> {
        match self {
            Self::Ready(value) => Ok(value),
            Self::Prepared { value, .. } => D::materialize(value),
        }
    }
}

/// Framework-side argument dispatch used by generated ABI wrappers.
#[doc(hidden)]
pub trait ExcelParameter<'call, M: InputMode>:
    sealed::ExcelParameterSealed<'call, M> + Sized
{
    // Only context-free built-ins may be decoded again after lookup. Custom
    // prepared ownership and call-scoped resources must stay retained.
    const BORROWED_ELEMENTS: bool = false;
    fn decode_borrowed_element(_: XlValueRef<'call>, _: &'static str) -> XllResult<Self> {
        unreachable!("only context-free element strategies can decode borrowed cells")
    }
    type Prepared;
    fn materialize_prepared(value: Self::Prepared) -> XllResult<Self>;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedParameter<'call, Self, M>> {
        Self::decode(value, argument, context, identity).map(PreparedArgument::Ready)
    }
    // Scalar preparation may carry state that is unnecessary for retained
    // collection cells. Keep their storage and admission budgets independent.
    type ElementPrepared;
    fn materialize_element(value: Self::ElementPrepared) -> XllResult<Self>;
    fn prepare_element(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedElement<'call, Self, M>>;

    type Elements;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements>;
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>>;

    fn prepare_elements_borrowed(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        Self::prepare_elements(cells, context, identity)
    }

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
    type Prepared = Infallible;
    fn materialize_prepared(value: Infallible) -> XllResult<Self> {
        match value {}
    }
    type ElementPrepared = Self::Prepared;
    fn materialize_element(value: Infallible) -> XllResult<Self> {
        match value {}
    }
    fn prepare_element(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut (),
    ) -> XllResult<PreparedElement<'call, Self, PlainInputMode>> {
        <Self as ExcelParameter<'call, PlainInputMode>>::prepare(value, argument, context, identity)
            .map(PreparedArgument::with_materializer)
    }

    type Elements = PreparedParameterSequence<'call, Self, PlainInputMode>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut (),
    ) -> XllResult<Self::Elements> {
        cells.retain(|value, argument| Self::prepare_element(value, argument, context, identity))
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

#[cfg(any(feature = "handles", test))]
impl<'call, T> sealed::ExcelParameterSealed<'call, FormulaInputMode> for T where
    T: PrepareExcel<'call>
{
}

#[cfg(any(feature = "handles", test))]
impl<'call, T> ExcelParameter<'call, FormulaInputMode> for T
where
    T: PrepareExcel<'call>,
{
    const BORROWED_ELEMENTS: bool = T::__BORROWED_ELEMENTS;
    fn decode_borrowed_element(
        value: XlValueRef<'call>,
        argument: &'static str,
    ) -> XllResult<Self> {
        T::from_excel(value, argument)
    }
    type Prepared = T::Prepared;
    fn materialize_prepared(value: T::Prepared) -> XllResult<Self> {
        T::materialize(value)
    }
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        _: &CallContext<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedParameter<'call, Self, FormulaInputMode>> {
        Ok(PreparedArgument::prepared(T::prepare(
            value, argument, identity,
        )?))
    }
    type ElementPrepared = Self::Prepared;
    fn materialize_element(value: Self::ElementPrepared) -> XllResult<Self> {
        T::materialize(value)
    }
    fn prepare_element(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedElement<'call, Self, FormulaInputMode>> {
        <Self as ExcelParameter<'call, FormulaInputMode>>::prepare(
            value, argument, context, identity,
        )
        .map(PreparedArgument::with_materializer)
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
        #[cfg(not(feature = "handles"))]
        let _ = call;
        Self {
            access: CallAccess::Runtime(RuntimeCallAccess {
                scope,
                #[cfg(feature = "handles")]
                handles: call.handle_call_access(),
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

    #[cfg(feature = "handles")]
    pub(crate) fn take_handle_access(&mut self) -> HandleCallAccess<'call> {
        self.call
            .take_handle_access()
            .expect("formula argument context must retain handle access")
    }

    fn finish_typed(&mut self) -> XllResult<M::FinishedFingerprint> {
        let fingerprint = self.inputs.take().ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
        })?;
        M::finish_typed(fingerprint)
    }

    pub fn finish(&mut self) -> XllResult<Option<[u8; 32]>> {
        self.inputs.take().map_or(Ok(None), M::finish)
    }

    pub(crate) fn prepare<T: ExcelParameter<'call, M>>(
        &mut self,
        index: usize,
        argument: &'static str,
        value: XlValueRef<'call>,
    ) -> XllResult<PreparedParameter<'call, T, M>> {
        // Construct the drop-bearing error only on failure. An eager `ok_or`
        // leaves an unnecessary XllError destructor call on each valid input.
        let Some(fingerprint) = self.inputs.as_mut() else {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
            });
        };
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
        let Some(fingerprint) = self.inputs.as_mut() else {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
            });
        };
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
        let Some(fingerprint) = self.inputs.as_mut() else {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
            });
        };
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
        let Some(fingerprint) = self.inputs.as_mut() else {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
            });
        };
        M::with_argument(fingerprint, index, argument, |identity| {
            T::encode_decoded(value, identity);
            Ok(())
        })
    }

    fn with_native<T>(
        &mut self,
        index: usize,
        argument: &'static str,
        convert: impl FnOnce(&mut M::Identity) -> XllResult<T>,
    ) -> XllResult<T> {
        let Some(fingerprint) = self.inputs.as_mut() else {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
            });
        };
        M::with_argument(fingerprint, index, argument, convert)
    }

    pub(crate) fn native_f64(
        &mut self,
        index: usize,
        argument: &'static str,
        raw: f64,
    ) -> XllResult<f64> {
        self.with_native(index, argument, |identity| {
            if !raw.is_finite() {
                return Err(XllError::input(
                    argument,
                    crate::error::InputError::NonFinite,
                ));
            }
            M::f64(identity, raw);
            Ok(raw)
        })
    }

    pub(crate) fn native_bool(
        &mut self,
        index: usize,
        argument: &'static str,
        raw: i16,
    ) -> XllResult<bool> {
        self.with_native(index, argument, |identity| {
            let value = match raw {
                0 => false,
                1 => true,
                _ => {
                    return Err(XllError::input(
                        argument,
                        crate::error::InputError::Malformed("invalid Excel Boolean"),
                    ));
                }
            };
            M::bool(identity, value);
            Ok(value)
        })
    }

    pub(crate) fn native_i32(
        &mut self,
        index: usize,
        argument: &'static str,
        raw: i32,
    ) -> XllResult<i32> {
        self.with_native(index, argument, |identity| {
            M::i64(identity, i64::from(raw));
            Ok(raw)
        })
    }

    /// # Safety
    /// `raw` satisfies `utf16::borrow_counted` for the complete call lifetime.
    #[allow(unsafe_code, reason = "borrows Excel's native counted string argument")]
    pub(crate) unsafe fn prepare_native_string(
        &mut self,
        index: usize,
        argument: &'static str,
        raw: *const u16,
    ) -> XllResult<PreparedArgument<String, super::PreparedString<'call>, OwnedStringMaterializer>>
    {
        self.with_native(index, argument, |identity| {
            // SAFETY: the generated wrapper retains Excel's live input for 'call.
            let units = unsafe { crate::utf16::borrow_counted(raw, argument) }?;
            M::prepare_native_string(identity, units, argument)
        })
    }

    /// # Safety
    /// `raw` satisfies `utf16::borrow_counted` until this method returns.
    #[allow(
        unsafe_code,
        reason = "copies Excel's native counted string into owned data"
    )]
    pub(crate) unsafe fn native_string(
        &mut self,
        index: usize,
        argument: &'static str,
        raw: *const u16,
    ) -> XllResult<String> {
        self.with_native(index, argument, |identity| {
            // SAFETY: the caller retains this input through materialization;
            // neither the resulting String nor the fingerprint borrows it.
            let units = unsafe { crate::utf16::borrow_counted(raw, argument) }?;
            M::prepare_native_string(identity, units, argument)?.materialize()
        })
    }

    /// # Safety
    /// `raw` satisfies `utf16::borrow_counted` for the complete call lifetime.
    #[allow(unsafe_code, reason = "borrows Excel's native counted string argument")]
    pub(crate) unsafe fn prepare_native_str(
        &mut self,
        index: usize,
        argument: &'static str,
        raw: *const u16,
    ) -> XllResult<
        PreparedArgument<
            &'call str,
            super::PreparedBorrowedString<'call>,
            BorrowedStringMaterializer,
        >,
    > {
        let scratch = self.call.scratch();
        self.with_native(index, argument, |identity| {
            // SAFETY: the generated wrapper retains Excel's live input for 'call.
            let units = unsafe { crate::utf16::borrow_counted(raw, argument) }?;
            M::prepare_borrowed_string(
                identity,
                super::PreparedBorrowedString {
                    units,
                    argument,
                    scratch,
                },
            )
        })
    }

    pub(crate) fn record_numeric_matrix(
        &mut self,
        index: usize,
        argument: &'static str,
        value: super::NumericMatrixRef<'call>,
    ) -> XllResult<()> {
        let Some(fingerprint) = self.inputs.as_mut() else {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
            });
        };
        M::with_argument(fingerprint, index, argument, |identity| {
            M::u64(identity, value.rows() as u64);
            M::u64(identity, value.columns() as u64);
            M::f64_slice(identity, value.as_slice())?;
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

    #[test]
    fn miri_native_scalars_match_value12_semantic_identity() {
        for raw in [-0.0, 0.0, -123.25, f64::MIN, f64::MAX] {
            let input = XLOPER12::number(raw);
            crate::call::with_excel_call_scope_and_state(&input, |input, scope| {
                let value = XlValueRef::from_array_cell(input).unwrap();
                let mut ordinary = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let expected = ordinary.decode::<f64>(0, "value", value).unwrap();
                let mut native = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                assert_eq!(
                    native.native_f64(0, "value", raw).unwrap().to_bits(),
                    expected.to_bits()
                );
                assert_eq!(native.finish().unwrap(), ordinary.finish().unwrap());
            });
        }
        for raw in [false, true] {
            let input = XLOPER12::boolean(raw);
            crate::call::with_excel_call_scope_and_state(&input, |input, scope| {
                let value = XlValueRef::from_array_cell(input).unwrap();
                let mut ordinary = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let expected = ordinary.decode::<bool>(0, "value", value).unwrap();
                let mut native = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                assert_eq!(
                    native.native_bool(0, "value", i16::from(raw)).unwrap(),
                    expected
                );
                assert_eq!(native.finish().unwrap(), ordinary.finish().unwrap());
            });
        }
        for raw in [i32::MIN, -1, 0, 1, i32::MAX] {
            let input = XLOPER12::integer(raw);
            crate::call::with_excel_call_scope_and_state(&input, |input, scope| {
                let value = XlValueRef::from_array_cell(input).unwrap();
                let mut ordinary = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let expected = ordinary.decode::<i32>(0, "value", value).unwrap();
                let mut native = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                assert_eq!(native.native_i32(0, "value", raw).unwrap(), expected);
                assert_eq!(native.finish().unwrap(), ordinary.finish().unwrap());
            });
        }
    }

    #[test]
    fn miri_native_scalars_reject_invalid_values_before_identity_completion() {
        use crate::error::InputError;
        crate::call::with_excel_call_scope(|scope| {
            for raw in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                assert!(matches!(
                    arguments.native_f64(0, "value", raw),
                    Err(XllError::Input {
                        argument: "value",
                        reason: InputError::NonFinite
                    }),
                ));
                assert!(arguments.finish().is_err());
                let mut plain = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
                assert!(plain.native_f64(0, "value", raw).is_err());
            }
            for raw in [i16::MIN, -1, 2, i16::MAX] {
                let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                assert!(matches!(
                    arguments.native_bool(0, "value", raw),
                    Err(XllError::Input {
                        argument: "value",
                        reason: InputError::Malformed("invalid Excel Boolean")
                    }),
                ));
                assert!(arguments.finish().is_err());
                let mut plain = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
                assert!(plain.native_bool(0, "value", raw).is_err());
            }
        });
    }

    #[test]
    #[allow(
        unsafe_code,
        reason = "tests native counted-string input using owned live buffers"
    )]
    fn miri_native_strings_preserve_value12_identity_and_defer_owned_materialization() {
        for text in ["", "a\0b", "価格💡école", &"日💡a".repeat(257)] {
            let mut units = crate::utf16::encode_counted(text, "value", 32_767).unwrap();
            let root = XLOPER12 {
                value: xlfn_sys::XLOPER12Value {
                    string: units.as_mut_ptr(),
                },
                xltype: xlfn_sys::XLTYPE_STR,
            };
            crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
                let value = XlValueRef::from_array_cell(root).unwrap();
                let mut ordinary = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let expected = ordinary.prepare::<String>(0, "value", value).unwrap();
                let identity = ordinary.finish().unwrap();
                // SAFETY: root owns a live immutable string buffer for this scope.
                let pointer = unsafe { root.value.string };
                let mut native = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                // SAFETY: the counted buffer remains live and immutable for this call.
                let warm = unsafe { native.prepare_native_string(0, "value", pointer) }.unwrap();
                assert!(matches!(warm, PreparedArgument::Prepared { .. }));
                assert_eq!(native.finish().unwrap(), identity);
                drop(warm);
                let mut native = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                // SAFETY: the counted buffer remains live and immutable for this call.
                let cold = unsafe { native.prepare_native_string(0, "value", pointer) }.unwrap();
                assert_eq!(native.finish().unwrap(), identity);
                assert_eq!(cold.materialize().unwrap(), expected.materialize().unwrap());
                let mut borrowed = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                // SAFETY: the counted buffer remains live and immutable for this call.
                let borrowed_text =
                    unsafe { borrowed.prepare_native_str(0, "value", pointer) }.unwrap();
                assert_eq!(borrowed.finish().unwrap(), identity);
                assert_eq!(borrowed_text.materialize().unwrap(), text);
                let mut plain = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
                // SAFETY: the counted buffer remains live and immutable for this call.
                let owned = unsafe { plain.prepare_native_string(0, "value", pointer) }.unwrap();
                assert!(matches!(owned, PreparedArgument::Ready(_)));
                assert_eq!(owned.materialize().unwrap(), text);
            });
        }
    }

    #[test]
    #[allow(
        unsafe_code,
        reason = "tests native and value12 string preparation using live owned buffers"
    )]
    fn miri_prepared_scalar_borrowed_strings_defer_scratch_and_preserve_identity() {
        for text in [String::new(), "a".repeat(24), "日💡a".repeat(17)] {
            let mut units = crate::utf16::encode_counted(&text, "value", 32_767).unwrap();
            let root = XLOPER12 {
                value: xlfn_sys::XLOPER12Value {
                    string: units.as_mut_ptr(),
                },
                xltype: xlfn_sys::XLTYPE_STR,
            };
            crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
                let value = XlValueRef::from_array_cell(root).unwrap();
                // SAFETY: root's counted string is live and immutable for this scope.
                let pointer = unsafe { root.value.string };
                let mut owned = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let prepared = owned.prepare::<String>(0, "value", value).unwrap();
                let identity = owned.finish().unwrap();
                drop(prepared);

                let mut ordinary = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let warm = ordinary.prepare::<&str>(0, "value", value).unwrap();
                assert!(matches!(warm, PreparedArgument::Prepared { .. }));
                assert_eq!(ordinary.finish().unwrap(), identity);
                let _ = warm;
                let mut native = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                // SAFETY: the counted buffer remains live and immutable for this call.
                let warm = unsafe { native.prepare_native_str(0, "value", pointer) }.unwrap();
                assert!(matches!(warm, PreparedArgument::Prepared { .. }));
                assert_eq!(native.finish().unwrap(), identity);
                let _ = warm;
                #[cfg(feature = "bench-internals")]
                assert_eq!(scope.scratch().allocated_bytes(), 0);

                let mut ordinary = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let cold = ordinary.prepare::<&str>(0, "value", value).unwrap();
                assert_eq!(ordinary.finish().unwrap(), identity);
                let mut native = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                // SAFETY: the counted buffer remains live and immutable for this call.
                let native_cold =
                    unsafe { native.prepare_native_str(0, "value", pointer) }.unwrap();
                assert_eq!(native.finish().unwrap(), identity);
                #[cfg(feature = "bench-internals")]
                assert_eq!(scope.scratch().allocated_bytes(), 0);

                let ordinary_text = cold.materialize().unwrap();
                let extra = vec![b'z' as u16; if cfg!(miri) { 4_096 } else { 32_767 }];
                scope.scratch().decode_utf16(&extra, "extra").unwrap();
                let native_text = native_cold.materialize().unwrap();
                scope.scratch().decode_utf16(&extra, "extra").unwrap();
                assert_eq!(ordinary_text, text);
                assert_eq!(native_text, text);

                let mut plain = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
                let eager = plain.prepare::<&str>(0, "value", value).unwrap();
                assert!(matches!(eager, PreparedArgument::Ready(_)));
                assert_eq!(eager.materialize().unwrap(), text);
                let mut plain = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
                // SAFETY: the counted buffer remains live and immutable for this call.
                let eager = unsafe { plain.prepare_native_str(0, "value", pointer) }.unwrap();
                assert!(matches!(eager, PreparedArgument::Ready(_)));
                assert_eq!(eager.materialize().unwrap(), text);
            });
        }
    }

    #[test]
    #[allow(
        unsafe_code,
        reason = "tests malformed native string payloads using live owned buffers"
    )]
    fn miri_native_strings_validate_before_cache_lookup() {
        use crate::error::InputError;
        for mut units in [
            [1_u16, 0xd800, 0],
            [1, 0xdc00, 0],
            [2, 0xd800, 0x41],
            [2, 0x41, 0xdc00],
        ] {
            let root = XLOPER12 {
                value: xlfn_sys::XLOPER12Value {
                    string: units.as_mut_ptr(),
                },
                xltype: xlfn_sys::XLTYPE_STR,
            };
            crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
                // SAFETY: root's string buffer remains live and immutable for this scope.
                let pointer = unsafe { root.value.string };
                let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                // SAFETY: the readable count and its complete payload are live.
                let result = unsafe { arguments.prepare_native_string(0, "value", pointer) };
                assert!(matches!(
                    result,
                    Err(XllError::Input {
                        argument: "value",
                        reason: InputError::InvalidUtf16
                    })
                ));
                assert!(arguments.finish().is_err());
                let mut borrowed = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                // SAFETY: the readable count and its complete payload are live.
                let result = unsafe { borrowed.prepare_native_str(0, "value", pointer) };
                assert!(matches!(
                    result,
                    Err(XllError::Input {
                        argument: "value",
                        reason: InputError::InvalidUtf16
                    })
                ));
                assert!(borrowed.finish().is_err());
                let value = XlValueRef::from_array_cell(root).unwrap();
                let mut ordinary = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                assert!(matches!(
                    ordinary.prepare::<&str>(0, "value", value),
                    Err(XllError::Input {
                        argument: "value",
                        reason: InputError::InvalidUtf16
                    })
                ));
                assert!(ordinary.finish().is_err());
                let mut plain = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
                // SAFETY: the readable count and its complete payload are live.
                let result = unsafe { plain.prepare_native_string(0, "value", pointer) };
                assert!(result.is_err());
            });
        }
        crate::call::with_excel_call_scope(|scope| {
            let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            // SAFETY: null is rejected before dereferencing the native pointer.
            let result = unsafe { arguments.prepare_native_string(0, "value", std::ptr::null()) };
            assert!(matches!(
                result,
                Err(XllError::Input {
                    argument: "value",
                    reason: InputError::NullPointer
                })
            ));
            assert!(arguments.finish().is_err());
        });
        let aligned = [0_u16; 2];
        crate::call::with_excel_call_scope_and_state(&aligned, |aligned, scope| {
            let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let misaligned = aligned.as_ptr().map_addr(|address| address + 1);
            // SAFETY: misalignment is rejected before reading the count.
            let result = unsafe { arguments.prepare_native_string(0, "value", misaligned) };
            assert!(matches!(
                result,
                Err(XllError::Input {
                    argument: "value",
                    reason: InputError::Malformed("misaligned string pointer"),
                })
            ));
            assert!(arguments.finish().is_err());
        });
        let mut units = vec![0; 32_769];
        units[0] = 32_767;
        crate::call::with_excel_call_scope_and_state(&units, |units, scope| {
            let mut arguments = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
            // SAFETY: the live allocation contains its declared payload.
            let prepared =
                unsafe { arguments.prepare_native_string(0, "value", units.as_ptr()) }.unwrap();
            assert_eq!(prepared.materialize().unwrap().len(), 32_767);
        });
        units[0] = 32_768;
        crate::call::with_excel_call_scope_and_state(&units, |units, scope| {
            let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            // SAFETY: an over-limit count is rejected before constructing the payload slice.
            let result = unsafe { arguments.prepare_native_string(0, "value", units.as_ptr()) };
            assert!(matches!(
                result,
                Err(XllError::Input {
                    argument: "value",
                    reason: InputError::TooLarge {
                        limit: 32_767,
                        actual: 32_768
                    }
                })
            ));
            assert!(arguments.finish().is_err());
        });
    }

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
        use crate::value::{Matrix, OptionalExcelValue};
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
                macro_rules! check {
                    ($ty:ty) => {{
                        let mut eager = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                        let expected = eager.decode::<$ty>(0, "values", value).unwrap_err();
                        let mut prepared =
                            ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                        let error = match prepared.prepare::<$ty>(0, "values", value) {
                            Ok(_) => panic!("invalid input must fail during preparation"),
                            Err(error) => error,
                        };
                        assert_eq!(error.to_string(), expected.to_string());
                        assert!(prepared.finish().is_err());
                    }};
                }
                check!(Vec<&str>);
                check!(Matrix<Option<&str>>);
                check!(Matrix<OptionalExcelValue<&str>>);
                check!(Matrix<Option<OptionalExcelValue<&str>>>);
                check!(Matrix<OptionalExcelValue<Option<&str>>>);
            });
        }
    }

    #[test]
    fn prepared_borrowed_strings_preserve_collection_cell_budgets() {
        use crate::value::OptionalExcelValue;

        fn check<'call, T, P>()
        where
            T: ExcelParameter<
                    'call,
                    FormulaInputMode,
                    ElementPrepared = P,
                    Elements = PreparedParameterSequence<'call, T, FormulaInputMode>,
                >,
        {
            // The explicit P fixes the retained payload shape independently
            // of the static materializer. Check the actual allocation budget
            // without constructing a matrix near the allocation ceiling.
            let bytes_per_cell =
                size_of::<XLOPER12>() + size_of::<PreparedElement<'call, T, FormulaInputMode>>();
            let maximum = crate::value::MAX_ARRAY_BYTES / bytes_per_cell;
            assert!(
                ExcelInputCells::wrapped_budget::<
                    T,
                    PreparedElement<'call, T, FormulaInputMode>,
                    true,
                >(maximum, "v")
                .is_ok()
            );
            assert!(matches!(
                ExcelInputCells::wrapped_budget::<T, PreparedElement<'call, T, FormulaInputMode>, true>(maximum + 1, "v"),
                Err(XllError::Input {
                    argument: "v",
                    reason: crate::error::InputError::TooLarge { limit, actual },
                }) if limit == crate::value::MAX_ARRAY_BYTES
                    && actual == (maximum + 1) * bytes_per_cell
            ));
        }

        type Text<'a> = PreparedElement<'a, &'a str, FormulaInputMode>;
        check::<&str, Infallible>();
        check::<Option<&str>, Option<Text<'_>>>();
        check::<OptionalExcelValue<&str>, OptionalExcelValue<Text<'_>>>();
        check::<Option<Option<&str>>, Option<PreparedElement<'_, Option<&str>, FormulaInputMode>>>(
        );
        check::<
            Option<OptionalExcelValue<&str>>,
            Option<PreparedElement<'_, OptionalExcelValue<&str>, FormulaInputMode>>,
        >();
        check::<
            OptionalExcelValue<Option<&str>>,
            OptionalExcelValue<PreparedElement<'_, Option<&str>, FormulaInputMode>>,
        >();
    }

    #[test]
    fn miri_prepared_optional_borrowed_strings_preserve_identity_and_values() {
        use crate::value::{Matrix, MatrixRef, OptionalExcelValue};
        let mut ascii = crate::utf16::encode_counted("ascii", "v", 32_767).unwrap();
        let mut unicode = crate::utf16::encode_counted("価格💡", "v", 32_767).unwrap();
        let mut cells = [
            XLOPER12::missing(),
            XLOPER12::nil(),
            XLOPER12 {
                value: xlfn_sys::XLOPER12Value {
                    string: ascii.as_mut_ptr(),
                },
                xltype: xlfn_sys::XLTYPE_STR,
            },
            XLOPER12 {
                value: xlfn_sys::XLOPER12Value {
                    string: unicode.as_mut_ptr(),
                },
                xltype: xlfn_sys::XLTYPE_STR,
            },
        ];
        let root = array_root(&mut cells);
        crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
            let value = XlValueRef::from_array_cell(root).unwrap();
            macro_rules! check {
                ($ty:ty) => {{
                    let mut eager = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                    let expected = eager.decode::<$ty>(0, "v", value).unwrap();
                    let identity = eager.finish().unwrap();
                    let mut args = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                    let state = args.prepare::<$ty>(0, "v", value).unwrap();
                    assert_eq!(args.finish().unwrap(), identity);
                    let result = state.materialize().unwrap();
                    assert_eq!(result.as_slice(), expected.as_slice());
                    let mut materialized =
                        ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                    materialized.record_decoded(0, "v", &result).unwrap();
                    assert_eq!(materialized.finish().unwrap(), identity);
                }};
            }
            check!(Matrix<Option<&str>>);
            check!(Matrix<OptionalExcelValue<&str>>);
            check!(Matrix<Option<Option<&str>>>);
            check!(Matrix<Option<OptionalExcelValue<&str>>>);
            check!(Matrix<OptionalExcelValue<Option<&str>>>);
            check!(MatrixRef<'_, Option<&str>>);
        });
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
        struct ResourceMaterializer;
        impl<'call> SequenceMaterializer<'call, f64, Resource> for ResourceMaterializer {
            fn retained(value: Resource) -> XllResult<f64> {
                Ok(value.value)
            }
            fn borrowed(_: XlValueRef<'call>, _: &'static str) -> XllResult<f64> {
                unreachable!("retained-only fixture")
            }
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
                .retain::<f64, Resource, ResourceMaterializer>(|value, argument| {
                    Ok(Resource {
                        value: f64::from_excel(value, argument)?,
                        drops: Arc::clone(&drops),
                        _owned: Box::new([0; 32]),
                    })
                })
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
            .retain::<f64, Resource, ResourceMaterializer>(|value, argument| {
                Ok(Resource {
                    value: f64::from_excel(value, argument)?,
                    drops: Arc::clone(&drops),
                    _owned: Box::new([0; 32]),
                })
            });
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
    fn miri_optional_numeric_collections_preserve_identity_and_values() {
        use crate::value::{Matrix, MatrixRef, OptionalExcelValue};
        for length in [1, 14, 15, 65] {
            let mut cells = (0..length)
                .map(|index| match index % 5 {
                    0 => XLOPER12::number(-0.0),
                    1 => XLOPER12::nil(),
                    2 => XLOPER12::missing(),
                    3 => XLOPER12::integer(7),
                    _ => XLOPER12::number(f64::from_bits(1)),
                })
                .collect::<Vec<_>>();
            let root = array_root(&mut cells);
            crate::call::with_excel_call_scope_and_state(&root, |raw, scope| {
                let raw = XlValueRef::from_array_cell(raw).unwrap();
                macro_rules! check {
                    ($ty:ty) => {{
                        let mut arguments =
                            ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                        let state = arguments.prepare::<$ty>(0, "values", raw).unwrap();
                        let identity = arguments.finish().unwrap();
                        let result = state.materialize().unwrap();
                        let mut materialized =
                            ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                        materialized.record_decoded(0, "values", &result).unwrap();
                        assert_eq!(materialized.finish().unwrap(), identity);
                        let mut eager = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                        eager.decode::<$ty>(0, "values", raw).unwrap();
                        assert_eq!(eager.finish().unwrap(), identity);
                        identity
                    }};
                }
                let optional = check!(Matrix<Option<f64>>);
                assert_eq!(optional, check!(MatrixRef<'_, Option<f64>>));
                check!(Matrix<OptionalExcelValue<f64>>);
                check!(Matrix<Option<OptionalExcelValue<f64>>>);
                check!(Matrix<Option<Option<f64>>>);
                check!(MatrixRef<'_, Option<Option<f64>>>);
            });
        }
        // Missing and blank share Option identity, but not OptionalExcelValue identity.
        fn digest(raw: XLOPER12, presence: bool) -> Option<[u8; 32]> {
            let mut cells = [raw];
            let root = array_root(&mut cells);
            crate::call::with_excel_call_scope_and_state(&root, |raw, scope| {
                let raw = XlValueRef::from_array_cell(raw).unwrap();
                let mut args = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                if presence {
                    drop(
                        args.prepare::<Matrix<OptionalExcelValue<f64>>>(0, "v", raw)
                            .unwrap(),
                    );
                } else {
                    drop(args.prepare::<Matrix<Option<f64>>>(0, "v", raw).unwrap());
                }
                args.finish().unwrap()
            })
        }
        assert_eq!(
            digest(XLOPER12::nil(), false),
            digest(XLOPER12::missing(), false)
        );
        assert_ne!(
            digest(XLOPER12::nil(), true),
            digest(XLOPER12::missing(), true)
        );
    }

    #[test]
    fn optional_builtin_collections_match_eager_conversion() {
        use crate::value::{ExcelError, ExcelSerialDate, Matrix, MatrixRef, OptionalExcelValue};
        macro_rules! check_builtin {
            ($ty:ty, $present:expr) => {{
                let mut cells = [XLOPER12::nil(), XLOPER12::missing(), $present];
                let root = array_root(&mut cells);
                crate::call::with_excel_call_scope_and_state(&root, |raw, scope| {
                    let raw = XlValueRef::from_array_cell(raw).unwrap();
                    macro_rules! check_collection {
                        ($collection:ty) => {{
                            let mut eager =
                                ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                            let expected = eager.decode::<$collection>(0, "v", raw).unwrap();
                            let mut args =
                                ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                            let prepared = args.prepare::<$collection>(0, "v", raw).unwrap();
                            assert_eq!(args.finish().unwrap(), eager.finish().unwrap());
                            assert_eq!(
                                prepared.materialize().unwrap().as_slice(),
                                expected.as_slice()
                            );
                        }};
                    }
                    check_collection!(Matrix<Option<$ty>>);
                    check_collection!(Matrix<OptionalExcelValue<$ty>>);
                    check_collection!(MatrixRef<'_, Option<$ty>>);
                });
            }};
        }
        check_builtin!(bool, XLOPER12::boolean(true));
        check_builtin!(i32, XLOPER12::integer(-7));
        check_builtin!(i64, XLOPER12::number(9_007_199_254_740_991.0));
        check_builtin!(ExcelSerialDate, XLOPER12::number(45_000.5));
        check_builtin!(ExcelError, XLOPER12::error(15));
    }

    #[test]
    fn optional_numeric_collection_errors_precede_materialization() {
        use crate::value::{Matrix, MatrixRef, OptionalExcelValue};
        for invalid in [
            XLOPER12::number(f64::NAN),
            XLOPER12::boolean(true),
            XLOPER12::error(15),
        ] {
            let mut cells = [XLOPER12::number(1.0), XLOPER12::nil(), invalid];
            let root = array_root(&mut cells);
            crate::call::with_excel_call_scope_and_state(&root, |raw, scope| {
                let raw = XlValueRef::from_array_cell(raw).unwrap();
                macro_rules! check {
                    ($ty:ty) => {{
                        let mut eager = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                        let expected = eager.decode::<$ty>(0, "values", raw).unwrap_err();
                        let mut args = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                        let error = match args.prepare::<$ty>(0, "values", raw) {
                            Ok(_) => panic!("invalid last cell accepted before lookup"),
                            Err(error) => error,
                        };
                        assert_eq!(format!("{error:?}"), format!("{expected:?}"));
                        assert!(args.finish().is_err());
                    }};
                }
                check!(Matrix<Option<f64>>);
                check!(Matrix<OptionalExcelValue<f64>>);
                check!(MatrixRef<'_, Option<f64>>);
            });
        }
    }

    #[test]
    fn optional_borrowed_preparation_preserves_retained_budget_precedence() {
        type LargePrepared = [u8; crate::value::MAX_ARRAY_BYTES];
        assert!(matches!(
            ExcelInputCells::wrapped_budget::<f64, LargePrepared, true>(2, "v"),
            Err(XllError::Input {
                reason: crate::error::InputError::TooLarge { .. },
                ..
            })
        ));
        assert!(ExcelInputCells::wrapped_budget::<f64, LargePrepared, false>(2, "v").is_ok());
        // Both paths still reject output size before cell validation.
        for error in [
            ExcelInputCells::wrapped_budget::<LargePrepared, f64, true>(2, "v")
                .err()
                .unwrap(),
            ExcelInputCells::wrapped_budget::<LargePrepared, f64, false>(2, "v")
                .err()
                .unwrap(),
        ] {
            assert!(matches!(
                error,
                XllError::Input {
                    reason: crate::error::InputError::TooLarge { .. },
                    ..
                }
            ));
        }
    }

    #[test]
    fn miri_static_materializer_preserves_custom_state_through_discard_and_argument_error() {
        static PREPARATIONS: AtomicUsize = AtomicUsize::new(0);
        static MATERIALIZATIONS: AtomicUsize = AtomicUsize::new(0);
        static DROPS: AtomicUsize = AtomicUsize::new(0);

        struct Retained {
            value: f64,
            _owned: Box<[u8; 16]>,
        }
        impl Drop for Retained {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::Relaxed);
            }
        }
        #[derive(Clone, Copy)]
        struct Observed(f64);
        impl<'call> FromExcel<'call> for Observed {
            fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
                f64::from_excel(value, argument).map(Self)
            }
        }
        impl ExcelInputIdentity for Observed {
            fn encode_input_identity(&self, identity: &mut InputIdentityEncoder) {
                identity.f64(self.0);
            }
        }
        impl<'call> PrepareExcel<'call> for Observed {
            type Prepared = Retained;
            fn prepare(
                value: XlValueRef<'call>,
                argument: &'static str,
                identity: &mut InputIdentityEncoder,
            ) -> XllResult<Retained> {
                let value = f64::from_excel(value, argument)?;
                identity.f64(value);
                PREPARATIONS.fetch_add(1, Ordering::Relaxed);
                Ok(Retained {
                    value,
                    _owned: Box::new([0; 16]),
                })
            }
            fn materialize(value: Retained) -> XllResult<Self> {
                MATERIALIZATIONS.fetch_add(1, Ordering::Relaxed);
                Ok(Self(value.value))
            }
        }
        fn counts() -> (usize, usize, usize) {
            (
                PREPARATIONS.load(Ordering::Relaxed),
                MATERIALIZATIONS.load(Ordering::Relaxed),
                DROPS.load(Ordering::Relaxed),
            )
        }

        let inputs = (
            XLOPER12::number(7.0),
            XLOPER12::error(crate::ExcelError::Value.code()),
        );
        crate::call::with_excel_call_scope_and_state(&inputs, |(raw, invalid), scope| {
            let value = XlValueRef::from_array_cell(raw).unwrap();
            let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let prepared = arguments.prepare::<Observed>(0, "first", value).unwrap();
            assert!(arguments.finish().unwrap().is_some());
            assert_eq!(counts(), (1, 0, 0));
            drop(prepared); // A warm hit never materializes custom state.
            assert_eq!(counts(), (1, 0, 1));

            let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let prepared = arguments.prepare::<Observed>(0, "first", value).unwrap();
            arguments.finish().unwrap();
            assert_eq!(prepared.materialize().unwrap().0, 7.0);
            assert_eq!(counts(), (2, 1, 2));

            let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 2);
            let first = arguments.prepare::<Observed>(0, "first", value).unwrap();
            assert!(
                arguments
                    .prepare::<f64>(1, "second", XlValueRef::from_array_cell(invalid).unwrap())
                    .is_err()
            );
            assert!(arguments.finish().is_err());
            drop(first);
            assert_eq!(counts(), (3, 1, 3));
        });

        let mut cells = [XLOPER12::number(7.0), XLOPER12::nil()];
        let root = array_root(&mut cells);
        for cold in [false, true] {
            let before = counts();
            crate::call::with_excel_call_scope_and_state(&root, |raw, scope| {
                let value = XlValueRef::from_array_cell(raw).unwrap();
                let mut arguments = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let prepared = arguments.prepare::<Option<crate::value::Matrix<crate::value::OptionalExcelValue<Observed>>>>(0, "values", value).unwrap();
                assert!(arguments.finish().unwrap().is_some());
                assert_eq!(counts(), (before.0 + 1, before.1, before.2));
                if cold {
                    let matrix = prepared.materialize().unwrap().unwrap();
                    assert!(matches!(
                        matrix.as_slice()[0],
                        crate::value::OptionalExcelValue::Value(Observed(7.0))
                    ));
                    assert!(matches!(
                        matrix.as_slice()[1],
                        crate::value::OptionalExcelValue::Blank
                    ));
                } else {
                    drop(prepared);
                }
                assert_eq!(
                    counts(),
                    (before.0 + 1, before.1 + usize::from(cold), before.2 + 1)
                );
            });
        }
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
            for materialize in [false, true] {
                let before = CONVERSIONS.load(Ordering::Relaxed);
                let mut args = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let prepared = args
                    .prepare::<crate::value::Matrix<Option<Observed>>>(0, "arg", value)
                    .unwrap();
                args.finish().unwrap();
                assert_eq!(CONVERSIONS.load(Ordering::Relaxed), before + 1);
                if materialize {
                    assert_eq!(
                        prepared.materialize().unwrap().as_slice()[0]
                            .as_ref()
                            .unwrap()
                            .0,
                        42.0
                    );
                } else {
                    drop(prepared);
                }
                assert_eq!(CONVERSIONS.load(Ordering::Relaxed), before + 1);
                let mut args = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
                let prepared = args
                    .prepare::<crate::value::Matrix<crate::value::OptionalExcelValue<Observed>>>(
                        0, "arg", value,
                    )
                    .unwrap();
                args.finish().unwrap();
                assert_eq!(CONVERSIONS.load(Ordering::Relaxed), before + 2);
                if materialize {
                    drop(prepared.materialize().unwrap());
                } else {
                    drop(prepared);
                }
                assert_eq!(CONVERSIONS.load(Ordering::Relaxed), before + 2);
            }
        });
    }
}
