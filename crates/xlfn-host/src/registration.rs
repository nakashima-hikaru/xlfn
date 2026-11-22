//! Owned native xlfRegister operands and batch-local scratch. This adapter
//! does not grant host admission, choose a callback session, or own journals.

use smallvec::SmallVec;
use std::ptr::NonNull;
use xlfn_common::EXCEL_STRING_LIMIT;
use xlfn_engine::error::{InputError, XllError, XllResult};
use xlfn_sys::{XLOPER12, XLOPER12Value, XLTYPE_STR};

pub type CountedString = SmallVec<[u16; 64]>;

pub struct CountedModuleName {
    units: Box<[u16]>,
}

impl CountedModuleName {
    /// Borrows the immutable length-prefixed UTF-16 registration operand.
    pub fn as_counted(&self) -> &[u16] {
        &self.units
    }

    pub fn from_units(units: &[u16]) -> XllResult<Self> {
        if units.len() > EXCEL_STRING_LIMIT {
            return Err(XllError::input(
                "registration",
                InputError::TooLarge {
                    limit: EXCEL_STRING_LIMIT,
                    actual: units.len(),
                },
            ));
        }
        let mut counted = Vec::with_capacity(units.len() + 1);
        counted.push(units.len() as u16);
        counted.extend_from_slice(units);
        Ok(Self {
            units: counted.into_boxed_slice(),
        })
    }
}

static EMPTY_COUNTED: [u16; 1] = [0];

/// The literal, if any, used by the preceding registration in this ABI slot.
/// Each slot retains at most one encoding and skips the UTF-16 conversion
/// when adjacent registrations supply the same text.
#[derive(Default)]
struct LastRegistrationText {
    text: &'static str,
    units: SmallVec<[u16; 64]>,
}

impl LastRegistrationText {
    fn prepare(
        &mut self,
        text: &'static str,
        encode: &impl Fn(&mut CountedString, &str) -> XllResult<()>,
    ) -> XllResult<()> {
        if !std::ptr::eq(self.text, text) && self.text != text {
            if text.is_empty() {
                self.units.clear();
            } else {
                encode(&mut self.units, text)?;
            }
            self.text = text;
        }
        Ok(())
    }

    fn units(&self) -> &[u16] {
        if self.text.is_empty() {
            &EMPTY_COUNTED
        } else {
            self.units.as_slice()
        }
    }
}

fn store_validated_units(
    storage: &mut SmallVec<[u16; 64]>,
    length: usize,
    units: impl Iterator<Item = u16>,
) {
    if length > EXCEL_STRING_LIMIT {
        xlfn_kernel::invariant::fail_stop();
    }
    storage.clear();
    storage.reserve(length + 1);
    storage.push(length as u16);
    storage.extend(units);
    if storage.len() != length + 1 {
        xlfn_kernel::invariant::fail_stop();
    }
}

fn string_operand(units: &[u16]) -> XLOPER12 {
    debug_assert_eq!(units.len(), usize::from(units[0]) + 1);
    XLOPER12 {
        value: XLOPER12Value {
            // The xlfRegister input is read-only despite the ABI's *mut.
            string: units.as_ptr().cast_mut(),
        },
        xltype: XLTYPE_STR,
    }
}

/// Storage owned by one registration batch, never shared with a nested one.
/// UTF-16, operand and pointer allocations survive successive callbacks.
/// Form pointers only after their respective backing stores stop growing.
#[derive(Default)]
pub struct RegistrationScratch {
    procedure: SmallVec<[u16; 64]>,
    signature: SmallVec<[u16; 64]>,
    function: SmallVec<[u16; 64]>,
    arguments: SmallVec<[u16; 64]>,
    category: LastRegistrationText,
    help_topic: LastRegistrationText,
    function_help: LastRegistrationText,
    argument_help: SmallVec<[LastRegistrationText; 4]>,
    frame: RegistrationFrame,
}

impl RegistrationScratch {
    pub fn prepare(
        &mut self,
        registration: RegistrationArgs<
            impl Iterator<Item = u16>,
            impl Iterator<Item = u16>,
            impl ExactSizeIterator<Item = &'static str>,
        >,
        encode: impl Fn(&mut CountedString, &str) -> XllResult<()>,
    ) -> XllResult<PreparedRegistration<'_>> {
        encode(&mut self.procedure, registration.export_name)?;
        store_validated_units(
            &mut self.signature,
            registration.signature.0,
            registration.signature.1,
        );
        encode(&mut self.function, registration.excel_name)?;
        store_validated_units(
            &mut self.arguments,
            registration.argument_names.0,
            registration.argument_names.1,
        );
        self.category.prepare(registration.category, &encode)?;
        self.help_topic.prepare(registration.help_topic, &encode)?;
        self.function_help
            .prepare(registration.function_help, &encode)?;
        let help_count = registration.argument_help.len();
        if self.argument_help.len() < help_count {
            self.argument_help
                .resize_with(help_count, LastRegistrationText::default);
        }
        for (cached, description) in self
            .argument_help
            .iter_mut()
            .zip(registration.argument_help)
        {
            cached.prepare(description, &encode)?;
        }
        Ok(PreparedRegistration {
            scratch: self,
            help_count,
            macro_type: registration.macro_type,
        })
    }
}

/// Exclusive loan of the prepared batch scratch until callback and release end.
pub struct PreparedRegistration<'batch> {
    scratch: &'batch mut RegistrationScratch,
    help_count: usize,
    macro_type: f64,
}

impl PreparedRegistration<'_> {
    pub fn with_arguments<T>(
        self,
        module: &CountedModuleName,
        invoke: impl FnOnce(&mut [*mut XLOPER12]) -> T,
    ) -> T {
        let scratch = self.scratch;
        let argument_help = &scratch.argument_help[..self.help_count];
        let call = RegistrationCall(&mut scratch.frame);
        call.0.operands.extend([
            string_operand(&module.units),
            string_operand(&scratch.procedure),
            string_operand(&scratch.signature),
            string_operand(&scratch.function),
            string_operand(&scratch.arguments),
            XLOPER12::number(self.macro_type),
            string_operand(scratch.category.units()),
            string_operand(&EMPTY_COUNTED),
            string_operand(scratch.help_topic.units()),
            string_operand(scratch.function_help.units()),
        ]);
        call.0.operands.extend(
            argument_help
                .iter()
                .map(|help| string_operand(help.units())),
        );
        if !argument_help.is_empty() {
            call.0.operands.push(string_operand(&EMPTY_COUNTED));
        }
        call.0
            .pointers
            .extend(call.0.operands.iter_mut().map(std::ptr::from_mut));
        invoke(&mut call.0.pointers)
    }
}

#[derive(Default)]
struct RegistrationFrame {
    operands: SmallVec<[XLOPER12; 16]>,
    pointers: SmallVec<[*mut XLOPER12; 16]>,
}

/// Clears the frame on ordinary return and unwind without releasing capacity.
/// Its exclusive loan keeps the freshly formed pointers private until the
/// registration callback and its result release have both finished.
struct RegistrationCall<'frame>(&'frame mut RegistrationFrame);

impl Drop for RegistrationCall<'_> {
    fn drop(&mut self) {
        self.0.pointers.clear();
        self.0.operands.clear();
    }
}

/// Raw registration text operands; transaction policy stays in the facade.
pub struct RegistrationArgs<Signature, Arguments, Help> {
    pub export_name: &'static str,
    pub excel_name: &'static str,
    pub signature: (usize, Signature),
    pub argument_names: (usize, Arguments),
    pub macro_type: f64,
    pub category: &'static str,
    pub help_topic: &'static str,
    pub function_help: &'static str,
    pub argument_help: Help,
}

pub struct TemporaryString {
    storage: SmallVec<[u16; 64]>,
    oper: XLOPER12,
}

impl TemporaryString {
    /// Takes an encoded counted string. The prefix must match its length.
    pub fn from_counted(storage: CountedString) -> Self {
        if storage.first().is_none_or(|length| {
            usize::from(*length) > EXCEL_STRING_LIMIT || usize::from(*length) + 1 != storage.len()
        }) {
            xlfn_kernel::invariant::fail_stop();
        }
        Self {
            storage,
            oper: XLOPER12::nil(),
        }
    }

    pub fn pointer(&mut self) -> NonNull<XLOPER12> {
        debug_assert_eq!(self.storage.len(), self.storage[0] as usize + 1);
        self.oper = XLOPER12 {
            value: XLOPER12Value {
                string: self.storage.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };
        NonNull::from_mut(&mut self.oper)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_frame_reuses_spilled_allocations_after_overwrite_and_shorter_calls() {
        let encode = |units: &mut CountedString, text: &str| {
            units.clear();
            units.push(text.encode_utf16().count() as u16);
            units.extend(text.encode_utf16());
            Ok(())
        };
        let help = ["A long help string whose encoding spills out of the inline buffer to prove retained allocation.";
            6];
        let module = CountedModuleName::from_units(&[65]).unwrap();
        let mut scratch = RegistrationScratch::default();
        let mut spilled = None;
        for count in [0, 5, 6, 1, 0, 6] {
            let prepared = scratch
                .prepare(
                    RegistrationArgs {
                        export_name: "entry",
                        excel_name: "ENTRY",
                        signature: (1, [66].into_iter()),
                        argument_names: (0, std::iter::empty()),
                        macro_type: 1.0,
                        category: "",
                        help_topic: "",
                        function_help: "",
                        argument_help: help[..count].iter().copied(),
                    },
                    encode,
                )
                .unwrap();
            prepared.with_arguments(&module, |pointers| {
                assert_eq!(pointers.len(), 10 + count + usize::from(count != 0));
                assert!(pointers.iter().all(|pointer| !pointer.is_null()));
                pointers.fill(std::ptr::null_mut());
            });
            assert!(scratch.frame.operands.is_empty());
            assert!(scratch.frame.pointers.is_empty());
            if count <= 5 && spilled.is_none() {
                assert!(!scratch.frame.operands.spilled());
                assert!(!scratch.frame.pointers.spilled());
            }
            if count == 6 {
                assert!(scratch.frame.operands.spilled());
                assert!(scratch.frame.pointers.spilled());
                assert!(scratch.argument_help[1].units.spilled());
                let retained = (
                    scratch.frame.operands.as_ptr(),
                    scratch.frame.pointers.as_ptr(),
                    scratch.argument_help[1].units.as_ptr(),
                );
                if let Some(previous) = spilled {
                    assert_eq!(retained, previous);
                }
                spilled = Some(retained);
            }
        }
    }

    #[test]
    fn registration_metadata_reuses_equal_counted_text_and_replaces_changed_text() {
        static LONG: [u8; 96] = [b'x'; 96];
        static ALTERNATE: [u8; 96] = [b'x'; 96];
        let long = std::str::from_utf8(&LONG).unwrap();
        let alternate = std::str::from_utf8(&ALTERNATE).unwrap();
        assert!(!std::ptr::eq(long, alternate));
        assert_eq!(long, alternate);

        let encode = |units: &mut CountedString, text: &str| {
            units.clear();
            units.push(text.encode_utf16().count() as u16);
            units.extend(text.encode_utf16());
            Ok(())
        };
        let mut text = LastRegistrationText::default();
        assert!(std::ptr::eq(text.units(), &EMPTY_COUNTED));
        text.prepare(long, &encode).unwrap();
        let counted = text.units().as_ptr();
        text.prepare(alternate, &encode).unwrap();
        assert_eq!(text.units().as_ptr(), counted);

        text.prepare("価格💡", &encode).unwrap();
        assert_eq!(text.units.as_ptr(), counted);
        let expected: Vec<_> = std::iter::once(4).chain("価格💡".encode_utf16()).collect();
        assert_eq!(text.units(), expected);
        text.prepare("", &encode).unwrap();
        assert!(std::ptr::eq(text.units(), &EMPTY_COUNTED));
        assert_eq!(text.units.as_ptr(), counted);
        text.prepare(long, &encode).unwrap();
        assert_eq!(text.units.as_ptr(), counted);
    }
}
