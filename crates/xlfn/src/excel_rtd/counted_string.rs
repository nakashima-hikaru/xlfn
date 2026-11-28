//! Owned counted UTF-16 and call-scoped views of RTD input arguments.

use crate::XllResult;
#[cfg(any(test, feature = "handles"))]
use crate::error::InputError;
use std::ptr::NonNull;
use xlfn_kernel::published_owner::PublishedOwner;
use xlfn_sys::{XLOPER12, XLOPER12Value, XLTYPE_STR};

pub(super) struct CountedString(PublishedOwner<[u16]>);

impl CountedString {
    pub(super) fn new(value: &str) -> XllResult<Self> {
        let units =
            crate::utf16::encode_counted(value, "RTD topic", crate::utf16::EXCEL_STRING_LIMIT)?;
        Ok(Self(PublishedOwner::from_box(units.into_boxed_slice())))
    }

    #[cfg(any(test, feature = "handles"))]
    pub(super) fn concat([prefix, suffix]: [&str; 2]) -> XllResult<Self> {
        use crate::utf16::{EXCEL_STRING_LIMIT, checked_utf16_len};

        // Each count fits in its input slice length. Two Rust slices cannot
        // exceed usize::MAX-1 bytes in total.
        let length = checked_utf16_len(prefix, "RTD topic", usize::MAX)?
            + checked_utf16_len(suffix, "RTD topic", usize::MAX)?;
        if length > EXCEL_STRING_LIMIT {
            return Err(crate::XllError::input(
                "RTD topic",
                InputError::TooLarge {
                    limit: EXCEL_STRING_LIMIT,
                    actual: length,
                },
            ));
        }
        let mut units = Vec::with_capacity(length + 1);
        units.push(length as u16);
        units.extend(prefix.encode_utf16().chain(suffix.encode_utf16()));
        Ok(Self(PublishedOwner::from_box(units.into_boxed_slice())))
    }

    fn borrow(&self) -> BorrowedCountedString<'_> {
        BorrowedCountedString::new(self.0.as_ref())
    }
}

impl AsRef<[u16]> for CountedString {
    fn as_ref(&self) -> &[u16] {
        self.0.as_ref()
    }
}

/// A fixed-width ASCII subscription transport encoded directly into an inline
/// Excel counted string. The XLOPER12 itself is assembled only when borrowed,
/// so moving this value cannot leave an interior pointer dangling.
#[cfg(any(test, all(target_os = "windows", feature = "rtd")))]
pub(super) struct InlineCountedTransport {
    units: [u16; 44],
}

#[cfg(any(test, all(target_os = "windows", feature = "rtd")))]
impl InlineCountedTransport {
    pub(super) fn new(bytes: &[u8; 43]) -> Self {
        debug_assert!(bytes.iter().all(u8::is_ascii));
        let mut units = [0_u16; 44];
        units[0] = 43;
        for (&byte, unit) in bytes.iter().zip(&mut units[1..]) {
            *unit = u16::from(byte);
        }
        Self { units }
    }
}

#[cfg(any(test, all(target_os = "windows", feature = "rtd")))]
impl AsRef<[u16]> for InlineCountedTransport {
    fn as_ref(&self) -> &[u16] {
        &self.units
    }
}

struct BorrowedCountedString<'a> {
    _units: std::marker::PhantomData<&'a [u16]>,
    oper: XLOPER12,
}

impl<'a> BorrowedCountedString<'a> {
    fn new(units: &'a [u16]) -> Self {
        Self {
            _units: std::marker::PhantomData,
            oper: XLOPER12 {
                value: XLOPER12Value {
                    // The RTD input is read-only; Excel exposes a mutable
                    // pointer field even for read-only input arguments.
                    string: units.as_ptr().cast_mut(),
                },
                xltype: XLTYPE_STR,
            },
        }
    }

    fn pointer(&mut self) -> NonNull<XLOPER12> {
        NonNull::from_mut(&mut self.oper)
    }
}

fn with_rtd_arguments<T>(
    prog_id: &CountedString,
    topic: &impl AsRef<[u16]>,
    invoke: impl FnOnce(&[NonNull<XLOPER12>]) -> T,
) -> T {
    let mut prog_id = prog_id.borrow();
    let mut topic = BorrowedCountedString::new(topic.as_ref());
    let mut server_name = XLOPER12::missing();
    // The argument pointers and their read-only buffers stay live through
    // invocation and result release. Keep the raw pointers in this adapter.
    invoke(&[
        prog_id.pointer(),
        NonNull::from_mut(&mut server_name),
        topic.pointer(),
    ])
}

#[cfg(any(test, windows))]
pub(super) fn invoke_rtd<T>(
    host: crate::host_api::ExcelHost<'_>,
    prog_id: &CountedString,
    topic: &impl AsRef<[u16]>,
    decode: impl FnOnce(&mut crate::callback_value::ExcelCallbackValue<'_>) -> XllResult<T>,
) -> XllResult<T> {
    with_rtd_arguments(prog_id, topic, |arguments| {
        host.invoke(
            xlfn_sys::XLF_RTD,
            crate::error::ExcelApiFunction::Rtd,
            arguments,
            decode,
        )
    })
}

#[cfg(any(test, feature = "handles"))]
pub(super) fn handle_matches(value: crate::value::XlValueRef<'_>, token: &str) -> XllResult<bool> {
    const ARGUMENT: &str = "RTD handle";
    let units = value.utf16(ARGUMENT)?;
    if units.iter().copied().eq(token.encode_utf16()) {
        // An exact match with encoded Rust text proves valid surrogates.
        return Ok(true);
    }
    // An early mismatch may hide malformed UTF-16 later in the input.
    // Preserve a malformed decode as distinct from a wrong token.
    if char::decode_utf16(units.iter().copied()).any(|unit| unit.is_err()) {
        return Err(crate::XllError::input(ARGUMENT, InputError::InvalidUtf16));
    }
    Ok(false)
}

#[cfg(all(feature = "bench-internals", feature = "handles", feature = "rtd"))]
pub use benchmark::RtdObservationArgumentsBenchmark;

#[cfg(all(feature = "bench-internals", feature = "handles", feature = "rtd"))]
mod benchmark {
    use super::*;
    use crate::value::{FromExcel, XlValueRef};
    use std::hint::black_box;
    use triomphe::Arc;

    const PROG_ID: &str = "XlFnRtd_00112233445566778899AABBCCDDEEFF";
    const LIFETIME_KEY: &str = "revision:sheet:row:column:QWERTYUIOPASDFGHJKLZXCVBNM";
    const TOKEN: &str = "QWERTYUIOPASDFGHJKLZXCVBNM1234567890";

    /// Measures reused RTD handle observation arguments and callback decoding.
    /// The mock result lives through every iteration. COM and Excel are excluded.
    pub struct RtdObservationArgumentsBenchmark {
        prog_id: String,
        encoded_prog_id: Arc<CountedString>,
        returned_token: CountedString,
    }

    impl Default for RtdObservationArgumentsBenchmark {
        fn default() -> Self {
            Self {
                prog_id: PROG_ID.to_owned(),
                encoded_prog_id: Arc::new(CountedString::new(PROG_ID).unwrap()),
                returned_token: CountedString::new(TOKEN).unwrap(),
            }
        }
    }

    impl RtdObservationArgumentsBenchmark {
        /// Replays the previous argument encoding and owned token decoding.
        pub fn owned(&self) {
            let prog_id = black_box(self.prog_id.clone());
            let encoded = CountedString::new(&prog_id).unwrap();
            let topic = CountedString::new(&format!("handle:{}", black_box(LIFETIME_KEY))).unwrap();
            with_rtd_arguments(&encoded, &topic, |arguments| {
                black_box(arguments);
                self.read_result(|value| String::from_excel(value, "RTD handle").unwrap() == TOKEN);
            });
        }

        /// Runs the borrowed argument and token comparison path.
        pub fn borrowed(&self) {
            let encoded = black_box(Arc::clone(&self.encoded_prog_id));
            let topic = CountedString::concat(["handle:", black_box(LIFETIME_KEY)]).unwrap();
            with_rtd_arguments(&encoded, &topic, |arguments| {
                black_box(arguments);
                self.read_result(|value| handle_matches(value, TOKEN).unwrap());
            });
        }

        #[allow(unsafe_code, reason = "fixture owns a live counted Excel result")]
        fn read_result(&self, decode: impl FnOnce(XlValueRef<'_>) -> bool) {
            let mut result = self.returned_token.borrow();
            // SAFETY: the fixture retains the argument and its counted buffer
            // throughout synchronous decoding.
            let result = unsafe { XlValueRef::from_raw(result.pointer().as_ptr()) }.unwrap();
            assert!(black_box(decode(result)));
        }
    }
}

#[cfg(test)]
#[allow(
    unsafe_code,
    reason = "Miri regression reads an owned Excel string argument"
)]
mod tests {
    use super::*;
    use crate::error::{ExcelApiFailure, ExcelApiFunction};
    use crate::host_api::ExcelHost;
    use crate::host_callback::HostCallbackSession;
    use crate::registration::host::test_support::{
        CallbackScript, Reply, read_registration_arguments,
    };
    use crate::return_abi::ExcelCallbackStatus;
    use xlfn_sys::{XLF_RTD, XLRET_ABORT, XLTYPE_MISSING};

    #[test]
    fn miri_inline_transport_uses_no_heap_and_survives_moves() {
        let text = *b"stream:v1:0123456789abcdef:fedcba9876543210";
        let transport = InlineCountedTransport::new(&text);
        let mut owners = vec![transport];
        owners.reserve(64);
        let transport = owners.pop().unwrap();
        let prog_id = CountedString::new("XlFnRtd_fixture").unwrap();
        with_rtd_arguments(&prog_id, &transport, |arguments| {
            // SAFETY: all argument owners and the borrowed XLOPER12 are live
            // for the entire closure; no pointer escapes this invocation.
            let topic =
                unsafe { crate::value::XlValueRef::from_raw(arguments[2].as_ptr()) }.unwrap();
            assert!(
                topic
                    .as_str()
                    .unwrap()
                    .as_utf16()
                    .iter()
                    .copied()
                    .eq(text.map(u16::from))
            );
        });
        assert_eq!(transport.units[0], 43);
    }

    #[test]
    fn miri_counted_string_remains_readable_after_owner_moves() {
        for expected in ["", "日本語😀", "RTD.Server.1"] {
            let value = CountedString::new(expected).unwrap();
            let mut owners = vec![value];
            owners.reserve(64);
            let value = Box::new(owners.pop().unwrap());
            let mut argument = value.borrow();
            let pointer = argument.pointer();
            // SAFETY: the argument and its retained UTF-16 allocation are live
            // until the decoded string has been copied into Rust ownership.
            let view = unsafe { crate::value::XlValueRef::from_raw(pointer.as_ptr()) }.unwrap();
            assert_eq!(view.as_str().unwrap().try_to_string().unwrap(), expected);
        }
    }

    #[test]
    fn miri_rtd_topic_concat_encodes_the_checked_total_length() {
        let text = CountedString::concat(["handle:", "日本語😀"]).unwrap();
        assert_eq!(text.0[0], 12);
        assert!(
            text.0[1..]
                .iter()
                .copied()
                .eq("handle:日本語😀".encode_utf16())
        );
        let long_tail = "a".repeat(crate::utf16::EXCEL_STRING_LIMIT - 6);
        assert!(matches!((CountedString::concat(["handle:", &long_tail])).as_ref().map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Input {
                reason: InputError::TooLarge {
                    limit: 32_767,
                    actual: 32_768
                },
                ..
            })
        ));
    }

    #[test]
    fn miri_handle_comparison_validates_unmatched_suffixes() {
        let matching = CountedString::new("日本語😀").unwrap();
        let mut argument = matching.borrow();
        // SAFETY: argument and its counted allocation stay live for the comparison.
        let value =
            unsafe { crate::value::XlValueRef::from_raw(argument.pointer().as_ptr()) }.unwrap();
        assert!(handle_matches(value, "日本語😀").unwrap());
        assert!(!handle_matches(value, "日本語😃").unwrap());
        assert!(!handle_matches(value, "日本語").unwrap());

        let mut malformed = [2, u16::from(b'X'), 0xd800];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                string: malformed.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };
        // SAFETY: raw and its initialized counted buffer remain live.
        let value = unsafe { crate::value::XlValueRef::from_raw(&raw mut raw) }.unwrap();
        assert!(matches!((handle_matches(value, "Y")).as_ref().map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Input {
                argument: "RTD handle",
                reason: InputError::InvalidUtf16
            })
        ));
    }

    #[test]
    fn rtd_arguments_borrow_the_same_buffer_and_cleanup_precedes_token_rejection() {
        use crate::registration::host::test_support::RegisterArgument;

        let prog_id = CountedString::new("XlFnRtd_fixture").unwrap();
        let topic = CountedString::concat(["handle:", "日本😀"]).unwrap();
        let mut text = [1, u16::from(b'X')];
        let result = XLOPER12 {
            value: XLOPER12Value {
                string: text.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };
        let script = CallbackScript::install([
            Reply::success(XLF_RTD, result),
            Reply::success(XLF_RTD, result).release_status(XLRET_ABORT),
        ]);
        for _ in 0..2 {
            let expected_pointer = prog_id.0.as_ptr().cast::<u16>().cast_const();
            script.on_arguments(XLF_RTD, move |arguments| {
                // SAFETY: the callback supplies three live RTD arguments.
                let observed = unsafe { read_registration_arguments(arguments) };
                assert_eq!(
                    observed,
                    [
                        RegisterArgument::Text("XlFnRtd_fixture".to_owned()),
                        RegisterArgument::UnexpectedType(XLTYPE_MISSING),
                        RegisterArgument::Text("handle:日本😀".to_owned()),
                    ]
                );
                // SAFETY: the callback supplies a live first argument.
                let first = unsafe { &*arguments[0] };
                // SAFETY: the verified string tag selects this member.
                let actual_pointer = unsafe { first.value.string };
                assert_eq!(actual_pointer.cast_const(), expected_pointer);
            });
        }
        let session = HostCallbackSession::new();
        let host = ExcelHost::new(&session);
        let observe = || {
            invoke_rtd(host, &prog_id, &topic, |result| {
                handle_matches(result.borrow()?, "Y")
            })
        };
        assert!(matches!(observe(), Ok(false)));
        assert!(matches!((observe()).as_ref().map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::ExcelApi {
                function: ExcelApiFunction::Free,
                failure: ExcelApiFailure::Status(ExcelCallbackStatus::Abort),
                ..
            })
        ));
        assert!(matches!((observe()).as_ref().map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::ExcelApi {
                function: ExcelApiFunction::Rtd,
                failure: ExcelApiFailure::Suppressed(ExcelCallbackStatus::Abort),
                ..
            })
        ));
        script.assert_calls(&[XLF_RTD, XLF_RTD]);
    }
}
