//! Host-module discovery and Excel callback boundary helpers.

use crate::XllResult;
#[cfg(not(target_os = "windows"))]
use crate::error::{InputError, XllError};
use crate::value::{CallContext, ExcelParameter, InputMode, XlValueRef};
use std::path::PathBuf;

pub(super) struct ModuleName {
    pub(super) path: PathBuf,
    pub(super) units: Vec<u16>,
}

impl<'call, M: InputMode> ExcelParameter<'call, M> for ModuleName {
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        _: &CallContext,
        _: &mut M::Identity,
    ) -> XllResult<Self> {
        Self::from_value(value, argument)
    }

    fn encode_decoded(&self, _: &mut M::Identity) {}
}

impl ModuleName {
    fn from_value(value: XlValueRef<'_>, argument: &'static str) -> XllResult<Self> {
        let units = value.utf16(argument)?.to_vec();
        #[cfg(target_os = "windows")]
        let path = {
            use std::ffi::OsString;
            use std::os::windows::ffi::OsStringExt;
            PathBuf::from(OsString::from_wide(&units))
        };
        #[cfg(not(target_os = "windows"))]
        let path = PathBuf::from(
            String::from_utf16(&units)
                .map_err(|_| XllError::input(argument, InputError::InvalidUtf16))?,
        );
        Ok(Self { path, units })
    }
}

pub(super) fn decode_module_name<'call>(value: XlValueRef<'call>) -> XllResult<ModuleName> {
    ModuleName::from_value(value, "module")
}
