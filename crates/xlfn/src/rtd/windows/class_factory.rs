use super::module_lifetime;
use super::module_state::{ComObjectKind, ComObjectLease};
use super::{
    ACTIVE_SERVER, IID_IUNKNOWN, RtdServer, com_boundary, guid_eq, server_add_ref,
    server_query_interface, server_release,
};
use crate::XllError;
use crate::win32::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOINTERFACE, E_POINTER,
    E_UNEXPECTED, GUID, S_OK,
};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicU32, Ordering};

pub(crate) const IID_ICLASS_FACTORY: GUID = GUID {
    data1: 0x0000_0001,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

pub(super) static CLASS_FACTORY_VTABLE: ClassFactoryVtable = ClassFactoryVtable {
    query_interface: factory_query_interface,
    add_ref: factory_add_ref,
    release: factory_release,
    create_instance: factory_create_instance,
    lock_server: factory_lock_server,
};

pub(super) struct ClassFactory {
    // The COM caller reads this first slot through the published object ABI.
    #[allow(dead_code, reason = "read by the external COM vtable ABI")]
    pub(super) vtable: *const ClassFactoryVtable,
    pub(super) references: AtomicU32,
    pub(super) server: *mut RtdServer,
    // Keep the module hold until every other field has been destroyed.
    pub(super) _module_lease: ComObjectLease,
}

#[repr(C)]
pub(super) struct ClassFactoryVtable {
    pub(super) query_interface:
        unsafe extern "system" fn(*mut ClassFactory, *const GUID, *mut *mut c_void) -> i32,
    pub(super) add_ref: unsafe extern "system" fn(*mut ClassFactory) -> u32,
    pub(super) release: unsafe extern "system" fn(*mut ClassFactory) -> u32,
    pub(super) create_instance: unsafe extern "system" fn(
        *mut ClassFactory,
        *mut c_void,
        *const GUID,
        *mut *mut c_void,
    ) -> i32,
    pub(super) lock_server: unsafe extern "system" fn(*mut ClassFactory, i32) -> i32,
}

pub(crate) unsafe fn dll_get_class_object(
    class_id: *const c_void,
    interface_id: *const c_void,
    output: *mut *mut c_void,
) -> i32 {
    com_boundary("DllGetClassObject", || {
        // SAFETY: the raw pointers are forwarded unchanged from the COM export.
        // The inner function validates them before dereferencing.
        unsafe { dll_get_class_object_inner(class_id, interface_id, output) }
    })
}

unsafe fn dll_get_class_object_inner(
    class_id: *const c_void,
    interface_id: *const c_void,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() {
        return E_POINTER;
    }

    // SAFETY: `output` was validated as non-null and points to a writable COM
    // output slot supplied by the caller.
    unsafe { *output = ptr::null_mut() };

    if class_id.is_null() || interface_id.is_null() {
        return E_POINTER;
    }

    // SAFETY: COM supplied a readable pointer to a GUID and it was validated as
    // non-null above.
    let requested_class = unsafe { *(class_id.cast::<GUID>()) };

    let active = ACTIVE_SERVER.lock();
    let Some(entry) = active
        .as_ref()
        .filter(|entry| guid_eq(entry.class_id, requested_class))
    else {
        return CLASS_E_CLASSNOTAVAILABLE;
    };

    let server = entry.pointer as *mut RtdServer;

    // Do not publish a new class factory once add-in shutdown has closed this
    // server. Holding the guard also makes a concurrent shutdown wait until the
    // factory owns its server reference.
    // SAFETY: ACTIVE_SERVER owns a live server reference while its mutex is held.
    let _server_operation = match unsafe { (*server).operations.enter() } {
        Some(operation) => operation,
        None => return CLASS_E_CLASSNOTAVAILABLE,
    };

    // SAFETY: ACTIVE_SERVER owns a live server reference. The factory receives
    // an additional reference that it releases when the factory is destroyed.
    unsafe { server_add_ref(server) };

    let factory = Box::into_raw(Box::new(ClassFactory {
        vtable: &CLASS_FACTORY_VTABLE,
        references: AtomicU32::new(1),
        server,
        _module_lease: ComObjectLease::new(ComObjectKind::Factory),
    }));

    // SAFETY: `factory` is a newly allocated live COM object, `interface_id` is
    // a validated readable GUID pointer, and `output` is writable.
    let status = unsafe { factory_query_interface(factory, interface_id.cast::<GUID>(), output) };

    // SAFETY: release the construction reference. QueryInterface acquired a
    // separate reference if it succeeded.
    unsafe { factory_release(factory) };

    status
}

unsafe extern "system" fn factory_query_interface(
    this: *mut ClassFactory,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    let _module_call = module_lifetime().enter_call();
    if output.is_null() {
        return E_POINTER;
    }

    // SAFETY: `output` was validated as non-null and points to writable storage.
    unsafe { *output = ptr::null_mut() };

    if this.is_null() || interface_id.is_null() {
        return E_POINTER;
    }

    // SAFETY: `interface_id` was validated as non-null and COM supplies a
    // readable GUID for the duration of this method.
    let interface_id = unsafe { *interface_id };

    if guid_eq(interface_id, IID_IUNKNOWN) || guid_eq(interface_id, IID_ICLASS_FACTORY) {
        // SAFETY: `output` is writable and `this` is a live factory pointer.
        // AddRef creates the reference returned through `output`.
        unsafe {
            *output = this.cast();
            factory_add_ref(this);
        }

        S_OK
    } else {
        E_NOINTERFACE
    }
}

unsafe extern "system" fn factory_add_ref(this: *mut ClassFactory) -> u32 {
    let _module_call = module_lifetime().enter_call();
    // SAFETY: COM calls AddRef only on a live object pointer. The atomic update
    // preserves the shared COM reference count.
    unsafe { (*this).references.fetch_add(1, Ordering::Relaxed) + 1 }
}

pub(super) unsafe extern "system" fn factory_release(this: *mut ClassFactory) -> u32 {
    let _module_call = module_lifetime().enter_call();
    let Some(this) = NonNull::new(this) else {
        return 0;
    };

    // SAFETY: COM calls Release only on a live object. A zero previous count
    // is an invariant violation and must never wrap to `u32::MAX`.
    let previous = unsafe { this.as_ref().references.fetch_sub(1, Ordering::AcqRel) };
    if previous == 0 {
        std::process::abort();
    }
    let remaining = previous - 1;

    if remaining == 0 {
        // SAFETY: observing the transition to zero proves this is the final
        // reference and uniquely owns the original Box allocation.
        let factory = unsafe { Box::from_raw(this.as_ptr()) };

        // SAFETY: each factory owns one server reference acquired when the
        // factory was constructed.
        unsafe { server_release(factory.server) };
    }

    remaining
}

unsafe extern "system" fn factory_create_instance(
    this: *mut ClassFactory,
    outer: *mut c_void,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    if !output.is_null() {
        // SAFETY: COM supplied `output` as the writable result slot for this
        // method. Clearing it before the panic boundary guarantees every
        // failure path, including an unexpected unwind, returns no stale
        // interface pointer.
        unsafe { *output = ptr::null_mut() };
    }

    com_boundary("IClassFactory::CreateInstance", || {
        // SAFETY: the raw arguments are forwarded from the COM vtable method.
        // The inner function validates all applicable pointer contracts.
        unsafe { factory_create_instance_inner(this, outer, interface_id, output) }
    })
}

unsafe fn factory_create_instance_inner(
    this: *mut ClassFactory,
    outer: *mut c_void,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() {
        return E_POINTER;
    }

    // SAFETY: `output` was validated as non-null and points to the writable
    // result slot supplied by COM.
    unsafe { *output = ptr::null_mut() };

    if !outer.is_null() {
        return CLASS_E_NOAGGREGATION;
    }

    if this.is_null() || interface_id.is_null() {
        return E_POINTER;
    }

    // SAFETY: `this` was validated as non-null, the live factory owns one server
    // reference, and therefore its server pointer remains valid for this call.
    let server = unsafe { (*this).server };
    // SAFETY: the live factory reference described above keeps `server` valid
    // while the operation guard is acquired and held.
    let _server_operation = match unsafe { (*server).operations.enter() } {
        Some(operation) => operation,
        None => return E_FAIL,
    };

    // SAFETY: `this` was validated as non-null and COM keeps the factory live
    // during this call. server_query_interface validates the forwarded interface
    // and output pointers before dereferencing them.
    unsafe { server_query_interface(server, interface_id, output) }
}

pub(super) unsafe extern "system" fn factory_lock_server(
    this: *mut ClassFactory,
    lock: i32,
) -> i32 {
    let operation = || {
        if this.is_null() {
            return E_POINTER;
        }
        if module_lifetime().set_server_lock(lock != 0) {
            S_OK
        } else {
            E_UNEXPECTED
        }
    };

    if lock == 0 {
        // Unlocking releases an existing module hold rather than admitting new
        // work. It must remain available after ingress enters CLOSING.
        let (_module_call, _accepted) = module_lifetime().enter_call();
        match catch_unwind(AssertUnwindSafe(operation)) {
            Ok(status) => status,
            Err(_) => {
                crate::diagnostics::report_no_unwind("IClassFactory::LockServer", &XllError::Panic);
                E_UNEXPECTED
            }
        }
    } else {
        // Acquiring another server lock is new work and remains subject to
        // normal ingress admission.
        com_boundary("IClassFactory::LockServer", operation)
    }
}
