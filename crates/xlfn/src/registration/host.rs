//! Host-module discovery and Excel callback boundary helpers.

use crate::{CallContext, ExcelParameter, InputError, XlValueRef, XllError, XllResult};
use std::path::PathBuf;

pub(super) struct ModuleName {
    pub(super) path: PathBuf,
    pub(super) units: Vec<u16>,
}

impl<'call> ExcelParameter<'call> for ModuleName {
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        _: &CallContext,
        _identity: Option<&mut crate::InputIdentityEncoder>,
    ) -> XllResult<Self> {
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
    ModuleName::decode(value, "module", &CallContext::without_runtime(), None)
}
