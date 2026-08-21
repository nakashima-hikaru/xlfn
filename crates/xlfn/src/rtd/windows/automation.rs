use super::com_abi::IUnknown_Vtbl;
use super::update_event::{OwnedRtdUpdateEvent, RtdUpdateEvent};
use super::{
    IID_IRTD_UPDATE_EVENT, RtdServer, com_boundary, connect_data, disconnect_data, guid_eq,
    heartbeat, refresh_data, server_start, server_terminate,
};
use crate::subscription::{RtdUpdate, StoredRtdValue};
use crate::win32::{
    DISP_E_BADPARAMCOUNT, DISP_E_MEMBERNOTFOUND, DISP_E_PARAMNOTFOUND, DISP_E_TYPEMISMATCH,
    DISP_E_UNKNOWNINTERFACE, DISP_E_UNKNOWNNAME, DISPATCH_METHOD, DISPID_UNKNOWN, DISPPARAMS,
    E_FAIL, E_INVALIDARG, E_OUTOFMEMORY, E_POINTER, EXCEPINFO, GUID, S_OK, SAFEARRAY,
    SAFEARRAYBOUND, SafeArrayCreate, SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement,
    SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayGetVartype, SafeArrayPutElement,
    SysAllocStringLen, SysFreeString, SysStringLen, VARIANT, VARIANT_BOOL, VT_ARRAY, VT_BOOL,
    VT_BSTR, VT_BYREF, VT_DISPATCH, VT_EMPTY, VT_ERROR, VT_I4, VT_R8, VT_UNKNOWN, VT_VARIANT,
    VariantClear,
};
use crate::{InputError, XllError, XllResult};
use std::ptr::{self, NonNull};

pub(super) const IID_NULL: super::GUID = super::GUID::from_u128(0);

pub(super) const DISPID_SERVER_START: i32 = 10;
pub(super) const DISPID_CONNECT_DATA: i32 = 11;
pub(super) const DISPID_REFRESH_DATA: i32 = 12;
pub(super) const DISPID_DISCONNECT_DATA: i32 = 13;
pub(super) const DISPID_HEARTBEAT: i32 = 14;
pub(super) const DISPID_SERVER_TERMINATE: i32 = 15;

pub(super) unsafe extern "system" fn server_get_ids_of_names(
    this: *mut RtdServer,
    riid: *const GUID,
    names: *const *const u16,
    count: u32,
    _locale: u32,
    ids: *mut i32,
) -> i32 {
    com_boundary("IDispatch::GetIDsOfNames", || {
        // SAFETY: raw COM arguments are validated before the inner function
        // dereferences them.
        unsafe { server_get_ids_of_names_inner(this, riid, names, count, ids) }
    })
}

unsafe fn server_get_ids_of_names_inner(
    this: *mut RtdServer,
    riid: *const GUID,
    names: *const *const u16,
    count: u32,
    ids: *mut i32,
) -> i32 {
    if this.is_null() || riid.is_null() || names.is_null() || ids.is_null() {
        return E_POINTER;
    }

    if count == 0 {
        return E_INVALIDARG;
    }

    // SAFETY: `riid` was validated as non-null and points to the IID supplied
    // by COM for this call.
    if !guid_eq(unsafe { *riid }, IID_NULL) {
        return DISP_E_UNKNOWNINTERFACE;
    }

    // Initialize every result before inspecting the supplied names so partial
    // name-resolution failure never leaves stale DISPIDs behind.
    for index in 0..count as usize {
        // SAFETY: COM guarantees `ids` points to `count` writable elements.
        unsafe { *ids.add(index) = DISPID_UNKNOWN };
    }

    // SAFETY: `names` points to `count` readable name pointers.
    let member = unsafe { dispatch_member_id(*names) };
    // SAFETY: `ids` points to at least one writable element.
    unsafe { *ids = member.unwrap_or(DISPID_UNKNOWN) };

    let mut all_known = member.is_some();
    if let Some(member) = member {
        for index in 1..count as usize {
            // SAFETY: `names` and `ids` each point to `count` elements. Name
            // matching reads only the expected ASCII name length plus its NUL.
            let parameter = unsafe { dispatch_parameter_id(member, *names.add(index)) };
            // SAFETY: `ids` points to `count` writable elements.
            unsafe { *ids.add(index) = parameter.unwrap_or(DISPID_UNKNOWN) };
            all_known &= parameter.is_some();
        }
    }

    if all_known { S_OK } else { DISP_E_UNKNOWNNAME }
}

pub(super) unsafe extern "system" fn server_invoke(
    this: *mut RtdServer,
    id: i32,
    riid: *const GUID,
    _locale: u32,
    flags: u16,
    parameters: *mut DISPPARAMS,
    result: *mut VARIANT,
    exception: *mut EXCEPINFO,
    argument_error: *mut u32,
) -> i32 {
    com_boundary("IDispatch::Invoke", || {
        // SAFETY: raw COM arguments are validated before the inner function
        // dereferences them.
        unsafe {
            server_invoke_inner(
                this,
                id,
                riid,
                flags,
                parameters,
                result,
                exception,
                argument_error,
            )
        }
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "COM IDispatch::Invoke ABI signature"
)]
unsafe fn server_invoke_inner(
    this: *mut RtdServer,
    id: i32,
    riid: *const GUID,
    flags: u16,
    parameters: *mut DISPPARAMS,
    result: *mut VARIANT,
    exception: *mut EXCEPINFO,
    argument_error: *mut u32,
) -> i32 {
    if !result.is_null() {
        // SAFETY: COM supplied an optional writable VARIANT result. The caller
        // initializes it before Invoke and transfers the empty slot to us.
        unsafe { ptr::write(result, VARIANT::default()) };
    }
    if !exception.is_null() {
        // SAFETY: COM supplied optional writable exception storage.
        unsafe { ptr::write(exception, EXCEPINFO::default()) };
    }
    if !argument_error.is_null() {
        // SAFETY: COM supplied optional writable argument-index storage.
        unsafe { *argument_error = 0 };
    }

    if this.is_null() || riid.is_null() || parameters.is_null() {
        return E_POINTER;
    }

    // SAFETY: `riid` was validated as non-null and points to a readable GUID.
    if !guid_eq(unsafe { *riid }, IID_NULL) {
        return DISP_E_UNKNOWNINTERFACE;
    }

    if flags & DISPATCH_METHOD == 0 {
        return DISP_E_MEMBERNOTFOUND;
    }

    let Some(expected_arguments) = dispatch_argument_count(id) else {
        return DISP_E_MEMBERNOTFOUND;
    };

    // SAFETY: `parameters` was validated as non-null and COM keeps the
    // DISPPARAMS and its argument arrays live throughout Invoke.
    let parameters = unsafe { &mut *parameters };
    // SAFETY: the DISPPARAMS pointer arrays are validated by the helper before
    // they are dereferenced.
    let arguments =
        match unsafe { collect_dispatch_arguments(parameters, expected_arguments, argument_error) }
        {
            Ok(arguments) => arguments,
            Err(status) => return status,
        };

    match id {
        DISPID_SERVER_START => {
            // SAFETY: argument zero was obtained from the validated DISPPARAMS.
            let Some(callback) = (unsafe { query_rtd_update_event(arguments[0]) }) else {
                // SAFETY: argument zero belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[0], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };

            let mut value = 0;
            // SAFETY: `callback` owns one queried IRTDUpdateEvent reference for
            // this call and `value` is writable.
            let status = unsafe { server_start(this, callback.as_ptr().cast(), &mut value) };

            if status == S_OK {
                // SAFETY: the optional result was initialized above.
                unsafe { write_i4_dispatch_result(result, value) };
            }
            status
        }
        DISPID_CONNECT_DATA => {
            // SAFETY: all argument pointers were obtained from validated
            // DISPPARAMS storage.
            let Some(topic_id) = (unsafe { dispatch_i4_value(arguments[0]) }) else {
                // SAFETY: argument zero belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[0], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };
            // SAFETY: see above.
            let Some(strings) = (unsafe { dispatch_array_argument(arguments[1]) }) else {
                // SAFETY: argument one belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[1], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };
            // SAFETY: see above.
            let Some(new_values) = (unsafe { dispatch_bool_reference(arguments[2]) }) else {
                // SAFETY: argument two belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[2], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };

            let mut by_value_array;
            let strings = match strings {
                DispatchArrayArgument::ByReference(strings) => strings.as_ptr(),
                DispatchArrayArgument::ByValue(strings) => {
                    by_value_array = strings.as_ptr();
                    &mut by_value_array
                }
            };
            let mut ignored_result = VARIANT::default();
            let result_slot = if result.is_null() {
                &mut ignored_result
            } else {
                result
            };

            // SAFETY: each typed argument was validated above and remains live
            // for the duration of the vtable call.
            let status =
                unsafe { connect_data(this, topic_id, strings, new_values.as_ptr(), result_slot) };

            if result.is_null() {
                // SAFETY: connect_data either left the initialized local empty
                // or populated it with a valid owned VARIANT.
                unsafe { VariantClear(&mut ignored_result) };
            }
            status
        }
        DISPID_REFRESH_DATA => {
            // SAFETY: argument zero was obtained from validated DISPPARAMS.
            let Some(topic_count) = (unsafe { dispatch_i4_reference(arguments[0]) }) else {
                // SAFETY: argument zero belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[0], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };

            let mut array = ptr::null_mut();
            // SAFETY: `topic_count` and `array` are writable for the call.
            let status = unsafe { refresh_data(this, topic_count.as_ptr(), &mut array) };
            if status == S_OK && !result.is_null() {
                // SAFETY: the optional result was initialized above and takes
                // ownership of the returned SAFEARRAY.
                unsafe { write_array_dispatch_result(result, array) };
            } else if !array.is_null() {
                // SAFETY: no caller-visible VARIANT took ownership of the
                // SAFEARRAY, so this branch destroys it exactly once.
                unsafe { SafeArrayDestroy(array) };
            }
            status
        }
        DISPID_DISCONNECT_DATA => {
            // SAFETY: argument zero was obtained from validated DISPPARAMS.
            let Some(topic_id) = (unsafe { dispatch_i4_value(arguments[0]) }) else {
                // SAFETY: argument zero belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[0], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };

            // SAFETY: the scalar argument was validated above.
            unsafe { disconnect_data(this, topic_id) }
        }
        DISPID_HEARTBEAT => {
            let mut value = 0;
            // SAFETY: `value` is writable for the typed RTD method.
            let status = unsafe { heartbeat(this, &mut value) };
            if status == S_OK {
                // SAFETY: the optional result was initialized above.
                unsafe { write_i4_dispatch_result(result, value) };
            }
            status
        }
        DISPID_SERVER_TERMINATE => {
            // SAFETY: `this` was validated above and COM holds it live for
            // Invoke.
            unsafe { server_terminate(this) }
        }
        _ => DISP_E_MEMBERNOTFOUND,
    }
}

fn dispatch_argument_count(id: i32) -> Option<usize> {
    match id {
        DISPID_SERVER_START | DISPID_REFRESH_DATA | DISPID_DISCONNECT_DATA => Some(1),
        DISPID_CONNECT_DATA => Some(3),
        DISPID_HEARTBEAT | DISPID_SERVER_TERMINATE => Some(0),
        _ => None,
    }
}

unsafe fn collect_dispatch_arguments(
    parameters: &mut DISPPARAMS,
    expected: usize,
    argument_error: *mut u32,
) -> Result<[*mut VARIANT; 3], i32> {
    let argument_count = parameters.cArgs as usize;
    let named_count = parameters.cNamedArgs as usize;

    if argument_count != expected || named_count > argument_count {
        return Err(DISP_E_BADPARAMCOUNT);
    }
    if expected != 0 && parameters.rgvarg.is_null() {
        return Err(E_POINTER);
    }
    if named_count != 0 && parameters.rgdispidNamedArgs.is_null() {
        return Err(E_POINTER);
    }

    let mut arguments: [*mut VARIANT; 3] = [ptr::null_mut(); 3];

    for named_index in 0..named_count {
        // SAFETY: both arrays contain at least `named_count` elements.
        let parameter_id = unsafe { *parameters.rgdispidNamedArgs.add(named_index) };
        let Ok(parameter_index) = usize::try_from(parameter_id) else {
            if !argument_error.is_null() {
                // SAFETY: `argument_error` is an optional writable output.
                unsafe { *argument_error = named_index as u32 };
            }
            return Err(DISP_E_PARAMNOTFOUND);
        };

        if parameter_index >= expected || !arguments[parameter_index].is_null() {
            if !argument_error.is_null() {
                // SAFETY: `argument_error` is an optional writable output.
                unsafe { *argument_error = named_index as u32 };
            }
            return Err(DISP_E_PARAMNOTFOUND);
        }

        // SAFETY: rgvarg contains `argument_count` elements and named
        // arguments occupy its first `named_count` entries.
        arguments[parameter_index] = unsafe { parameters.rgvarg.add(named_index) };
    }

    let mut value_index = named_count;
    for parameter_index in (0..expected).rev() {
        if arguments[parameter_index].is_null() {
            // SAFETY: exact argument-count validation and the named-argument
            // checks ensure each remaining positional element is consumed once.
            arguments[parameter_index] = unsafe { parameters.rgvarg.add(value_index) };
            value_index += 1;
        }
    }

    Ok(arguments)
}

unsafe fn set_dispatch_argument_error(
    parameters: &DISPPARAMS,
    argument: *mut VARIANT,
    argument_error: *mut u32,
) {
    if argument_error.is_null() {
        return;
    }

    // SAFETY: `argument` was selected from `parameters.rgvarg`, so both
    // pointers belong to the same allocation and offset_from is defined.
    let index = unsafe { argument.offset_from(parameters.rgvarg) };
    // SAFETY: the optional output is writable. A valid DISPPARAMS index is
    // non-negative and fits in u32 because cArgs itself is u32.
    unsafe { *argument_error = index as u32 };
}

unsafe fn dispatch_i4_value(argument: *mut VARIANT) -> Option<i32> {
    // SAFETY: the caller supplies a readable argument VARIANT.
    let argument = unsafe { unwrap_dispatch_variant(argument)? };
    // SAFETY: `argument` is a readable VARIANT and the discriminant is checked
    // before selecting the corresponding union field.
    if unsafe { (*argument.as_ptr()).Anonymous.Anonymous.vt } == VT_I4 {
        // SAFETY: the checked discriminant selects lVal.
        Some(unsafe { (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.lVal })
    } else {
        None
    }
}

unsafe fn dispatch_i4_reference(argument: *mut VARIANT) -> Option<NonNull<i32>> {
    // SAFETY: the caller supplies a readable argument VARIANT.
    let argument = unsafe { unwrap_dispatch_variant(argument)? };
    // SAFETY: the discriminant is checked before selecting the by-reference
    // union field.
    if unsafe { (*argument.as_ptr()).Anonymous.Anonymous.vt } == (VT_BYREF | VT_I4) {
        // SAFETY: the checked discriminant selects plVal.
        NonNull::new(unsafe { (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.plVal })
    } else {
        None
    }
}

unsafe fn dispatch_bool_reference(argument: *mut VARIANT) -> Option<NonNull<VARIANT_BOOL>> {
    // SAFETY: the caller supplies a readable argument VARIANT.
    let argument = unsafe { unwrap_dispatch_variant(argument)? };
    // SAFETY: the discriminant is checked before selecting the by-reference
    // union field.
    if unsafe { (*argument.as_ptr()).Anonymous.Anonymous.vt } == (VT_BYREF | VT_BOOL) {
        // SAFETY: the checked discriminant selects pboolVal.
        NonNull::new(unsafe { (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.pboolVal })
    } else {
        None
    }
}

enum DispatchArrayArgument {
    ByValue(NonNull<SAFEARRAY>),
    ByReference(NonNull<*mut SAFEARRAY>),
}

unsafe fn dispatch_array_argument(argument: *mut VARIANT) -> Option<DispatchArrayArgument> {
    // SAFETY: the caller supplies a readable argument VARIANT.
    let argument = unsafe { unwrap_dispatch_variant(argument)? };
    // SAFETY: `argument` is readable for the discriminant and matching union
    // field selected below.
    let variant_type = unsafe { (*argument.as_ptr()).Anonymous.Anonymous.vt };
    match variant_type {
        value if value == (VT_ARRAY | VT_VARIANT) => {
            // SAFETY: the checked discriminant selects parray.
            let array = unsafe { (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.parray };
            NonNull::new(array).map(DispatchArrayArgument::ByValue)
        }
        value if value == (VT_BYREF | VT_ARRAY | VT_VARIANT) => {
            // SAFETY: the checked discriminant selects pparray.
            let array = unsafe { (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.pparray };
            NonNull::new(array).map(DispatchArrayArgument::ByReference)
        }
        _ => None,
    }
}

unsafe fn query_rtd_update_event(argument: *mut VARIANT) -> Option<OwnedRtdUpdateEvent> {
    // SAFETY: the caller supplies a readable argument VARIANT.
    let argument = unsafe { unwrap_dispatch_variant(argument)? };
    // SAFETY: the discriminant is checked before reading its matching interface
    // pointer field.
    let variant_type = unsafe { (*argument.as_ptr()).Anonymous.Anonymous.vt };
    // SAFETY: each match arm reads only the union field selected by the checked
    // discriminant; by-reference arms also reject null before dereferencing.
    let unknown = unsafe {
        match variant_type {
            VT_DISPATCH => (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.pdispVal,
            VT_UNKNOWN => (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.punkVal,
            value if value == (VT_BYREF | VT_DISPATCH) => {
                let pointer = (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.ppdispVal;
                NonNull::new(pointer).map(|pointer| *pointer.as_ptr())?
            }
            value if value == (VT_BYREF | VT_UNKNOWN) => {
                let pointer = (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.ppunkVal;
                NonNull::new(pointer).map(|pointer| *pointer.as_ptr())?
            }
            _ => return None,
        }
    };
    let unknown = NonNull::new(unknown)?;

    // SAFETY: every COM interface begins with a readable IUnknown vtable
    // pointer, and `unknown` was obtained from a matching VARIANT arm.
    let vtable = unsafe { *unknown.as_ptr().cast::<*const IUnknown_Vtbl>() };
    let vtable = NonNull::new(vtable as *mut IUnknown_Vtbl)?;
    let mut output = ptr::null_mut();

    // SAFETY: the queried object is live for the VARIANT argument's lifetime,
    // the IID is readable, and `output` is writable.
    let status = unsafe {
        ((*vtable.as_ptr()).QueryInterface)(unknown.as_ptr(), &IID_IRTD_UPDATE_EVENT, &mut output)
    };

    if status == S_OK {
        NonNull::new(output.cast::<RtdUpdateEvent>()).map(|pointer| {
            // SAFETY: QueryInterface returned S_OK and one owned
            // IRTDUpdateEvent reference in `output`.
            unsafe { OwnedRtdUpdateEvent::from_raw(pointer) }
        })
    } else {
        None
    }
}

pub(super) unsafe fn unwrap_dispatch_variant(argument: *mut VARIANT) -> Option<NonNull<VARIANT>> {
    let argument = NonNull::new(argument)?;
    // SAFETY: the caller supplies a readable argument VARIANT.
    if unsafe { (*argument.as_ptr()).Anonymous.Anonymous.vt } != (VT_BYREF | VT_VARIANT) {
        return Some(argument);
    }

    // SAFETY: the checked discriminant selects pvarVal.
    let referenced =
        NonNull::new(unsafe { (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.pvarVal })?;

    // Automation permits only one level of VT_BYREF | VT_VARIANT indirection.
    // Reject malformed nested references instead of following arbitrary
    // pointer chains.
    // SAFETY: a valid VT_BYREF | VT_VARIANT argument points to a readable
    // referenced VARIANT.
    if unsafe { (*referenced.as_ptr()).Anonymous.Anonymous.vt } == (VT_BYREF | VT_VARIANT) {
        return None;
    }

    Some(referenced)
}

unsafe fn write_i4_dispatch_result(result: *mut VARIANT, value: i32) {
    if result.is_null() {
        return;
    }

    // SAFETY: the optional result points to initialized writable VARIANT
    // storage and the discriminant matches the selected union field.
    unsafe {
        (*result).Anonymous.Anonymous.vt = VT_I4;
        (*result).Anonymous.Anonymous.Anonymous.lVal = value;
    }
}

unsafe fn write_array_dispatch_result(result: *mut VARIANT, array: *mut SAFEARRAY) {
    // SAFETY: the caller passes initialized writable VARIANT storage and
    // transfers ownership of `array` to it.
    unsafe {
        (*result).Anonymous.Anonymous.vt = VT_ARRAY | VT_VARIANT;
        (*result).Anonymous.Anonymous.Anonymous.parray = array;
    }
}

unsafe fn dispatch_member_id(name: *const u16) -> Option<i32> {
    // SAFETY: GetIDsOfNames supplies a NUL-terminated OLE string. Each helper
    // reads no further than its fixed ASCII candidate plus the terminator.
    unsafe {
        if wide_name_eq_ascii(name, b"ServerStart") {
            Some(DISPID_SERVER_START)
        } else if wide_name_eq_ascii(name, b"ConnectData") {
            Some(DISPID_CONNECT_DATA)
        } else if wide_name_eq_ascii(name, b"RefreshData") {
            Some(DISPID_REFRESH_DATA)
        } else if wide_name_eq_ascii(name, b"DisconnectData") {
            Some(DISPID_DISCONNECT_DATA)
        } else if wide_name_eq_ascii(name, b"Heartbeat") {
            Some(DISPID_HEARTBEAT)
        } else if wide_name_eq_ascii(name, b"ServerTerminate") {
            Some(DISPID_SERVER_TERMINATE)
        } else {
            None
        }
    }
}

unsafe fn dispatch_parameter_id(member: i32, name: *const u16) -> Option<i32> {
    // SAFETY: GetIDsOfNames supplies a NUL-terminated OLE string and each
    // comparison reads only its fixed candidate length plus the terminator.
    unsafe {
        match member {
            DISPID_SERVER_START => wide_name_eq_ascii(name, b"CallbackObject").then_some(0),
            DISPID_CONNECT_DATA => {
                if wide_name_eq_ascii(name, b"TopicID") {
                    Some(0)
                } else if wide_name_eq_ascii(name, b"Strings") {
                    Some(1)
                } else if wide_name_eq_ascii(name, b"GetNewValues") {
                    Some(2)
                } else {
                    None
                }
            }
            DISPID_REFRESH_DATA => wide_name_eq_ascii(name, b"TopicCount").then_some(0),
            DISPID_DISCONNECT_DATA => wide_name_eq_ascii(name, b"TopicID").then_some(0),
            DISPID_HEARTBEAT | DISPID_SERVER_TERMINATE => None,
            _ => None,
        }
    }
}

unsafe fn wide_name_eq_ascii(name: *const u16, expected: &[u8]) -> bool {
    if name.is_null() {
        return false;
    }

    for (index, expected) in expected.iter().copied().enumerate() {
        // SAFETY: the GetIDsOfNames contract supplies a readable NUL-terminated
        // string. This loop reads only the fixed expected-name prefix.
        let actual = unsafe { *name.add(index) };
        let folded = if (u16::from(b'A')..=u16::from(b'Z')).contains(&actual) {
            actual + u16::from(b'a' - b'A')
        } else {
            actual
        };
        if folded != u16::from(expected.to_ascii_lowercase()) {
            return false;
        }
    }

    // SAFETY: the caller-supplied name is NUL-terminated; checking this one
    // element establishes that it has no unmatched suffix.
    unsafe { *name.add(expected.len()) == 0 }
}
pub(super) unsafe fn write_bstr_variant(result: *mut VARIANT, value: &str) -> i32 {
    let wide =
        match crate::utf16::encode_bounded(value, "RTD value", crate::utf16::EXCEL_STRING_LIMIT) {
            Ok(wide) => wide,
            Err(_) => return E_INVALIDARG,
        };

    let length = match u32::try_from(wide.len()) {
        Ok(length) => length,
        Err(_) => return E_INVALIDARG,
    };

    // SAFETY: `wide` is readable for exactly `length` UTF-16 code units and
    // remains live for the duration of SysAllocStringLen.
    let bstr = unsafe { SysAllocStringLen(wide.as_ptr(), length) };

    if bstr.is_null() {
        return E_FAIL;
    }

    let mut variant = VARIANT::default();
    variant.Anonymous.Anonymous.vt = VT_BSTR;
    variant.Anonymous.Anonymous.Anonymous.bstrVal = bstr;

    // SAFETY: the caller guarantees that `result` points to writable VARIANT
    // output storage. Ownership of `bstr` transfers into the written VARIANT.
    unsafe { *result = variant };

    S_OK
}

pub(super) unsafe fn write_value_variant(result: *mut VARIANT, value: &StoredRtdValue) -> i32 {
    if result.is_null() {
        return E_POINTER;
    }

    let mut variant = VARIANT::default();

    match value {
        StoredRtdValue::Number(value) => {
            variant.Anonymous.Anonymous.vt = VT_R8;
            variant.Anonymous.Anonymous.Anonymous.dblVal = *value;
        }
        StoredRtdValue::Boolean(value) => {
            variant.Anonymous.Anonymous.vt = VT_BOOL;
            variant.Anonymous.Anonymous.Anonymous.boolVal = if *value { -1 } else { 0 };
        }
        StoredRtdValue::Integer(value) => {
            variant.Anonymous.Anonymous.vt = VT_I4;
            variant.Anonymous.Anonymous.Anonymous.lVal = *value;
        }
        StoredRtdValue::String(value) => {
            // SAFETY: `result` was validated as non-null above and `value`
            // remains readable for the duration of the call.
            return unsafe { write_bstr_variant(result, &**value) };
        }
        StoredRtdValue::Error(value) => {
            variant.Anonymous.Anonymous.vt = VT_ERROR;
            variant.Anonymous.Anonymous.Anonymous.scode = 2000 + value.0.code();
        }
        StoredRtdValue::Empty => {
            variant.Anonymous.Anonymous.vt = VT_EMPTY;
        }
    }

    // SAFETY: `result` was validated as non-null and points to writable VARIANT
    // storage supplied by the caller.
    unsafe { *result = variant };

    S_OK
}

pub(super) unsafe fn write_refresh_data(
    topic_count: *mut i32,
    result: *mut *mut SAFEARRAY,
    updates: &[RtdUpdate],
) -> i32 {
    // SAFETY: the caller validated both output pointers as non-null and writable.
    unsafe {
        *topic_count = 0;
        *result = ptr::null_mut();
    }

    if updates.is_empty() {
        return S_OK;
    }

    let count = match u32::try_from(updates.len()) {
        Ok(count) => count,
        Err(_) => return E_INVALIDARG,
    };

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

    // SAFETY: `bounds` describes a valid two-dimensional SAFEARRAY of VARIANTs
    // and remains readable for the duration of SafeArrayCreate.
    let array = unsafe { SafeArrayCreate(VT_VARIANT, 2, bounds.as_mut_ptr()) };

    if array.is_null() {
        return E_OUTOFMEMORY;
    }

    for (column, update) in updates.iter().enumerate() {
        let Ok(column) = i32::try_from(column) else {
            // SAFETY: `array` was allocated above, has not been transferred to
            // Excel, and is destroyed exactly once on this error path.
            unsafe { SafeArrayDestroy(array) };
            return E_INVALIDARG;
        };

        let mut topic = VARIANT::default();
        topic.Anonymous.Anonymous.vt = VT_I4;
        topic.Anonymous.Anonymous.Anonymous.lVal = update.topic_id;

        let mut value_variant = VARIANT::default();

        // SAFETY: `value_variant` is initialized writable VARIANT storage owned
        // by this stack frame and `update.value` remains readable.
        let value_status = unsafe { write_value_variant(&mut value_variant, &update.value) };

        if value_status != S_OK {
            // SAFETY: `value_variant` is initialized and locally owned.
            // `array` has not been transferred to Excel and is destroyed once.
            unsafe {
                VariantClear(&mut value_variant);
                SafeArrayDestroy(array);
            }

            return value_status;
        }

        // The SAFEARRAY consists of two rows and one column per RTD update.
        // The first row stores topic IDs and the second row stores values.
        let mut topic_index = [0, column];
        let mut value_index = [1, column];

        // SAFETY: `topic_index` is within the declared two-dimensional bounds,
        // and `topic` points to a readable initialized VARIANT.
        let topic_status = unsafe {
            SafeArrayPutElement(
                array,
                topic_index.as_mut_ptr(),
                (&mut topic as *mut VARIANT).cast(),
            )
        };

        // SAFETY: `value_index` is within the declared two-dimensional bounds,
        // and `value_variant` points to a readable initialized VARIANT.
        let value_status = unsafe {
            SafeArrayPutElement(
                array,
                value_index.as_mut_ptr(),
                (&mut value_variant as *mut VARIANT).cast(),
            )
        };

        // SAFETY: SafeArrayPutElement copies VARIANT payloads. The local
        // VARIANTs remain owned here and must be cleared exactly once.
        unsafe {
            VariantClear(&mut topic);
            VariantClear(&mut value_variant);
        }

        if topic_status < 0 {
            // SAFETY: array has not been transferred to Excel.
            unsafe { SafeArrayDestroy(array) };
            return topic_status;
        }

        if value_status < 0 {
            // SAFETY: array has not been transferred to Excel.
            unsafe { SafeArrayDestroy(array) };
            return value_status;
        }
    }

    // SAFETY: both output pointers were validated as writable. Ownership of the
    // fully initialized SAFEARRAY transfers to Excel through `result`.
    unsafe {
        *topic_count = updates.len() as i32;
        *result = array;
    }

    S_OK
}

pub(super) const MAX_RTD_TOPIC_PARTS: usize = 253;
const REQUIRED_RTD_TOPIC_PARTS: usize = 1;

pub(super) fn checked_topic_part_count(lower: i32, upper: i32) -> XllResult<usize> {
    if upper < lower {
        return Err(XllError::InvalidHandle);
    }
    let count = i64::from(upper)
        .checked_sub(i64::from(lower))
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| {
            XllError::input(
                "RTD topic",
                InputError::TooLarge {
                    limit: MAX_RTD_TOPIC_PARTS,
                    actual: usize::MAX,
                },
            )
        })?;
    let count = usize::try_from(count).map_err(|_| {
        XllError::input(
            "RTD topic",
            InputError::TooLarge {
                limit: MAX_RTD_TOPIC_PARTS,
                actual: usize::MAX,
            },
        )
    })?;
    if count == 0 || count > MAX_RTD_TOPIC_PARTS {
        return Err(XllError::input(
            "RTD topic",
            InputError::TooLarge {
                limit: MAX_RTD_TOPIC_PARTS,
                actual: count,
            },
        ));
    }
    Ok(count)
}

pub(super) fn checked_topic_part_length(length: usize) -> XllResult<()> {
    if length > crate::utf16::EXCEL_STRING_LIMIT {
        Err(XllError::input(
            "RTD topic",
            InputError::TooLarge {
                limit: crate::utf16::EXCEL_STRING_LIMIT,
                actual: length,
            },
        ))
    } else {
        Ok(())
    }
}

pub(super) unsafe fn topic_key_from_safearray(strings: *mut *mut SAFEARRAY) -> XllResult<String> {
    let Some(strings) = NonNull::new(strings) else {
        return Err(XllError::InvalidHandle);
    };

    // SAFETY: `strings` is a validated non-null pointer to the SAFEARRAY pointer
    // supplied by COM and is readable for this method invocation.
    let array = unsafe { *strings.as_ptr() };

    let Some(array) = NonNull::new(array) else {
        return Err(XllError::InvalidHandle);
    };

    let array_ptr = array.as_ptr();

    // SAFETY: `array_ptr` is a non-null live SAFEARRAY pointer supplied by COM.
    let dim = unsafe { SafeArrayGetDim(array_ptr) };

    if dim != 1 {
        return Err(XllError::InvalidHandle);
    }

    let mut lower = 0;
    let mut upper = 0;

    // SAFETY: `array_ptr` is a live one-dimensional SAFEARRAY and `lower` is
    // writable output storage.
    let lower_status = unsafe { SafeArrayGetLBound(array_ptr, 1, &mut lower) };

    // SAFETY: `array_ptr` is a live one-dimensional SAFEARRAY and `upper` is
    // writable output storage.
    let upper_status = unsafe { SafeArrayGetUBound(array_ptr, 1, &mut upper) };

    if lower_status < 0 || upper_status < 0 {
        return Err(XllError::InvalidHandle);
    }
    let count = checked_topic_part_count(lower, upper)?;
    if count != REQUIRED_RTD_TOPIC_PARTS {
        return Err(XllError::InvalidHandle);
    }

    let mut vt = 0u16;

    // SAFETY: `array_ptr` is a live SAFEARRAY and `vt` is writable output
    // storage for its element VARTYPE.
    let vt_status = unsafe { SafeArrayGetVartype(array_ptr, &mut vt) };

    if vt_status < 0 {
        return Err(XllError::InvalidHandle);
    }

    let mut parts = Vec::with_capacity(count);

    for offset in 0..count {
        let index = i64::from(lower)
            .checked_add(i64::try_from(offset).map_err(|_| XllError::InvalidHandle)?)
            .and_then(|index| i32::try_from(index).ok())
            .ok_or(XllError::InvalidHandle)?;
        let part_str = match vt {
            VT_BSTR => {
                let mut bstr_ptr: *mut u16 = ptr::null_mut();

                // SAFETY: `index` is within the validated SAFEARRAY bounds and
                // `bstr_ptr` points to writable storage. For a VT_BSTR array,
                // SafeArrayGetElement returns a copied BSTR owned by the caller.
                if unsafe {
                    SafeArrayGetElement(array_ptr, &index, (&mut bstr_ptr as *mut *mut u16).cast())
                } < 0
                {
                    return Err(XllError::InvalidHandle);
                }

                let Some(bstr) = NonNull::new(bstr_ptr) else {
                    return Err(XllError::InvalidHandle);
                };

                let _bstr_guard = BstrGuard(bstr);

                // SAFETY: `bstr` is a live BSTR returned by
                // SafeArrayGetElement and remains live while `_bstr_guard` exists.
                let length = unsafe { SysStringLen(bstr.as_ptr()) } as usize;
                checked_topic_part_length(length)?;

                // SAFETY: a BSTR contains at least SysStringLen UTF-16 code
                // units, and `_bstr_guard` keeps the allocation alive.
                let units = unsafe { std::slice::from_raw_parts(bstr.as_ptr(), length) };

                String::from_utf16(units)
                    .map_err(|_| XllError::input("RTD topic", InputError::InvalidUtf16))?
            }
            VT_VARIANT | VT_EMPTY => {
                let mut value = VARIANT::default();

                // SAFETY: `index` is within the validated SAFEARRAY bounds and
                // `value` points to initialized writable VARIANT storage. On
                // success VariantGuard clears the copied value exactly once.
                if unsafe {
                    SafeArrayGetElement(array_ptr, &index, (&mut value as *mut VARIANT).cast())
                } < 0
                {
                    return Err(XllError::InvalidHandle);
                }

                let _value = VariantGuard(NonNull::from(&mut value));

                // SAFETY: SafeArrayGetElement successfully initialized `value`,
                // so its VARIANT header and discriminated payload may be read.
                let variant = unsafe { value.Anonymous.Anonymous };

                if variant.vt != VT_BSTR {
                    return Err(XllError::InvalidHandle);
                }

                // SAFETY: the VARIANT discriminant was checked to be VT_BSTR,
                // so `bstrVal` is the active union member.
                let Some(bstr) = NonNull::new(unsafe { variant.Anonymous.bstrVal as *mut u16 })
                else {
                    return Err(XllError::InvalidHandle);
                };

                // SAFETY: `bstr` is owned by the live VARIANT and remains valid
                // until `_value` clears that VARIANT.
                let length = unsafe { SysStringLen(bstr.as_ptr()) } as usize;
                checked_topic_part_length(length)?;

                // SAFETY: the BSTR contains at least `length` UTF-16 code units
                // and `_value` keeps the owning VARIANT alive.
                let units = unsafe { std::slice::from_raw_parts(bstr.as_ptr(), length) };

                String::from_utf16(units)
                    .map_err(|_| XllError::input("RTD topic", InputError::InvalidUtf16))?
            }
            _ => return Err(XllError::InvalidHandle),
        };

        parts.push(part_str);
    }

    parts.pop().ok_or(XllError::InvalidHandle)
}

struct BstrGuard(NonNull<u16>);

impl Drop for BstrGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a non-null BSTR allocated or copied for this
        // guard and has not previously been freed.
        unsafe { SysFreeString(self.0.as_ptr()) };
    }
}

struct VariantGuard(NonNull<VARIANT>);

impl Drop for VariantGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` points to an initialized VARIANT exclusively owned by
        // the surrounding stack frame and not previously cleared.
        let _ = unsafe { VariantClear(self.0.as_ptr()) };
    }
}
