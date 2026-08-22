#[cfg(any(feature = "sdk-bindgen", test))]
use std::collections::BTreeMap;
#[cfg(feature = "sdk-bindgen")]
use std::mem::{align_of, offset_of, size_of};

#[cfg(feature = "sdk-bindgen")]
use xlfn_sys::{
    IDSHEET, XL_ASYNC_RETURN, XL_EVENT_REGISTER, XLBIT_DLL_FREE, XLEVENT_CALCULATION_CANCELED,
    XLEVENT_CALCULATION_ENDED, XLF_REGISTER, XLF_UNREGISTER, XLOPER12, XLOPER12Array, XLOPER12Flow,
    XLOPER12FlowValue, XLOPER12MRef, XLOPER12SRef, XLREF12,
};

#[cfg(feature = "sdk-bindgen")]
#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unsafe_op_in_unsafe_fn
)]
mod sdk {
    include!(concat!(env!("OUT_DIR"), "/xlcall.rs"));
}

#[cfg(feature = "sdk-bindgen")]
unsafe extern "C" {
    fn xlfn_probe_xloper12_size() -> usize;
    fn xlfn_probe_xloper12_align() -> usize;
    fn xlfn_probe_xloper12_xltype_offset() -> usize;
    fn xlfn_probe_xloper12_array_size() -> usize;
    fn xlfn_probe_xloper12_sref_size() -> usize;
    fn xlfn_probe_idsheet_size() -> usize;
    fn xlfn_probe_xloper12_mref_size() -> usize;
    fn xlfn_probe_xloper12_mref_idsheet_offset() -> usize;
    fn xlfn_probe_xloper12_flow_size() -> usize;
    fn xlfn_probe_xloper12_flow_value_size() -> usize;
    fn xlfn_probe_xloper12_flow_row_offset() -> usize;
    fn xlfn_probe_xloper12_flow_column_offset() -> usize;
    fn xlfn_probe_xloper12_flow_type_offset() -> usize;
    fn xlfn_probe_xloper12_flow_level_size() -> usize;
    fn xlfn_probe_xloper12_flow_toolbar_control_size() -> usize;
    fn xlfn_probe_xlref12_size() -> usize;
    fn xlfn_probe_xl_async_return() -> i32;
    fn xlfn_probe_xl_event_register() -> i32;
    fn xlfn_probe_xlevent_calculation_ended() -> i32;
    fn xlfn_probe_xlevent_calculation_canceled() -> i32;
    fn xlfn_probe_xlf_register() -> i32;
    fn xlfn_probe_xlf_unregister() -> i32;
    fn xlfn_probe_xlbit_dll_free() -> i32;
}

#[cfg(feature = "sdk-bindgen")]
fn get_rust_probe_values() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert("XLOPER12.size".into(), size_of::<XLOPER12>().to_string());
    map.insert("XLOPER12.align".into(), align_of::<XLOPER12>().to_string());
    map.insert(
        "XLOPER12.xltype.offset".into(),
        offset_of!(XLOPER12, xltype).to_string(),
    );
    map.insert(
        "XLOPER12Array.size".into(),
        size_of::<XLOPER12Array>().to_string(),
    );
    map.insert(
        "XLOPER12SRef.size".into(),
        size_of::<XLOPER12SRef>().to_string(),
    );
    map.insert("IDSHEET.size".into(), size_of::<IDSHEET>().to_string());
    map.insert(
        "XLOPER12MRef.size".into(),
        size_of::<XLOPER12MRef>().to_string(),
    );
    map.insert(
        "XLOPER12MRef.idSheet.offset".into(),
        offset_of!(XLOPER12MRef, sheet_id).to_string(),
    );
    map.insert(
        "XLOPER12Flow.size".into(),
        size_of::<XLOPER12Flow>().to_string(),
    );
    map.insert(
        "XLOPER12FlowValue.size".into(),
        size_of::<XLOPER12FlowValue>().to_string(),
    );
    map.insert(
        "XLOPER12Flow.rw.offset".into(),
        offset_of!(XLOPER12Flow, row).to_string(),
    );
    map.insert(
        "XLOPER12Flow.col.offset".into(),
        offset_of!(XLOPER12Flow, column).to_string(),
    );
    map.insert(
        "XLOPER12Flow.xlflow.offset".into(),
        offset_of!(XLOPER12Flow, flow).to_string(),
    );
    map.insert(
        "XLOPER12FlowValue.level.size".into(),
        size_of::<i32>().to_string(),
    );
    map.insert(
        "XLOPER12FlowValue.tbctrl.size".into(),
        size_of::<i32>().to_string(),
    );
    map.insert("XLREF12.size".into(), size_of::<XLREF12>().to_string());
    map.insert("xlAsyncReturn".into(), XL_ASYNC_RETURN.to_string());
    map.insert("xlEventRegister".into(), XL_EVENT_REGISTER.to_string());
    map.insert(
        "xleventCalculationEnded".into(),
        XLEVENT_CALCULATION_ENDED.to_string(),
    );
    map.insert(
        "xleventCalculationCanceled".into(),
        XLEVENT_CALCULATION_CANCELED.to_string(),
    );
    map.insert("xlfRegister".into(), XLF_REGISTER.to_string());
    map.insert("xlfUnregister".into(), XLF_UNREGISTER.to_string());
    map.insert("xlbitDLLFree".into(), XLBIT_DLL_FREE.to_string());
    map
}

#[cfg(feature = "sdk-bindgen")]
fn get_native_probe_values() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    // SAFETY: build.rs links these functions from native/probe.cpp, compiled
    // against the exact XLCALL.H selected by XLFN_SDK_INCLUDE.
    unsafe {
        map.insert(
            "XLOPER12.size".into(),
            xlfn_probe_xloper12_size().to_string(),
        );
        map.insert(
            "XLOPER12.align".into(),
            xlfn_probe_xloper12_align().to_string(),
        );
        map.insert(
            "XLOPER12.xltype.offset".into(),
            xlfn_probe_xloper12_xltype_offset().to_string(),
        );
        map.insert(
            "XLOPER12Array.size".into(),
            xlfn_probe_xloper12_array_size().to_string(),
        );
        map.insert(
            "XLOPER12SRef.size".into(),
            xlfn_probe_xloper12_sref_size().to_string(),
        );
        map.insert("IDSHEET.size".into(), xlfn_probe_idsheet_size().to_string());
        map.insert(
            "XLOPER12MRef.size".into(),
            xlfn_probe_xloper12_mref_size().to_string(),
        );
        map.insert(
            "XLOPER12MRef.idSheet.offset".into(),
            xlfn_probe_xloper12_mref_idsheet_offset().to_string(),
        );
        map.insert(
            "XLOPER12Flow.size".into(),
            xlfn_probe_xloper12_flow_size().to_string(),
        );
        map.insert(
            "XLOPER12FlowValue.size".into(),
            xlfn_probe_xloper12_flow_value_size().to_string(),
        );
        map.insert(
            "XLOPER12Flow.rw.offset".into(),
            xlfn_probe_xloper12_flow_row_offset().to_string(),
        );
        map.insert(
            "XLOPER12Flow.col.offset".into(),
            xlfn_probe_xloper12_flow_column_offset().to_string(),
        );
        map.insert(
            "XLOPER12Flow.xlflow.offset".into(),
            xlfn_probe_xloper12_flow_type_offset().to_string(),
        );
        map.insert(
            "XLOPER12FlowValue.level.size".into(),
            xlfn_probe_xloper12_flow_level_size().to_string(),
        );
        map.insert(
            "XLOPER12FlowValue.tbctrl.size".into(),
            xlfn_probe_xloper12_flow_toolbar_control_size().to_string(),
        );
        map.insert("XLREF12.size".into(), xlfn_probe_xlref12_size().to_string());
        map.insert(
            "xlAsyncReturn".into(),
            xlfn_probe_xl_async_return().to_string(),
        );
        map.insert(
            "xlEventRegister".into(),
            xlfn_probe_xl_event_register().to_string(),
        );
        map.insert(
            "xleventCalculationEnded".into(),
            xlfn_probe_xlevent_calculation_ended().to_string(),
        );
        map.insert(
            "xleventCalculationCanceled".into(),
            xlfn_probe_xlevent_calculation_canceled().to_string(),
        );
        map.insert("xlfRegister".into(), xlfn_probe_xlf_register().to_string());
        map.insert(
            "xlfUnregister".into(),
            xlfn_probe_xlf_unregister().to_string(),
        );
        map.insert(
            "xlbitDLLFree".into(),
            xlfn_probe_xlbit_dll_free().to_string(),
        );
    }
    map
}

#[cfg(feature = "sdk-bindgen")]
fn get_bindgen_probe_values() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert(
        "XLOPER12.size".into(),
        size_of::<sdk::XLOPER12>().to_string(),
    );
    map.insert(
        "XLOPER12.align".into(),
        align_of::<sdk::XLOPER12>().to_string(),
    );
    map.insert(
        "XLOPER12.xltype.offset".into(),
        offset_of!(sdk::XLOPER12, xltype).to_string(),
    );
    map.insert(
        "XLOPER12Array.size".into(),
        size_of::<sdk::XLOPER12Array>().to_string(),
    );
    map.insert(
        "XLOPER12SRef.size".into(),
        size_of::<sdk::XLOPER12SRef>().to_string(),
    );
    map.insert("XLREF12.size".into(), size_of::<sdk::XLREF12>().to_string());
    map.insert("IDSHEET.size".into(), size_of::<sdk::IDSHEET>().to_string());
    map.insert(
        "XLOPER12MRef.size".into(),
        size_of::<sdk::XLOPER12MRef>().to_string(),
    );
    map.insert(
        "XLOPER12MRef.idSheet.offset".into(),
        offset_of!(sdk::XLOPER12MRef, idSheet).to_string(),
    );
    map.insert(
        "XLOPER12Flow.size".into(),
        size_of::<sdk::XLOPER12Flow>().to_string(),
    );
    map.insert(
        "XLOPER12FlowValue.size".into(),
        size_of::<sdk::XLOPER12FlowValue>().to_string(),
    );
    map.insert(
        "XLOPER12FlowValue.level.size".into(),
        size_of::<i32>().to_string(),
    );
    map.insert(
        "XLOPER12FlowValue.tbctrl.size".into(),
        size_of::<i32>().to_string(),
    );
    map.insert(
        "XLOPER12Flow.rw.offset".into(),
        offset_of!(sdk::XLOPER12Flow, rw).to_string(),
    );
    map.insert(
        "XLOPER12Flow.col.offset".into(),
        offset_of!(sdk::XLOPER12Flow, col).to_string(),
    );
    map.insert(
        "XLOPER12Flow.xlflow.offset".into(),
        offset_of!(sdk::XLOPER12Flow, xlflow).to_string(),
    );
    map.insert("xlAsyncReturn".into(), sdk::xlAsyncReturn.to_string());
    map.insert("xlEventRegister".into(), sdk::xlEventRegister.to_string());
    map.insert(
        "xleventCalculationEnded".into(),
        sdk::xleventCalculationEnded.to_string(),
    );
    map.insert(
        "xleventCalculationCanceled".into(),
        sdk::xleventCalculationCanceled.to_string(),
    );
    map.insert("xlfRegister".into(), sdk::xlfRegister.to_string());
    map.insert("xlfUnregister".into(), sdk::xlfUnregister.to_string());
    map.insert("xlbitDLLFree".into(), sdk::xlbitDLLFree.to_string());
    map
}

#[cfg(feature = "sdk-bindgen")]
fn verify_callback_abi() {
    use core::ffi::c_void;
    use xlfn_sys::{XLRET_SUCCESS, XLTYPE_INT, excel12, install_callback_for_abi_probe};

    unsafe extern "system" {
        fn xlfn_callback_probe(
            function: i32,
            argument_count: i32,
            arguments: *mut *mut XLOPER12,
            result: *mut XLOPER12,
        ) -> i32;
    }

    // SAFETY: the linked C++ function is declared with XLCALL.H's exact
    // MdCallBack12-compatible prototype.
    unsafe {
        install_callback_for_abi_probe(xlfn_callback_probe as *const () as *mut c_void);
    }
    let mut argument = XLOPER12::integer(7);
    let arguments = [std::ptr::NonNull::from_mut(&mut argument)];
    // SAFETY: one live, non-null argument pointer is supplied to the linked
    // trampoline for the duration of the call.
    let (status, result) = unsafe { excel12(0x1234, &arguments) };
    // SAFETY: the trampoline sets xltypeInt before writing the integer member.
    let value = unsafe { result.value.integer };
    assert_eq!(status, XLRET_SUCCESS, "MdCallBack12 returned {status}");
    assert_eq!(result.base_type(), XLTYPE_INT);
    assert_eq!(value, 0x1234);
}

#[cfg(any(feature = "sdk-bindgen", test))]
fn compare_probe_values(
    reference_name: &str,
    rust_map: &BTreeMap<String, String>,
    reference_map: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for (key, rust_value) in rust_map {
        match reference_map.get(key) {
            Some(reference_value) if rust_value == reference_value => {}
            Some(reference_value) => errors.push(format!(
                "{key}: {reference_name}={reference_value}, Rust={rust_value}"
            )),
            None => errors.push(format!("missing {reference_name} probe value for {key}")),
        }
    }
    for key in reference_map.keys() {
        if !rust_map.contains_key(key) {
            errors.push(format!("missing Rust probe value for {key}"));
        }
    }
    errors
}

#[cfg(feature = "sdk-bindgen")]
fn run_probe() {
    verify_callback_abi();
    let rust_map = get_rust_probe_values();
    let native_map = get_native_probe_values();
    let native_errors = compare_probe_values("C++", &rust_map, &native_map);
    assert!(
        native_errors.is_empty(),
        "Rust and C++ ABI definitions differ: {native_errors:?}"
    );

    let bindgen_map = get_bindgen_probe_values();
    let bindgen_errors = compare_probe_values("bindgen", &rust_map, &bindgen_map);
    assert!(
        bindgen_errors.is_empty(),
        "Rust and XLCALL.H bindgen definitions differ: {bindgen_errors:?}"
    );
}

#[cfg(feature = "sdk-bindgen")]
fn main() {
    run_probe();
    println!("Excel ABI probe passed");
}

#[cfg(not(feature = "sdk-bindgen"))]
fn main() {
    eprintln!("excel-abi-probe requires --features sdk-bindgen and XLFN_SDK_INCLUDE");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_comparison_reports_mismatches_and_missing_values() {
        let rust = BTreeMap::from([("size".into(), "32".into()), ("extra".into(), "4".into())]);
        let reference =
            BTreeMap::from([("size".into(), "16".into()), ("align".into(), "8".into())]);
        let errors = compare_probe_values("reference", &rust, &reference);
        assert_eq!(errors.len(), 3);
        assert!(errors.iter().any(|error| error.contains("reference=16")));
        assert!(errors.iter().any(|error| error.contains("missing Rust")));
        assert!(
            errors
                .iter()
                .any(|error| error.contains("missing reference probe value for extra"))
        );
    }

    #[cfg(feature = "sdk-bindgen")]
    #[test]
    fn sdk_layout_and_callback_abi_are_verified_by_the_test_harness() {
        run_probe();
    }

    #[cfg(not(feature = "sdk-bindgen"))]
    #[test]
    #[ignore = "requires the Excel SDK header and native toolchain"]
    fn sdk_probe_requires_the_real_header_and_native_probe() {
        panic!("run this package with --features sdk-bindgen and XLFN_SDK_INCLUDE");
    }
}
