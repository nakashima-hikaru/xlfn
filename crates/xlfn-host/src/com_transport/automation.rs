use crate::win32::{
    E_FAIL, E_INVALIDARG, E_OUTOFMEMORY, E_POINTER, S_OK, SAFEARRAY, SAFEARRAYBOUND,
    SafeArrayAccessData, SafeArrayCreate, SafeArrayDestroy, SafeArrayUnaccessData,
    SysAllocStringLen, SysFreeString, VARIANT,
};
use std::ptr::{self, NonNull};

const VT_BOOL: u16 = crate::win32::VT_BOOL as u16;
const VT_BSTR: u16 = crate::win32::VT_BSTR as u16;
const VT_EMPTY: u16 = crate::win32::VT_EMPTY as u16;
const VT_ERROR: u16 = crate::win32::VT_ERROR as u16;
const VT_I4: u16 = crate::win32::VT_I4 as u16;
const VT_R8: u16 = crate::win32::VT_R8 as u16;
const VT_VARIANT: u16 = crate::win32::VT_VARIANT as u16;

/// Initializes an empty output cell with an owned BSTR on success.
///
/// # Safety
/// A non-null `result` must be writable and must not own a resource; the
/// caller receives ownership of the resulting VARIANT only on success.
pub unsafe fn write_bstr_variant(result: *mut VARIANT, value: &str) -> i32 {
    if result.is_null() {
        return E_POINTER;
    }
    let length = xlfn_common::utf16_code_units(value);
    if length > xlfn_common::EXCEL_STRING_LIMIT {
        return E_INVALIDARG;
    }

    // SAFETY: a null source requests an uninitialized BSTR with exactly this
    // many code units plus its terminator. The validated Excel limit fits u32.
    let bstr = unsafe { SysAllocStringLen(ptr::null(), length as u32) };
    // The generated BSTR alias is const-qualified, but this allocation is new
    // writable storage exclusively owned by this function.
    let Some(bstr) = NonNull::new(bstr.cast_mut()) else {
        return E_FAIL;
    };
    let owner = std::mem::DropGuard::new(bstr, |bstr| {
        // SAFETY: this guard owns the allocation until the completed VARIANT
        // receives it; unwind before transfer frees it exactly once.
        unsafe { SysFreeString(bstr.as_ptr()) };
    });
    if value.len() == length {
        // Equal UTF-8 and UTF-16 lengths prove ASCII for a valid Rust str.
        for (index, &byte) in value.as_bytes().iter().enumerate() {
            // SAFETY: the byte count equals the allocated code-unit count.
            // Write directly without borrowing uninitialized BSTR contents.
            unsafe { bstr.as_ptr().wrapping_add(index).write(u16::from(byte)) };
        }
    } else {
        for (index, unit) in value.encode_utf16().enumerate() {
            // SAFETY: checked_utf16_len counted this same immutable string, so
            // every write is in bounds. The allocator's terminator is retained.
            unsafe { bstr.as_ptr().wrapping_add(index).write(unit) };
        }
    }

    let mut variant = VARIANT::default();
    variant.Anonymous.Anonymous.vt = VT_BSTR;
    variant.Anonymous.Anonymous.Anonymous.bstrVal = bstr.as_ptr();

    // SAFETY: the caller guarantees that `result` points to writable VARIANT
    // output storage. Ownership of `bstr` transfers into the written VARIANT.
    unsafe { *result = variant };
    let _ = std::mem::DropGuard::dismiss(owner);

    S_OK
}

/// Native representation before taking ownership of a destination VARIANT.
/// The host caller determines the application-specific meaning of a SCODE.
pub enum VariantValue<'a> {
    Number(f64),
    Boolean(bool),
    Integer(i32),
    Text(&'a str),
    ErrorCode(i32),
    Empty,
}

/// # Safety
/// A non-null `result` must be writable and must not own a resource.
/// A successful call transfers the new VARIANT to that slot.
pub unsafe fn write_variant(result: *mut VARIANT, value: VariantValue<'_>) -> i32 {
    if result.is_null() {
        return E_POINTER;
    }

    let mut variant = VARIANT::default();

    match value {
        VariantValue::Number(value) => {
            variant.Anonymous.Anonymous.vt = VT_R8;
            variant.Anonymous.Anonymous.Anonymous.dblVal = value;
        }
        VariantValue::Boolean(value) => {
            variant.Anonymous.Anonymous.vt = VT_BOOL;
            variant.Anonymous.Anonymous.Anonymous.boolVal = if value { -1 } else { 0 };
        }
        VariantValue::Integer(value) => {
            variant.Anonymous.Anonymous.vt = VT_I4;
            variant.Anonymous.Anonymous.Anonymous.lVal = value;
        }
        VariantValue::Text(value) => {
            // SAFETY: `result` was validated as non-null above and `value`
            // remains readable for the duration of the call.
            return unsafe { write_bstr_variant(result, value) };
        }
        VariantValue::ErrorCode(scode) => {
            variant.Anonymous.Anonymous.vt = VT_ERROR;
            variant.Anonymous.Anonymous.Anonymous.scode = scode;
        }
        VariantValue::Empty => {
            variant.Anonymous.Anonymous.vt = VT_EMPTY;
        }
    }

    // SAFETY: `result` was validated as non-null and points to writable VARIANT
    // storage supplied by the caller.
    unsafe { *result = variant };

    S_OK
}

/// Raw Automation primitives used only by the private refresh ownership core.
/// These functions do not issue batch delivery authority.
pub enum VariantArray {}
impl VariantArray {
    pub fn create(count: u32) -> Result<NonNull<SAFEARRAY>, i32> {
        let Some(cells) = (count as usize).checked_mul(2) else {
            return Err(E_INVALIDARG);
        };
        if std::alloc::Layout::array::<VARIANT>(cells).is_err() {
            return Err(E_INVALIDARG);
        }
        let mut bounds = [
            SAFEARRAYBOUND {
                cElements: 2,
                lLbound: 0,
            },
            SAFEARRAYBOUND {
                cElements: count,
                lLbound: 0,
            },
        ];
        // SAFETY: bounds remain live and describe a two-dimensional VARIANT array.
        NonNull::new(unsafe { SafeArrayCreate(VT_VARIANT, 2, bounds.as_mut_ptr()) })
            .ok_or(E_OUTOFMEMORY)
    }

    /// # Safety
    /// `array` must be live, unlocked and uniquely controlled by the caller.
    /// On success the caller owns exactly one access that must be released.
    pub unsafe fn access(array: NonNull<SAFEARRAY>) -> Result<*mut VARIANT, i32> {
        let mut data = ptr::null_mut();
        // SAFETY: the ownership core retains this fresh, unlocked array.
        let status = unsafe { SafeArrayAccessData(array.as_ptr(), &mut data) };
        if status < 0 {
            Err(status)
        } else {
            Ok(data.cast())
        }
    }

    /// # Safety
    /// The caller must own one successful access of this live array and stop
    /// using its data before the release attempt.
    pub unsafe fn unaccess(array: NonNull<SAFEARRAY>) -> Result<(), i32> {
        // SAFETY: this is the one data access retained by the ownership core.
        let status = unsafe { SafeArrayUnaccessData(array.as_ptr()) };
        if status < 0 { Err(status) } else { Ok(()) }
    }

    /// # Safety
    /// The caller must own this live, unlocked array; ownership does not remain
    /// with the caller and the array must not be used again.
    pub unsafe fn destroy(array: NonNull<SAFEARRAY>) {
        // SAFETY: the ownership core destroys only its untransferred unlocked array.
        if unsafe { SafeArrayDestroy(array.as_ptr()) } < 0 {
            xlfn_kernel::invariant::fail_stop();
        }
    }
}
