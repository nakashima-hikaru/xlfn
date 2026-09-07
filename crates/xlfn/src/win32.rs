#![allow(
    non_snake_case,
    non_upper_case_globals,
    non_camel_case_types,
    dead_code,
    unreachable_pub,
    clippy::all,
    reason = "Generated code from windows-bindgen"
)]

windows_link::link!("kernel32.dll" "system" fn CloseHandle(hobject : HANDLE) -> BOOL);
windows_link::link!("ole32.dll" "system" fn CoCreateGuid(pguid : *mut GUID) -> HRESULT);
windows_link::link!("ole32.dll" "system" fn CoCreateInstance(rclsid : *const GUID, punkouter : *mut core::ffi::c_void, dwclscontext : u32, riid : *const GUID, ppv : *mut *mut core::ffi::c_void) -> HRESULT);
windows_link::link!("ole32.dll" "system" fn CoInitializeEx(pvreserved : *const core::ffi::c_void, dwcoinit : u32) -> HRESULT);
windows_link::link!("ole32.dll" "system" fn CoUninitialize());
windows_link::link!("ole32.dll" "system" fn CoWaitForMultipleHandles(dwflags : u32, dwtimeout : u32, chandles : u32, phandles : *const HANDLE, lpdwindex : *mut u32) -> HRESULT);
windows_link::link!("kernel32.dll" "system" fn CreateEventW(lpeventattributes : *const SECURITY_ATTRIBUTES, bmanualreset : BOOL, binitialstate : BOOL, lpname : PCWSTR) -> HANDLE);
windows_link::link!("kernel32.dll" "system" fn CreateMutexW(lpmutexattributes : *const SECURITY_ATTRIBUTES, binitialowner : BOOL, lpname : PCWSTR) -> HANDLE);
windows_link::link!("kernel32.dll" "system" fn FreeLibrary(hlibmodule : HMODULE) -> BOOL);
windows_link::link!("kernel32.dll" "system" fn GetLastError() -> u32);
windows_link::link!("kernel32.dll" "system" fn GetModuleHandleExW(dwflags : u32, lpmodulename : PCWSTR, phmodule : *mut HMODULE) -> BOOL);
windows_link::link!("kernel32.dll" "system" fn OutputDebugStringW(lpoutputstring : PCWSTR));
windows_link::link!("advapi32.dll" "system" fn RegCloseKey(hkey : HKEY) -> LSTATUS);
windows_link::link!("advapi32.dll" "system" fn RegCreateKeyExW(hkey : HKEY, lpsubkey : PCWSTR, reserved : u32, lpclass : PCWSTR, dwoptions : u32, samdesired : REGSAM, lpsecurityattributes : *const SECURITY_ATTRIBUTES, phkresult : *mut HKEY, lpdwdisposition : *mut u32) -> LSTATUS);
windows_link::link!("advapi32.dll" "system" fn RegDeleteTreeW(hkey : HKEY, lpsubkey : PCWSTR) -> LSTATUS);
windows_link::link!("advapi32.dll" "system" fn RegEnumKeyExW(hkey : HKEY, dwindex : u32, lpname : PWSTR, lpcchname : *mut u32, lpreserved : *const u32, lpclass : PWSTR, lpcchclass : *mut u32, lpftlastwritetime : *mut FILETIME) -> LSTATUS);
windows_link::link!("advapi32.dll" "system" fn RegOpenKeyExW(hkey : HKEY, lpsubkey : PCWSTR, uloptions : u32, samdesired : REGSAM, phkresult : *mut HKEY) -> LSTATUS);
windows_link::link!("advapi32.dll" "system" fn RegQueryValueExW(hkey : HKEY, lpvaluename : PCWSTR, lpreserved : *const u32, lptype : *mut u32, lpdata : *mut u8, lpcbdata : *mut u32) -> LSTATUS);
windows_link::link!("advapi32.dll" "system" fn RegSetValueExW(hkey : HKEY, lpvaluename : PCWSTR, reserved : u32, dwtype : u32, lpdata : *const u8, cbdata : u32) -> LSTATUS);
windows_link::link!("kernel32.dll" "system" fn ReleaseMutex(hmutex : HANDLE) -> BOOL);
windows_link::link!("kernel32.dll" "system" fn ResetEvent(hevent : HANDLE) -> BOOL);
windows_link::link!("oleaut32.dll" "system" fn SafeArrayCreate(vt : VARTYPE, cdims : u32, rgsabound : *const SAFEARRAYBOUND) -> *mut SAFEARRAY);
windows_link::link!("oleaut32.dll" "system" fn SafeArrayDestroy(psa : *const SAFEARRAY) -> HRESULT);
windows_link::link!("oleaut32.dll" "system" fn SafeArrayGetDim(psa : *const SAFEARRAY) -> u32);
windows_link::link!("oleaut32.dll" "system" fn SafeArrayGetElement(psa : *const SAFEARRAY, rgindices : *const i32, pv : *mut core::ffi::c_void) -> HRESULT);
windows_link::link!("oleaut32.dll" "system" fn SafeArrayGetLBound(psa : *const SAFEARRAY, ndim : u32, pllbound : *mut i32) -> HRESULT);
windows_link::link!("oleaut32.dll" "system" fn SafeArrayGetUBound(psa : *const SAFEARRAY, ndim : u32, plubound : *mut i32) -> HRESULT);
windows_link::link!("oleaut32.dll" "system" fn SafeArrayGetVartype(psa : *const SAFEARRAY, pvt : *mut VARTYPE) -> HRESULT);
windows_link::link!("oleaut32.dll" "system" fn SafeArrayPutElement(psa : *const SAFEARRAY, rgindices : *const i32, pv : *const core::ffi::c_void) -> HRESULT);
windows_link::link!("kernel32.dll" "system" fn SetEvent(hevent : HANDLE) -> BOOL);
windows_link::link!("oleaut32.dll" "system" fn SysAllocStringLen(strin : *const OLECHAR, ui : u32) -> BSTR);
windows_link::link!("oleaut32.dll" "system" fn SysFreeString(bstrstring : BSTR));
windows_link::link!("oleaut32.dll" "system" fn SysStringLen(pbstr : BSTR) -> u32);
windows_link::link!("oleaut32.dll" "system" fn VariantClear(pvarg : *mut VARIANTARG) -> HRESULT);
windows_link::link!("kernel32.dll" "system" fn WaitForSingleObject(hhandle : HANDLE, dwmilliseconds : u32) -> u32);
pub type ACCESS_MASK = u32;
pub type BOOL = i32;
pub type BSTR = *const u16;
pub const CLASS_E_CLASSNOTAVAILABLE: HRESULT = 0x80040111_u32 as _;
pub const CLASS_E_NOAGGREGATION: HRESULT = 0x80040110_u32 as _;
pub type CLSCTX = u32;
pub const CLSCTX_INPROC_SERVER: CLSCTX = 1;
pub type COINIT = i32;
pub const COINIT_MULTITHREADED: COINIT = 0;
pub const COWAIT_DISPATCH_CALLS: COWAIT_FLAGS = 8;
pub type COWAIT_FLAGS = u32;
pub const CO_E_SERVER_STOPPING: HRESULT = 0x80080008_u32 as _;
#[repr(C)]
#[derive(Clone, Copy)]
pub union CY {
    pub Anonymous: CY_0,
    pub int64: i64,
}
impl Default for CY {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CY_0 {
    pub Lo: u32,
    pub Hi: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DECIMAL {
    pub wReserved: u16,
    pub Anonymous: DECIMAL_0,
    pub Hi32: u32,
    pub Anonymous2: DECIMAL_1,
}
impl Default for DECIMAL {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub union DECIMAL_0 {
    pub Anonymous: DECIMAL_0_0,
    pub signscale: u16,
}
impl Default for DECIMAL_0 {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DECIMAL_0_0 {
    pub scale: u8,
    pub sign: u8,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub union DECIMAL_1 {
    pub Anonymous: DECIMAL_1_0,
    pub Lo64: u64,
}
impl Default for DECIMAL_1 {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DECIMAL_1_0 {
    pub Lo32: u32,
    pub Mid32: u32,
}
pub const DISPATCH_METHOD: i32 = 1;
pub type DISPID = i32;
pub const DISPID_UNKNOWN: i32 = -1;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DISPPARAMS {
    pub rgvarg: *mut VARIANTARG,
    pub rgdispidNamedArgs: *mut DISPID,
    pub cArgs: u32,
    pub cNamedArgs: u32,
}
pub const DISP_E_BADINDEX: HRESULT = 0x8002000B_u32 as _;
pub const DISP_E_BADPARAMCOUNT: HRESULT = 0x8002000E_u32 as _;
pub const DISP_E_MEMBERNOTFOUND: HRESULT = 0x80020003_u32 as _;
pub const DISP_E_PARAMNOTFOUND: HRESULT = 0x80020004_u32 as _;
pub const DISP_E_TYPEMISMATCH: HRESULT = 0x80020005_u32 as _;
pub const DISP_E_UNKNOWNINTERFACE: HRESULT = 0x80020001_u32 as _;
pub const DISP_E_UNKNOWNNAME: HRESULT = 0x80020006_u32 as _;
pub const ERROR_FILE_NOT_FOUND: i32 = 2;
pub const ERROR_NO_MORE_ITEMS: i32 = 259;
pub const ERROR_SUCCESS: i32 = 0;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct EXCEPINFO {
    pub wCode: u16,
    pub wReserved: u16,
    pub bstrSource: BSTR,
    pub bstrDescription: BSTR,
    pub bstrHelpFile: BSTR,
    pub dwHelpContext: u32,
    pub pvReserved: *mut core::ffi::c_void,
    pub pfnDeferredFillIn: *mut u8,
    pub scode: SCODE,
}
pub const E_FAIL: HRESULT = 0x80004005_u32 as _;
pub const E_INVALIDARG: HRESULT = 0x80070057_u32 as _;
pub const E_NOINTERFACE: HRESULT = 0x80004002_u32 as _;
pub const E_NOTIMPL: HRESULT = 0x80004001_u32 as _;
pub const E_OUTOFMEMORY: HRESULT = 0x8007000E_u32 as _;
pub const E_POINTER: HRESULT = 0x80004003_u32 as _;
pub const E_UNEXPECTED: HRESULT = 0x8000FFFF_u32 as _;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FILETIME {
    pub dwLowDateTime: u32,
    pub dwHighDateTime: u32,
}
pub const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: i32 = 4;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}
pub type HANDLE = *mut core::ffi::c_void;
pub type HINSTANCE = *mut core::ffi::c_void;
pub type HKEY = *mut core::ffi::c_void;
pub const HKEY_CURRENT_USER: HKEY = -2147483647 as _;
pub type HMODULE = HINSTANCE;
pub type HRESULT = i32;
pub const IID_IDispatch: GUID = GUID {
    data1: 0x00020400,
    data2: 0x0000,
    data3: 0x0000,
    data4: [192, 0, 0, 0, 0, 0, 0, 70],
};
#[repr(C)]
pub struct IDispatch_Vtbl {
    pub base__: IUnknown_Vtbl,
    GetTypeInfoCount: usize,
    GetTypeInfo: usize,
    GetIDsOfNames: usize,
    Invoke: usize,
}
pub const INFINITE: u32 = 4294967295;
pub const IID_IRecordInfo: GUID = GUID {
    data1: 0x0000002f,
    data2: 0x0000,
    data3: 0x0000,
    data4: [192, 0, 0, 0, 0, 0, 0, 70],
};
#[repr(C)]
pub struct IRecordInfo_Vtbl {
    pub base__: IUnknown_Vtbl,
    RecordInit: usize,
    RecordClear: usize,
    RecordCopy: usize,
    GetGuid: usize,
    GetName: usize,
    GetSize: usize,
    GetTypeInfo: usize,
    GetField: usize,
    GetFieldNoCopy: usize,
    PutField: usize,
    PutFieldNoCopy: usize,
    GetFieldNames: usize,
    IsMatchingType: usize,
    RecordCreate: usize,
    RecordCreateCopy: usize,
    RecordDestroy: usize,
}
pub const IID_IUnknown: GUID = GUID {
    data1: 0x00000000,
    data2: 0x0000,
    data3: 0x0000,
    data4: [192, 0, 0, 0, 0, 0, 0, 70],
};
#[repr(C)]
pub struct IUnknown_Vtbl {
    pub QueryInterface: unsafe extern "system" fn(
        this: *mut core::ffi::c_void,
        iid: *const GUID,
        interface: *mut *mut core::ffi::c_void,
    ) -> HRESULT,
    pub AddRef: unsafe extern "system" fn(this: *mut core::ffi::c_void) -> u32,
    pub Release: unsafe extern "system" fn(this: *mut core::ffi::c_void) -> u32,
}
pub const KEY_READ: i32 = 131097;
pub const KEY_WRITE: i32 = 131078;
pub type LSTATUS = i32;
pub type OLECHAR = u16;
pub type PCWSTR = *const u16;
pub type PWSTR = *mut u16;
pub type REGSAM = ACCESS_MASK;
pub const REG_OPTION_NON_VOLATILE: i32 = 0;
pub const REG_SZ: u32 = 1;
pub const RPC_E_CHANGED_MODE: HRESULT = 0x80010106_u32 as _;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SAFEARRAY {
    pub cDims: u16,
    pub fFeatures: u16,
    pub cbElements: u32,
    pub cLocks: u32,
    pub pvData: *mut core::ffi::c_void,
    pub rgsabound: [SAFEARRAYBOUND; 1],
}
impl Default for SAFEARRAY {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SAFEARRAYBOUND {
    pub cElements: u32,
    pub lLbound: i32,
}
pub type SCODE = i32;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SECURITY_ATTRIBUTES {
    pub nLength: u32,
    pub lpSecurityDescriptor: *mut core::ffi::c_void,
    pub bInheritHandle: BOOL,
}
pub const S_FALSE: HRESULT = 0x1_u32 as _;
pub const S_OK: HRESULT = 0x0_u32 as _;
pub type VARENUM = i32;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VARIANT {
    pub Anonymous: VARIANT_0,
}
impl Default for VARIANT {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub union VARIANT_0 {
    pub Anonymous: VARIANT_0_0,
    pub decVal: DECIMAL,
}
impl Default for VARIANT_0 {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VARIANT_0_0 {
    pub vt: VARTYPE,
    pub wReserved1: u16,
    pub wReserved2: u16,
    pub wReserved3: u16,
    pub Anonymous: VARIANT_0_0_0,
}
impl Default for VARIANT_0_0 {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub union VARIANT_0_0_0 {
    pub llVal: i64,
    pub lVal: i32,
    pub bVal: u8,
    pub iVal: i16,
    pub fltVal: f32,
    pub dblVal: f64,
    pub boolVal: VARIANT_BOOL,
    pub __OBSOLETE__VARIANT_BOOL: VARIANT_BOOL,
    pub scode: SCODE,
    pub cyVal: CY,
    pub date: f64,
    pub bstrVal: BSTR,
    pub punkVal: *mut core::ffi::c_void,
    pub pdispVal: *mut core::ffi::c_void,
    pub parray: *mut SAFEARRAY,
    pub pbVal: *mut u8,
    pub piVal: *mut i16,
    pub plVal: *mut i32,
    pub pllVal: *mut i64,
    pub pfltVal: *mut f32,
    pub pdblVal: *mut f64,
    pub pboolVal: *mut VARIANT_BOOL,
    pub __OBSOLETE__VARIANT_PBOOL: *mut VARIANT_BOOL,
    pub pscode: *mut SCODE,
    pub pcyVal: *mut CY,
    pub pdate: *mut f64,
    pub pbstrVal: *mut BSTR,
    pub ppunkVal: *mut *mut core::ffi::c_void,
    pub ppdispVal: *mut *mut core::ffi::c_void,
    pub pparray: *mut *mut SAFEARRAY,
    pub pvarVal: *mut VARIANT,
    pub byref: *mut core::ffi::c_void,
    pub cVal: i8,
    pub uiVal: u16,
    pub ulVal: u32,
    pub ullVal: u64,
    pub intVal: i32,
    pub uintVal: u32,
    pub pdecVal: *mut DECIMAL,
    pub pcVal: *mut i8,
    pub puiVal: *mut u16,
    pub pulVal: *mut u32,
    pub pullVal: *mut u64,
    pub pintVal: *mut i32,
    pub puintVal: *mut u32,
    pub Anonymous: VARIANT_0_0_0_0,
}
impl Default for VARIANT_0_0_0 {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VARIANT_0_0_0_0 {
    pub pvRecord: *mut core::ffi::c_void,
    pub pRecInfo: *mut core::ffi::c_void,
}
pub type VARIANTARG = VARIANT;
pub type VARIANT_BOOL = i16;
pub const VARIANT_FALSE: VARIANT_BOOL = 0;
pub const VARIANT_TRUE: VARIANT_BOOL = -1;
pub type VARTYPE = u16;
pub const VT_ARRAY: VARENUM = 8192;
pub const VT_BOOL: VARENUM = 11;
pub const VT_BSTR: VARENUM = 8;
pub const VT_BYREF: VARENUM = 16384;
pub const VT_DISPATCH: VARENUM = 9;
pub const VT_EMPTY: VARENUM = 0;
pub const VT_ERROR: VARENUM = 10;
pub const VT_I4: VARENUM = 3;
pub const VT_R8: VARENUM = 5;
pub const VT_UNKNOWN: VARENUM = 13;
pub const VT_VARIANT: VARENUM = 12;
pub const WAIT_ABANDONED: i32 = 128;
pub const WAIT_FAILED: u32 = 4294967295;
pub const WAIT_OBJECT_0: i32 = 0;
