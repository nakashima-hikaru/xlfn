//! Exercise a failed registration through the production RTD-enabled library.
#![cfg(all(feature = "rtd", not(target_os = "windows")))]

use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn::{addin::RuntimeConfig, prelude::*};
use xlfn_sys::{
    XL_EVENT_REGISTER, XL_FREE, XL_GET_NAME, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER,
    XLF_UNREGISTER, XLOPER12, XLOPER12Value, XLRET_FAILED, XLRET_SUCCESS, XLTYPE_STR,
};

static OPENS: AtomicUsize = AtomicUsize::new(0);
static REGISTRATIONS: AtomicUsize = AtomicUsize::new(0);
static CALC_EVENTS: AtomicUsize = AtomicUsize::new(0);
static MODULE_PATH: [u16; 10] = [9, 47, 116, 101, 115, 116, 46, 120, 108, 108];

struct RollbackAddin;

#[excel_addin(name = "Rollback test", id = "rtd-rollback-test")]
impl Addin for RollbackAddin {
    type SharedState = ();

    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        OPENS.fetch_add(1, Ordering::Relaxed);
        Ok(Opened::new(())
            .with_runtime_config(RuntimeConfig::new().with_calculation_end_notifications(false)))
    }
}

#[excel_function]
fn rollback_test() -> f64 {
    1.0
}

unsafe extern "system" fn excel_callback(
    function: i32,
    _: i32,
    _: *mut *mut XLOPER12,
    result: *mut XLOPER12,
) -> i32 {
    if function == XL_FREE {
        return XLRET_SUCCESS;
    }
    if result.is_null() {
        return XLRET_FAILED;
    }
    let reply = match function {
        XL_GET_NAME => XLOPER12 {
            value: XLOPER12Value {
                string: MODULE_PATH.as_ptr().cast_mut(),
            },
            xltype: XLTYPE_STR,
        },
        XL_EVENT_REGISTER => {
            CALC_EVENTS.fetch_add(1, Ordering::Relaxed);
            XLOPER12::integer(17)
        }
        XLF_EVALUATE => XLOPER12::error(XLERR_NAME),
        XLF_REGISTER => {
            if REGISTRATIONS.fetch_add(1, Ordering::Relaxed) == 0 {
                return XLRET_FAILED;
            }
            XLOPER12::number(42.0)
        }
        XLF_UNREGISTER => XLOPER12::boolean(true),
        _ => return XLRET_FAILED,
    };
    // SAFETY: the framework supplies an ABI result slot for these callbacks.
    unsafe { result.write(reply) };
    XLRET_SUCCESS
}

#[test]
fn registration_failure_before_rtd_publication_can_reopen() {
    // SAFETY: this process-local fixture implements Excel's callback ABI.
    unsafe {
        xlfn_sys::install_callback_for_abi_probe(
            excel_callback as *const () as *mut std::ffi::c_void,
        )
    };
    // SAFETY: this test is the only user and emulates the Excel lifecycle thread.
    assert_eq!(unsafe { xlAutoOpen() }, 0);
    assert_eq!(OPENS.load(Ordering::Relaxed), 1);
    assert_eq!(REGISTRATIONS.load(Ordering::Relaxed), 1);

    // SAFETY: the same lifecycle thread retries with a successful Excel reply.
    assert_eq!(unsafe { xlAutoOpen() }, 1);
    assert_eq!(OPENS.load(Ordering::Relaxed), 2);
    assert_eq!(REGISTRATIONS.load(Ordering::Relaxed), 2);
    assert_eq!(CALC_EVENTS.load(Ordering::Relaxed), 0);
    // SAFETY: the emulated lifecycle thread owns final removal and close.
    assert_eq!(unsafe { xlAutoRemove() }, 1);
    // SAFETY: the emulated lifecycle thread still owns the close callback.
    assert_eq!(unsafe { xlAutoClose() }, 1);
}
