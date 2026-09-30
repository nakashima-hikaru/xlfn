#![allow(unsafe_code, reason = "Exercises generated Excel metadata ABI exports")]

use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn::prelude::*;
use xlfn_sys::{XLOPER12, XLTYPE_ERR, XLTYPE_STR};

#[excel_addin(name = "仕様🚀")]
struct MetadataAddin;

static OPEN_CALLS: AtomicUsize = AtomicUsize::new(0);

impl Addin for MetadataAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Layers = ();
    type Error = XllError;

    fn open(_: &OpenContext) -> Result<Opened<()>, XllError> {
        OPEN_CALLS.fetch_add(1, Ordering::Relaxed);
        panic!("a metadata query must not initialize the add-in")
    }
}

#[test]
fn generated_metadata_export_returns_unicode_name_before_auto_open() {
    // SAFETY: These live numeric actions satisfy Excel's metadata ABI.
    let name = unsafe { xlAddInManagerInfo12(&mut XLOPER12::number(1.0)) };
    // SAFETY: The generated export returned its immutable static name root.
    let root = unsafe { &*name };
    assert_eq!(root.xltype, XLTYPE_STR);
    // SAFETY: XLTYPE_STR selects the counted UTF-16 string member.
    let string = unsafe { root.value.string };
    // SAFETY: The generated name has four UTF-16 units after its length unit.
    let units = unsafe { std::slice::from_raw_parts(string, 5) };
    assert_eq!(units[0], 4);
    assert_eq!(String::from_utf16(&units[1..]).unwrap(), "仕様🚀");

    // SAFETY: The action is live throughout the call.
    let error = unsafe { xlAddInManagerInfo12(&mut XLOPER12::integer(2)) };
    assert_ne!(name, error);
    // SAFETY: The generated export returned its immutable static error root.
    let root = unsafe { &*error };
    assert_eq!(root.xltype, XLTYPE_ERR);
    // SAFETY: XLTYPE_ERR selects the error union member.
    assert_eq!(unsafe { root.value.error }, xlfn_sys::XLERR_VALUE);

    // SAFETY: The unopened runtime has no lifecycle callbacks to invoke; this
    // test simulates Excel's close entrypoint on the current lifecycle thread.
    assert_eq!(unsafe { xlAutoClose() }, 1);
    // SAFETY: The action is live after close; the earlier root remains read-only.
    let after_close = unsafe { xlAddInManagerInfo12(&mut XLOPER12::integer(1)) };
    assert_eq!(after_close, name);
    assert_eq!(OPEN_CALLS.load(Ordering::Relaxed), 0);
}
