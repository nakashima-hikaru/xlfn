#![allow(
    non_snake_case,
    non_upper_case_globals,
    non_camel_case_types,
    dead_code,
    unreachable_pub,
    clippy::all,
    reason = "Generated code from windows-bindgen"
)]

windows_link::link!("kernel32.dll" "system" fn GetModuleHandleW(lpmodulename : PCWSTR) -> HMODULE);
windows_link::link!("kernel32.dll" "system" fn GetProcAddress(hmodule : HMODULE, lpprocname : PCSTR) -> FARPROC);
pub type FARPROC = Option<unsafe extern "system" fn() -> isize>;
pub type HINSTANCE = *mut core::ffi::c_void;
pub type HMODULE = HINSTANCE;
pub type PCSTR = *const u8;
pub type PCWSTR = *const u16;
