//! Excel callback resolution and raw `MdCallBack12` invocation.

use crate::{XL_ASYNC_RETURN, XL_FREE, XLOPER12};
#[cfg(any(target_os = "windows", feature = "abi-probe"))]
use core::ffi::c_void;
use core::ptr::{self, NonNull};
use smallvec::SmallVec;
use std::sync::OnceLock;
#[cfg(feature = "abi-probe")]
use std::sync::atomic::{AtomicPtr, Ordering};

pub const XLRET_SUCCESS: i32 = 0;
pub const XLRET_ABORT: i32 = 1;
pub const XLRET_FAILED: i32 = 32;
pub const XLRET_UNCALCED: i32 = 64;

type Excel12Callback = unsafe extern "system" fn(
    function: i32,
    argument_count: i32,
    arguments: *mut *mut XLOPER12,
    result: *mut XLOPER12,
) -> i32;

// Store the result, including a failed lookup, so concurrent callers cannot
// repeatedly resolve the host entry point.
static CALLBACK: OnceLock<Option<Excel12Callback>> = OnceLock::new();

// Test and ABI-probe callers must be able to install their callback even when
// the production resolver has already cached a failed lookup. Keep this
// override separate from the production cache so it can be replaced by each
// serialized test without changing normal host resolution.
#[cfg(feature = "abi-probe")]
static CALLBACK_OVERRIDE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

#[cfg(feature = "abi-probe")]
fn callback_override() -> Option<Excel12Callback> {
    let address = CALLBACK_OVERRIDE.load(Ordering::Acquire);
    if address.is_null() {
        None
    } else {
        // SAFETY: the installer requires the address to have Excel's exact
        // MdCallBack12 ABI and to remain live for the process lifetime.
        Some(unsafe { core::mem::transmute::<*mut c_void, Excel12Callback>(address) })
    }
}

#[cfg(target_os = "windows")]
fn resolve_callback() -> Option<Excel12Callback> {
    use crate::win32::{GetModuleHandleW, GetProcAddress};

    #[cfg(feature = "abi-probe")]
    if let Some(callback) = callback_override() {
        return Some(callback);
    }

    CALLBACK
        .get_or_init(|| {
            // SAFETY: A null module name asks for the host executable module.
            // The nul-terminated symbol is static, and the returned address
            // is checked before it is converted to a callback pointer.
            let address = unsafe {
                let host = GetModuleHandleW(ptr::null());
                if host.is_null() {
                    return None;
                }
                GetProcAddress(host, c"MdCallBack12".as_ptr().cast())
                    .map_or(ptr::null_mut(), |f| f as *const () as *mut c_void)
            };
            if address.is_null() {
                return None;
            }
            // SAFETY: XLCALL.CPP declares MdCallBack12 as
            // (xlfn, count, args, result), with the exact ABI above.
            Some(unsafe { core::mem::transmute::<*mut c_void, Excel12Callback>(address) })
        })
        .as_ref()
        .copied()
}

#[cfg(not(target_os = "windows"))]
fn resolve_callback() -> Option<Excel12Callback> {
    #[cfg(feature = "abi-probe")]
    if let Some(callback) = callback_override() {
        return Some(callback);
    }

    CALLBACK.get().copied().flatten()
}

/// Calls Excel's `MdCallBack12` trampoline.
///
/// # Safety
///
/// `argument_count` must be non-negative. `arguments` must point to writable
/// pointer-array storage containing `argument_count` live XLOPER12 pointers;
/// Excel may rewrite the array but must treat the pointed-to XLOPER12 values as
/// read-only. `result`, when non-null, must be writable for one XLOPER12. Calls
/// are only valid while Excel has transferred control to the XLL.
unsafe fn excel12v(
    function: i32,
    result: *mut XLOPER12,
    argument_count: i32,
    arguments: *mut *mut XLOPER12,
) -> i32 {
    if argument_count < 0 {
        return XLRET_FAILED;
    }
    let Some(callback) = resolve_callback() else {
        return XLRET_FAILED;
    };
    // SAFETY: The caller owns the pointer validity contract described above.
    unsafe { callback(function, argument_count, arguments, result) }
}

/// Installs a native callback address for the cross-language ABI probe.
///
/// This is deliberately unavailable in normal builds: production code must
/// resolve `MdCallBack12` from the Excel host process.
///
/// # Safety
///
/// `address` must point to a live function with Excel's exact
/// `(xlfn, count, args, result)` `MdCallBack12` ABI.
#[cfg(feature = "abi-probe")]
pub unsafe fn install_callback_for_abi_probe(address: *mut c_void) {
    CALLBACK_OVERRIDE.store(address, Ordering::Release);
}

/// Convenience wrapper for a slice of argument pointers.
///
/// # Safety
///
/// Every pointer in `arguments` must reference a live XLOPER12 for the duration
/// of the callback, and the callback must be valid in the host. Excel must
/// treat the pointed-to XLOPER12 values as read-only. The callback may rewrite
/// its pointer array because this function supplies a private mutable copy.
#[must_use]
pub unsafe fn excel12(function: i32, arguments: &[NonNull<XLOPER12>]) -> (i32, XLOPER12) {
    // SAFETY: the caller satisfies the same pointer and host-callback contract
    // required by `excel12_with_invocation`.
    let (status, result, _) = unsafe { excel12_with_invocation(function, arguments) };
    (status, result)
}

/// Calls Excel and also reports whether the host callback was actually invoked.
///
/// The invocation flag distinguishes a genuine Excel callback result from the
/// local `nil` placeholder returned when the callback is unavailable or the
/// argument count cannot be represented. Higher layers use that distinction to
/// call `xlFree` exactly for values that originated as C API return values.
///
/// # Safety
///
/// Every pointer in `arguments` must reference a live XLOPER12 for the duration
/// of the callback, and the callback must be valid in the host.
#[doc(hidden)]
#[must_use]
pub unsafe fn excel12_with_invocation(
    function: i32,
    arguments: &[NonNull<XLOPER12>],
) -> (i32, XLOPER12, bool) {
    let Ok(argument_count) = i32::try_from(arguments.len()) else {
        return (XLRET_FAILED, XLOPER12::nil(), false);
    };
    let Some(callback) = resolve_callback() else {
        return (XLRET_FAILED, XLOPER12::nil(), false);
    };
    // Keep a valid value for failure/abort returns as well. The Excel API
    // permits a callback to return an error without populating the result.
    let mut result = XLOPER12::nil();
    let mut raw_arguments: SmallVec<[*mut XLOPER12; 16]> =
        arguments.iter().map(|argument| argument.as_ptr()).collect();
    // SAFETY: the private pointer array supplies exactly the reported number
    // of live, read-only XLOPER12 pointers and may be mutated by the callback.
    // Result is writable for the duration of the call.
    let status = unsafe {
        callback(
            function,
            argument_count,
            raw_arguments.as_mut_ptr(),
            &mut result,
        )
    };
    (status, result, true)
}

/// Calls the native asynchronous UDF completion callback.
///
/// The returned value is the scalar acceptance flag from `xlAsyncReturn`.
/// This function deliberately does not expose a release operation: Excel's
/// async callback is permitted on a worker thread only for the completion
/// call, and its boolean result has no child allocation to release.
///
/// # Safety
///
/// `handle` and `result` must point to live XLOPER12 values for the duration
/// of the callback, and the callback must be valid in the host.
#[doc(hidden)]
#[must_use]
pub unsafe fn excel12_async_return(
    handle: NonNull<XLOPER12>,
    result: NonNull<XLOPER12>,
) -> (i32, XLOPER12, bool) {
    let arguments = [handle, result];
    // SAFETY: forwarded from this function's caller; both pointers are live
    // for the duration of the callback.
    unsafe { excel12_with_invocation(XL_ASYNC_RETURN, &arguments) }
}

/// Releases a value returned by an Excel C API callback.
///
/// # Safety
///
/// `value` must be the result storage supplied to a completed Excel4/Excel12
/// callback and must remain valid for this call. The `xlbitXLFree` flag is not
/// required; `xlFree` is valid for every C API return value.
pub unsafe fn excel_free(value: &mut XLOPER12) -> i32 {
    let mut pointer = value as *mut XLOPER12;
    // SAFETY: `pointer` is writable one-element pointer-array storage
    // containing the supplied live callback-result value.
    unsafe { excel12v(XL_FREE, ptr::null_mut(), 1, &mut pointer) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{XLTYPE_INT, XLTYPE_NIL};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FREE_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn fake_excel(
        function: i32,
        argument_count: i32,
        arguments: *mut *mut XLOPER12,
        result: *mut XLOPER12,
    ) -> i32 {
        if function == XL_ASYNC_RETURN {
            assert_eq!(argument_count, 2);
            assert!(!arguments.is_null());
            // SAFETY: the test supplies two live argument pointers.
            assert!(!unsafe { *arguments }.is_null());
            // SAFETY: the second argument is the live async return value.
            assert!(!unsafe { *arguments.add(1) }.is_null());
            assert!(!result.is_null());
            // SAFETY: the callback result is the scalar acceptance boolean.
            unsafe {
                *result = XLOPER12::boolean(true);
            }
            return XLRET_SUCCESS;
        }

        assert_eq!(argument_count, 1);
        assert!(!arguments.is_null());
        // SAFETY: the test supplies exactly one live argument pointer.
        assert!(!unsafe { *arguments }.is_null());
        // SAFETY: the callback contract permits rewriting its private pointer
        // array without mutating the caller's immutable slice storage.
        unsafe {
            *arguments = ptr::null_mut();
        }
        if function == XL_FREE {
            assert!(result.is_null());
            FREE_CALLS.fetch_add(1, Ordering::Relaxed);
            return XLRET_SUCCESS;
        }

        if function == 124 {
            return XLRET_FAILED;
        }

        assert_eq!(function, 123);
        assert!(!result.is_null());
        // SAFETY: The test wrapper always supplies a live result pointer.
        unsafe {
            *result = XLOPER12::integer(function);
        }
        XLRET_SUCCESS
    }

    fn install_fake() {
        let _ = CALLBACK.set(Some(fake_excel));
    }

    #[test]
    fn wrapper_passes_function_and_result() {
        install_fake();
        let mut argument = XLOPER12::integer(7);
        let arguments = [NonNull::from(&mut argument)];
        let original_argument = arguments[0];
        // SAFETY: one live argument is supplied and the installed test callback
        // has the exact Excel callback signature.
        let (status, result) = unsafe { excel12(123, &arguments) };
        assert_eq!(status, XLRET_SUCCESS);
        assert_eq!(arguments[0], original_argument);
        // SAFETY: XLTYPE_INT makes the integer union member active.
        assert_eq!(unsafe { argument.value.integer }, 7);
        assert_eq!(result.base_type(), XLTYPE_INT);
        // SAFETY: XLTYPE_INT makes the integer union member active.
        assert_eq!(unsafe { result.value.integer }, 123);
    }

    #[test]
    fn wrapper_reports_that_the_host_callback_was_invoked() {
        install_fake();
        let mut argument = XLOPER12::integer(7);
        let arguments = [NonNull::from(&mut argument)];
        // SAFETY: one live argument is supplied and the installed test callback
        // has the exact Excel callback signature.
        let (status, result, invoked) = unsafe { excel12_with_invocation(123, &arguments) };
        assert_eq!(status, XLRET_SUCCESS);
        assert!(invoked);
        assert_eq!(result.base_type(), XLTYPE_INT);
    }

    #[test]
    fn async_return_wrapper_passes_both_operands_without_a_release_callback() {
        install_fake();
        let mut handle = XLOPER12::integer(7);
        let mut value = XLOPER12::number(42.0);
        // SAFETY: both operands are live for the callback and the installed
        // test callback has the exact Excel callback signature.
        let (status, result, invoked) =
            unsafe { excel12_async_return(NonNull::from(&mut handle), NonNull::from(&mut value)) };
        assert_eq!(status, XLRET_SUCCESS);
        assert!(invoked);
        assert_eq!(result.base_type(), crate::XLTYPE_BOOL);
        // SAFETY: XLTYPE_BOOL selects the boolean union member.
        assert_ne!(unsafe { result.value.boolean }, 0);
    }

    #[test]
    fn failed_callback_returns_a_well_formed_result() {
        install_fake();
        let mut argument = XLOPER12::integer(7);
        let arguments = [NonNull::from(&mut argument)];
        // SAFETY: one live argument is supplied and the installed test callback
        // has the exact Excel callback signature.
        let (status, result, invoked) = unsafe { excel12_with_invocation(124, &arguments) };
        assert_eq!(status, XLRET_FAILED);
        assert!(invoked);
        assert_eq!(result.base_type(), XLTYPE_NIL);
    }

    #[test]
    fn xl_free_accepts_a_callback_result_without_an_ownership_flag() {
        install_fake();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let mut argument = XLOPER12::integer(7);
        let arguments = [NonNull::from(&mut argument)];
        // SAFETY: one live argument is supplied and the installed test callback
        // has the exact Excel callback signature.
        let (status, mut result) = unsafe { excel12(123, &arguments) };
        assert_eq!(status, XLRET_SUCCESS);
        // SAFETY: `result` is the result storage populated by the callback above.
        assert_eq!(unsafe { excel_free(&mut result) }, XLRET_SUCCESS);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn default_result_is_well_formed() {
        let result = XLOPER12::nil();
        assert_eq!(result.base_type(), XLTYPE_NIL);
    }

    #[test]
    fn low_level_callback_rejects_a_negative_argument_count() {
        install_fake();
        // SAFETY: the negative count is rejected before either pointer is used.
        let status = unsafe { excel12v(123, ptr::null_mut(), -1, ptr::null_mut()) };
        assert_eq!(status, XLRET_FAILED);
    }
}
