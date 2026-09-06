//! Movable Excel string arguments whose UTF-16 allocation remains published.

use crate::XllResult;
use std::ptr::NonNull;
use xlfn_kernel::published_owner::PublishedOwner;
use xlfn_sys::{XLOPER12, XLOPER12Value, XLTYPE_STR};

pub(super) struct CountedString {
    units: PublishedOwner<[u16]>,
    oper: XLOPER12,
}

impl CountedString {
    pub(super) fn new(value: &str) -> XllResult<Self> {
        let units =
            crate::utf16::encode_counted(value, "RTD topic", crate::utf16::EXCEL_STRING_LIMIT)?;
        let units = PublishedOwner::from_box(units.into_boxed_slice());
        let oper = XLOPER12 {
            value: XLOPER12Value {
                string: units.as_ptr().cast::<u16>(),
            },
            xltype: XLTYPE_STR,
        };
        Ok(Self { units, oper })
    }

    pub(super) fn pointer(&mut self) -> NonNull<XLOPER12> {
        let _keep_alive = &self.units;
        NonNull::from_mut(&mut self.oper)
    }
}

#[cfg(test)]
#[allow(
    unsafe_code,
    reason = "Miri regression reads an owned Excel string argument"
)]
mod tests {
    use super::*;

    #[test]
    fn miri_counted_string_remains_readable_after_owner_moves() {
        for expected in ["", "日本語😀", "RTD.Server.1"] {
            let value = CountedString::new(expected).unwrap();
            let mut owners = vec![value];
            owners.reserve(64);
            let mut value = Box::new(owners.pop().unwrap());
            let pointer = value.pointer();
            // SAFETY: the argument and its retained UTF-16 allocation are live
            // until the decoded string has been copied into Rust ownership.
            let view = unsafe { crate::value::XlValueRef::from_raw(pointer.as_ptr()) }.unwrap();
            assert_eq!(view.as_str().unwrap().to_string().unwrap(), expected);
        }
    }
}
