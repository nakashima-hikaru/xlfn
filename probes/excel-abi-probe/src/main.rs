use std::collections::BTreeMap;
use std::mem::{align_of, offset_of, size_of};
use std::process::Command;
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
    map.insert("XLREF12.size".into(), size_of::<sdk::XLREF12>().to_string());
    map.insert("IDSHEET.size".into(), size_of::<sdk::IDSHEET>().to_string());
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
    let arguments = [&mut argument as *mut XLOPER12];
    // SAFETY: one live argument pointer is supplied to the linked trampoline.
    let (status, result) = unsafe { excel12(0x1234, &arguments) };
    // SAFETY: the trampoline sets xltypeInt before writing the integer member.
    let value = unsafe { result.value.integer };
    if status != XLRET_SUCCESS || result.base_type() != XLTYPE_INT || value != 0x1234 {
        eprintln!(
            "ABI PROBE ERROR: MdCallBack12 trampoline mismatch \
             (status={status}, type={}, value={value})",
            result.base_type()
        );
        std::process::exit(1);
    }
    println!("MdCallBack12 ABI Verification: C++ trampoline call succeeded!");
}

fn compare_probe_values(
    reference_name: &str,
    rust_map: &BTreeMap<String, String>,
    reference_map: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for (key, reference_value) in reference_map {
        match rust_map.get(key) {
            Some(rust_value) if rust_value == reference_value => {}
            Some(rust_value) => errors.push(format!(
                "{key}: {reference_name}={reference_value}, Rust={rust_value}"
            )),
            None => errors.push(format!("missing Rust probe value for {key}")),
        }
    }
    errors
}

fn main() {
    #[cfg(feature = "sdk-bindgen")]
    verify_callback_abi();

    let rust_map = get_rust_probe_values();
    for (k, v) in &rust_map {
        println!("{k}={v}");
    }

    let native_probe = std::env::var_os("XLFN_CPP_PROBE")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(|| {
            eprintln!("ABI PROBE ERROR: XLFN_CPP_PROBE must name a built C++ probe");
            std::process::exit(1);
        });
    println!(
        "--- Comparing against native C++ probe at {} ---",
        native_probe.display()
    );
    let output = Command::new(native_probe)
        .output()
        .expect("failed to execute native C++ probe");
    if !output.status.success() {
        eprintln!("ABI PROBE ERROR: native C++ probe failed");
        std::process::exit(1);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut cpp_map = BTreeMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            cpp_map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    if cpp_map.is_empty() {
        eprintln!("ABI PROBE ERROR: native C++ probe produced no comparable values");
        std::process::exit(1);
    }

    let errors = compare_probe_values("C++", &rust_map, &cpp_map);
    if errors.is_empty() {
        println!("ABI Probe Verification: C++ and Rust definitions match 100%!");
    } else {
        for error in errors {
            eprintln!("ABI PROBE ERROR: {error}");
        }
        std::process::exit(1);
    }

    #[cfg(feature = "sdk-bindgen")]
    {
        let bindgen_map = get_bindgen_probe_values();
        if bindgen_map.is_empty() {
            eprintln!("ABI PROBE ERROR: bindgen produced no comparable SDK values");
            std::process::exit(1);
        }
        let errors = compare_probe_values("bindgen", &rust_map, &bindgen_map);
        if errors.is_empty() {
            println!("Bindgen Verification: XLCALL.H and xlfn-sys match!");
        } else {
            for error in errors {
                eprintln!("ABI PROBE ERROR: {error}");
            }
            std::process::exit(1);
        }
    }
    #[cfg(not(feature = "sdk-bindgen"))]
    {
        eprintln!("ABI PROBE ERROR: sdk-bindgen is required");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_comparison_reports_mismatches_and_missing_values() {
        let rust = BTreeMap::from([("size".into(), "32".into())]);
        let reference =
            BTreeMap::from([("size".into(), "16".into()), ("align".into(), "8".into())]);
        let errors = compare_probe_values("reference", &rust, &reference);
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().any(|error| error.contains("reference=16")));
        assert!(errors.iter().any(|error| error.contains("missing Rust")));
    }
}
