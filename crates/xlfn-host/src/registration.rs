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
        encode: &impl Fn(&str) -> XllResult<CountedString>,
    ) -> XllResult<()> {
        if !std::ptr::eq(self.text, text) && self.text != text {
            self.units = if text.is_empty() {
                SmallVec::new()
            } else {
                encode(text)?
            };
            self.text = text;
        }
        Ok(())
    }

    fn borrow(&self) -> BorrowedRegistrationString<'_> {
        let units = if self.text.is_empty() {
            &EMPTY_COUNTED
        } else {
            self.units.as_slice()
        };
        BorrowedRegistrationString::new(units)
    }
}

/// Scratch storage owned by one registration batch. A nested batch receives
/// its own cache, and an active RegistrationScratch borrows its cached strings
/// until the synchronous callback and result release both return.
#[derive(Default)]
pub struct RegistrationTextCache {
    category: LastRegistrationText,
    help_topic: LastRegistrationText,
    function_help: LastRegistrationText,
    argument_help: SmallVec<[LastRegistrationText; 4]>,
}

impl RegistrationTextCache {
    fn prepare(
        &mut self,
        category: &'static str,
        help_topic: &'static str,
        function_help: &'static str,
        argument_help: impl ExactSizeIterator<Item = &'static str>,
        encode: &impl Fn(&str) -> XllResult<CountedString>,
    ) -> XllResult<()> {
        self.category.prepare(category, encode)?;
        self.help_topic.prepare(help_topic, encode)?;
        self.function_help.prepare(function_help, encode)?;
        self.argument_help
            .resize_with(argument_help.len(), LastRegistrationText::default);
        for (cached, description) in self.argument_help.iter_mut().zip(argument_help) {
            cached.prepare(description, encode)?;
        }
        Ok(())
    }
}

struct BorrowedRegistrationString<'call> {
    units: &'call [u16],
    oper: XLOPER12,
}

impl<'call> BorrowedRegistrationString<'call> {
    fn new(units: &'call [u16]) -> Self {
        Self {
            units,
            oper: XLOPER12::nil(),
        }
    }

    fn pointer(&mut self) -> NonNull<XLOPER12> {
        self.oper = XLOPER12 {
            value: XLOPER12Value {
                // The xlfRegister input is read-only despite the ABI's *mut.
                string: self.units.as_ptr().cast_mut(),
            },
            xltype: XLTYPE_STR,
        };
        NonNull::from_mut(&mut self.oper)
    }
}

/// Owns the scratch operands for one synchronous xlfRegister call and borrows
/// retained UTF-16 for repeated metadata. Pointers are formed only in invoke.
pub struct RegistrationScratch<'call> {
    module: BorrowedRegistrationString<'call>,
    procedure: TemporaryString,
    type_text: TemporaryString,
    function_text: TemporaryString,
    arguments: TemporaryString,
    macro_type: XLOPER12,
    category: BorrowedRegistrationString<'call>,
    shortcut: BorrowedRegistrationString<'static>,
    help_topic: BorrowedRegistrationString<'call>,
    function_help: BorrowedRegistrationString<'call>,
    argument_help: SmallVec<[BorrowedRegistrationString<'call>; 4]>,
}

impl<'call> RegistrationScratch<'call> {
    pub fn new(
        module: &'call CountedModuleName,
        registration: RegistrationArgs<
            impl Iterator<Item = u16>,
            impl Iterator<Item = u16>,
            impl ExactSizeIterator<Item = &'static str>,
        >,
        text: &'call mut RegistrationTextCache,
        encode: impl Fn(&str) -> XllResult<CountedString>,
    ) -> XllResult<Self> {
        text.prepare(
            registration.category,
            registration.help_topic,
            registration.function_help,
            registration.argument_help,
            &encode,
        )?;
        let help_count = text.argument_help.len();
        let mut argument_help = SmallVec::with_capacity(help_count + usize::from(help_count != 0));
        if help_count != 0 {
            argument_help.extend(text.argument_help.iter().map(LastRegistrationText::borrow));
            argument_help.push(BorrowedRegistrationString::new(&EMPTY_COUNTED));
        }
        Ok(Self {
            module: BorrowedRegistrationString::new(&module.units),
            procedure: TemporaryString::from_counted(encode(registration.export_name)?),
            type_text: TemporaryString::from_validated_units(
                registration.signature.0,
                registration.signature.1,
            ),
            function_text: TemporaryString::from_counted(encode(registration.excel_name)?),
            arguments: TemporaryString::from_validated_units(
                registration.argument_names.0,
                registration.argument_names.1,
            ),
            macro_type: XLOPER12::number(registration.macro_type),
            category: text.category.borrow(),
            shortcut: BorrowedRegistrationString::new(&EMPTY_COUNTED),
            help_topic: text.help_topic.borrow(),
            function_help: text.function_help.borrow(),
            argument_help,
        })
    }

    /// Borrows immobile operands through the caller's synchronous callback
    /// and result-release closure. This does not authorize an Excel call.
    pub fn with_arguments<T>(mut self, invoke: impl FnOnce(&[NonNull<XLOPER12>]) -> T) -> T {
        let mut pointers: SmallVec<[NonNull<XLOPER12>; 16]> = smallvec::smallvec![
            self.module.pointer(),
            self.procedure.pointer(),
            self.type_text.pointer(),
            self.function_text.pointer(),
            self.arguments.pointer(),
            NonNull::from_mut(&mut self.macro_type),
            self.category.pointer(),
            self.shortcut.pointer(),
            self.help_topic.pointer(),
            self.function_help.pointer(),
        ];
        pointers.extend(
            self.argument_help
                .iter_mut()
                .map(BorrowedRegistrationString::pointer),
        );

        invoke(&pointers)
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

    fn from_validated_units(length: usize, units: impl Iterator<Item = u16>) -> Self {
        if length > EXCEL_STRING_LIMIT {
            xlfn_kernel::invariant::fail_stop();
        }
        let mut storage = SmallVec::with_capacity(length + 1);
        storage.push(length as u16);
        storage.extend(units);
        if storage.len() != length + 1 {
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
    fn registration_metadata_reuses_equal_counted_text_and_replaces_changed_text() {
        static LONG: [u8; 96] = [b'x'; 96];
        static ALTERNATE: [u8; 96] = [b'x'; 96];
        let long = std::str::from_utf8(&LONG).unwrap();
        let alternate = std::str::from_utf8(&ALTERNATE).unwrap();
        assert!(!std::ptr::eq(long, alternate));
        assert_eq!(long, alternate);

        let encode = |value: &str| {
            let count = value.encode_utf16().count() as u16;
            Ok(std::iter::once(count).chain(value.encode_utf16()).collect())
        };
        let mut text = LastRegistrationText::default();
        assert!(std::ptr::eq(text.borrow().units, &EMPTY_COUNTED));
        text.prepare(long, &encode).unwrap();
        let counted = text.borrow().units.as_ptr();
        text.prepare(alternate, &encode).unwrap();
        assert_eq!(text.borrow().units.as_ptr(), counted);

        text.prepare("価格💡", &encode).unwrap();
        let expected: Vec<_> = std::iter::once(4).chain("価格💡".encode_utf16()).collect();
        assert_eq!(text.borrow().units, expected);
        text.prepare("", &encode).unwrap();
        assert!(std::ptr::eq(text.borrow().units, &EMPTY_COUNTED));
    }
}
