//! Standalone, process-pinned experiment. No production unload certificate is issued here.
#![deny(unsafe_op_in_unsafe_fn)]

use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    ptr::NonNull,
    sync::{
        Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering::SeqCst},
    },
    time::{Duration, Instant},
};
use xlfn_sys::*;

struct Trace {
    file: Mutex<File>,
    token: String,
    created: u64,
    start: Instant,
}
static TRACE: OnceLock<Trace> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);
static ENDED: AtomicU64 = AtomicU64::new(0);
static CANCELED: AtomicU64 = AtomicU64::new(0);
static STARTED: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicU64 = AtomicU64::new(0);
static CLOSED: AtomicBool = AtomicBool::new(false);
static RETURN_GATE: RwLock<()> = RwLock::new(());
static REMOVAL_METHOD: OnceLock<RemovalMethod> = OnceLock::new();
static REGISTRATION_IDS: [AtomicI32; 2] = [AtomicI32::new(0), AtomicI32::new(0)];

#[derive(Clone, Copy, PartialEq)]
enum RemovalMethod {
    Nil,
    UnregisterId,
}

fn log(event: &str, details: Value) {
    if let Some(t) = TRACE.get() {
        let entry = json!({"schema":1,"source":"event-probe","pid":std::process::id(),
            "creation_ticks":t.created,"token":t.token,"elapsed_ns":t.start.elapsed().as_nanos() as u64,
            "generation":GENERATION.load(SeqCst),"event":event,"details":details});
        let mut bytes = entry.to_string().into_bytes();
        bytes.push(b'\n');
        if let Ok(mut file) = t.file.lock() {
            let _ = file.write_all(&bytes);
        }
    }
}

fn snapshot() {
    log(
        "snapshot",
        json!({"generation":GENERATION.load(SeqCst),"ended":ENDED.load(SeqCst),"canceled":CANCELED.load(SeqCst),
        "started":STARTED.load(SeqCst),"active":ACTIVE.load(SeqCst)}),
    );
}

fn initialize() -> bool {
    let Some(created) = native::pin_and_identity(xlAutoOpen as *const ()) else {
        return false;
    };
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return false;
    };
    let path = PathBuf::from(local)
        .join("xlfn/event-probe")
        .join(format!("{}.json", std::process::id()));
    let mut text = String::new();
    let Ok(file) = File::open(path) else {
        return false;
    };
    if file.take(16_384).read_to_string(&mut text).is_err() {
        return false;
    }
    let Ok(config) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let method = match config["removal_method"].as_str().unwrap_or("nil") {
        "nil" => RemovalMethod::Nil,
        "unregister-id" => RemovalMethod::UnregisterId,
        _ => return false,
    };
    let Some(token) = config["token"].as_str() else {
        return false;
    };
    let Some(path) = config["path"].as_str().map(PathBuf::from) else {
        return false;
    };
    if config["schema"] != 1
        || config["pid"] != std::process::id()
        || config["creation_ticks"] != created
        || !path.is_absolute()
        || token.len() != 32
        || !token.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return false;
    }
    let Ok(file) = OpenOptions::new().append(true).open(path) else {
        return false;
    };
    if REMOVAL_METHOD.set(method).is_err() {
        return false;
    }
    TRACE
        .set(Trace {
            file: Mutex::new(file),
            token: token.into(),
            created,
            start: Instant::now(),
        })
        .is_ok()
}

struct Text(Vec<u16>);
impl Text {
    fn new(s: &str) -> Self {
        let mut v = vec![0];
        v.extend(s.encode_utf16());
        v[0] = (v.len() - 1) as u16;
        Self(v)
    }
    fn value(&mut self) -> XLOPER12 {
        XLOPER12 {
            value: XLOPER12Value {
                string: self.0.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        }
    }
}

fn raw(value: &XLOPER12) -> Value {
    // SAFETY: only inline scalar union members selected by their exact tag are read.
    unsafe {
        match value.base_type() {
            XLTYPE_INT => json!({"xltype":value.xltype,"value":value.value.integer,
                "bits":format!("0x{:08x}",value.value.integer as u32)}),
            XLTYPE_NUM => json!({"xltype":value.xltype,"value":value.value.number}),
            XLTYPE_BOOL => json!({"xltype":value.xltype,"value":value.value.boolean}),
            XLTYPE_ERR => json!({"xltype":value.xltype,"value":value.value.error}),
            _ => json!({"xltype":value.xltype}),
        }
    }
}

// Only called from xlAutoOpen or registered commands, never from a worksheet UDF.
fn invoke(function: i32, values: &mut [XLOPER12]) -> (i32, XLOPER12, bool) {
    let pointers: Vec<_> = values.iter_mut().map(NonNull::from_mut).collect();
    // SAFETY: owned arguments and their backing strings outlive this synchronous callback.
    unsafe { excel12_with_invocation(function, &pointers) }
}

fn release(value: &mut XLOPER12, invoked: bool) -> Option<i32> {
    // SAFETY: only a result actually supplied by Excel is released, exactly once.
    invoked.then(|| unsafe { excel_free(value) })
}

fn events(remove: bool) -> bool {
    events_with_method(
        remove,
        REMOVAL_METHOD.get().copied().unwrap_or(RemovalMethod::Nil),
    )
}

fn events_with_method(remove: bool, method: RemovalMethod) -> bool {
    if ACTIVE.load(SeqCst) != 0 || CLOSED.load(SeqCst) {
        log("command.refused", json!({"active":ACTIVE.load(SeqCst)}));
        return false;
    }
    if !remove {
        if GENERATION.load(SeqCst) >= 4 {
            return false;
        }
        GENERATION.fetch_add(1, SeqCst);
    }
    let generation = GENERATION.load(SeqCst);
    let handlers = match generation {
        1 => ["probe_ended_1", "probe_canceled_1"],
        2 => ["probe_ended_2", "probe_canceled_2"],
        3 => ["probe_ended_3", "probe_canceled_3"],
        4 => ["probe_ended_4", "probe_canceled_4"],
        _ => return false,
    };
    let mut ok = true;
    for (selector, name) in [(1, handlers[0]), (2, handlers[1])] {
        let registration_id = REGISTRATION_IDS[(selector - 1) as usize].load(SeqCst);
        let mut procedure = Text::new(name);
        let arg = if remove {
            XLOPER12::nil()
        } else {
            procedure.value()
        };
        let by_id = remove && method == RemovalMethod::UnregisterId;
        if by_id && registration_id == 0 {
            return false;
        }
        // Experimental only: the public xlfUnregister documentation covers
        // xlfRegister IDs, not xlEventRegister IDs. Keep this out of production
        // until actual event behavior establishes whether the candidate works.
        let (status, mut result, invoked) = if by_id {
            invoke(
                XLF_UNREGISTER,
                &mut [XLOPER12::number(f64::from(registration_id))],
            )
        } else {
            invoke(XL_EVENT_REGISTER, &mut [arg, XLOPER12::integer(selector)])
        };
        let value = raw(&result);
        let accepted = invoked
            && status == XLRET_SUCCESS
            && (remove
                || (result.base_type() == XLTYPE_INT
                    && value["value"].as_i64().is_some_and(|n| n != 0)));
        if !remove && accepted {
            REGISTRATION_IDS[(selector - 1) as usize]
                .store(value["value"].as_i64().unwrap() as i32, SeqCst);
        }
        let cleanup = release(&mut result, invoked);
        let mut details = json!({"selector":selector,"transport":status,"invoked":invoked,"raw":value,"cleanup":cleanup});
        if by_id {
            details["method"] = json!("unregister-id");
            details["api"] = json!("xlfUnregister");
            details["registration_id"] = json!(registration_id);
            details["argument_xltype"] = json!(XLTYPE_NUM);
            details["argument_value"] = json!(f64::from(registration_id));
        } else {
            details["api"] = json!("xlEventRegister");
            details["procedure"] = if remove { Value::Null } else { json!(name) };
            details["procedure_xltype"] = json!(arg.xltype);
            details["event_xltype"] = json!(XLTYPE_INT);
            if remove {
                details["method"] = json!("nil");
            }
        }
        log(
            if remove {
                "event.remove"
            } else {
                "event.register"
            },
            details,
        );
        ok &= accepted && cleanup == Some(XLRET_SUCCESS);
        if !ok {
            break;
        }
    }
    // For removal this reports only that both calls completed, NOT that handlers detached.
    ok
}

fn register_functions() -> bool {
    let (status, mut module, invoked) = invoke(XL_GET_NAME, &mut []);
    if status != XLRET_SUCCESS || module.base_type() != XLTYPE_STR {
        release(&mut module, invoked);
        return false;
    }
    let mut ok = true;
    for (procedure, name, types, macro_type) in [
        ("probe_register", "XEP.REGISTER", "J", 2),
        ("probe_remove", "XEP.REMOVE", "J", 2),
        ("probe_snapshot", "XEP.SNAPSHOT", "J", 2),
        ("probe_delay", "XEP.DELAY", ">BX", 1),
    ] {
        let (mut p, mut n, mut t, mut a) = (
            Text::new(procedure),
            Text::new(name),
            Text::new(types),
            Text::new("milliseconds"),
        );
        let (status, mut result, invoked) = invoke(
            XLF_REGISTER,
            &mut [
                module,
                p.value(),
                t.value(),
                n.value(),
                a.value(),
                XLOPER12::integer(macro_type),
            ],
        );
        let value = raw(&result);
        ok &= status == XLRET_SUCCESS && result.base_type() == XLTYPE_NUM;
        let cleanup = release(&mut result, invoked);
        ok &= cleanup == Some(XLRET_SUCCESS);
        log(
            "function.register",
            json!({"name":name,"transport":status,"raw":value,"cleanup":cleanup}),
        );
        if !ok {
            break;
        }
    }
    ok &= release(&mut module, invoked) == Some(XLRET_SUCCESS);
    ok
}

// All exported entry points contain unwinding, including diagnostic IO/allocation failures.
fn boundary(f: impl FnOnce() -> bool) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => i32::from(value),
        Err(payload) => {
            // A payload destructor must not cause a second unwind across Excel's ABI.
            std::mem::forget(payload);
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn xlAutoOpen() -> i32 {
    boundary(|| {
        if !initialize() {
            return false;
        }
        log("open", json!({"process_pinned":true}));
        events(false) && register_functions()
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn xlAutoClose() -> i32 {
    boundary(|| {
        CLOSED.store(true, SeqCst);
        // Join any completion already in flight; later workers cannot call Excel.
        let _gate = RETURN_GATE.write().unwrap_or_else(|e| e.into_inner());
        log(
            "close",
            json!({"active":ACTIVE.load(SeqCst),"process_pinned":true}),
        );
        true
    });
    1
}
#[unsafe(no_mangle)]
pub extern "system" fn probe_register() -> i32 {
    boundary(|| events(false))
}
#[unsafe(no_mangle)]
pub extern "system" fn probe_remove() -> i32 {
    boundary(|| events(true))
}
#[unsafe(no_mangle)]
pub extern "system" fn probe_snapshot() -> i32 {
    boundary(|| {
        snapshot();
        true
    })
}
// Distinct addresses identify stale callbacks after re-registration. Reading
// the current generation alone would incorrectly relabel an old handler.
macro_rules! handlers {
    ($ended:ident, $canceled:ident, $generation:expr) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn $ended() {
            boundary(|| {
                ENDED.fetch_add(1, SeqCst);
                log("callback.ended", json!({"handler_generation":$generation}));
                true
            });
        }
        #[unsafe(no_mangle)]
        pub extern "system" fn $canceled() {
            boundary(|| {
                CANCELED.fetch_add(1, SeqCst);
                log("callback.canceled", json!({"handler_generation":$generation}));
                true
            });
        }
    };
}
handlers!(probe_ended_1, probe_canceled_1, 1);
handlers!(probe_ended_2, probe_canceled_2, 2);
handlers!(probe_ended_3, probe_canceled_3, 3);
handlers!(probe_ended_4, probe_canceled_4, 4);

struct Handle(XLOPER12);
// SAFETY: the validated big-data token is opaque Excel-owned identity. It is
// copied, never dereferenced, and used only by Excel's worker-safe xlAsyncReturn.
unsafe impl Send for Handle {}
impl Handle {
    fn complete(mut self) {
        let mut result = XLOPER12::number(1.0);
        // SAFETY: both carriers are live; the gate excludes calls after xlAutoClose.
        let (status, reply, invoked) = unsafe {
            excel12_async_return(
                NonNull::from_mut(&mut self.0),
                NonNull::from_mut(&mut result),
            )
        };
        // xlFree is NOT permitted on this worker. xlAsyncReturn returns an inline scalar.
        log(
            "task.return",
            json!({"transport":status,"invoked":invoked,"raw":raw(&reply)}),
        );
    }
}

#[unsafe(no_mangle)]
/// # Safety
/// Excel must supply a live, aligned X-type async handle for this call.
pub unsafe extern "system" fn probe_delay(milliseconds: f64, handle: *const XLOPER12) {
    boundary(|| {
        if handle.is_null()
            || !milliseconds.is_finite()
            || !(0.0..=30_000.0).contains(&milliseconds)
        {
            return false;
        }
        // SAFETY: Excel supplies a live X-type carrier for this asynchronous call.
        let value = unsafe { *handle };
        if value.base_type() != XLTYPE_BIG_DATA {
            return false;
        }
        // SAFETY: the checked big-data tag selects the opaque handle member.
        let big_data = unsafe { value.value.big_data };
        let token = Handle(XLOPER12 {
            value: XLOPER12Value { big_data },
            xltype: XLTYPE_BIG_DATA,
        });
        if ACTIVE
            .try_update(SeqCst, SeqCst, |n| (n < 8).then_some(n + 1))
            .is_err()
        {
            return false;
        }
        STARTED.fetch_add(1, SeqCst);
        let canceled = CANCELED.load(SeqCst);
        log("task.started", json!({"milliseconds":milliseconds}));
        let spawn = std::thread::Builder::new()
            .name("event-probe".into())
            .spawn(move || {
                boundary(|| {
                    let deadline = Instant::now() + Duration::from_secs_f64(milliseconds / 1000.0);
                    while Instant::now() < deadline
                        && CANCELED.load(SeqCst) == canceled
                        && !CLOSED.load(SeqCst)
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    let _gate = RETURN_GATE.read().unwrap_or_else(|e| e.into_inner());
                    if !CLOSED.load(SeqCst) && CANCELED.load(SeqCst) == canceled {
                        token.complete();
                    }
                    true
                });
                ACTIVE.fetch_sub(1, SeqCst);
                log("task.finished", json!({"active":ACTIVE.load(SeqCst)}));
            });
        if spawn.is_err() {
            ACTIVE.fetch_sub(1, SeqCst);
            log("task.spawn_failed", json!({}));
            return false;
        }
        true
    });
}

#[cfg(windows)]
mod native {
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    windows_link::link!("kernel32.dll" "system" fn GetModuleHandleExW(flags:u32,address:*const u16,module:*mut *mut core::ffi::c_void)->i32);
    windows_link::link!("kernel32.dll" "system" fn GetCurrentProcess()->*mut core::ffi::c_void);
    windows_link::link!("kernel32.dll" "system" fn GetProcessTimes(process:*mut core::ffi::c_void,created:*mut FileTime,exited:*mut FileTime,kernel:*mut FileTime,user:*mut FileTime)->i32);
    pub fn pin_and_identity(anchor: *const ()) -> Option<u64> {
        let mut module = core::ptr::null_mut();
        // SAFETY: FROM_ADDRESS|PIN uses our exported entry point's address, not a string.
        if unsafe { GetModuleHandleExW(4 | 1, anchor.cast(), &mut module) } == 0 {
            return None;
        }
        let (mut c, mut e, mut k, mut u) = (
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
        );
        // SAFETY: valid process pseudo-handle and writable FILETIME outputs.
        (unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) } != 0)
            .then_some((u64::from(c.high) << 32) | u64::from(c.low))
    }
}
#[cfg(not(windows))]
mod native {
    pub fn pin_and_identity(_: *const ()) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static CALLS: Mutex<Vec<(i32, u32, i32)>> = Mutex::new(Vec::new());

    unsafe extern "system" fn mock_excel(
        function: i32,
        count: i32,
        args: *mut *mut XLOPER12,
        result: *mut XLOPER12,
    ) -> i32 {
        if function == XL_FREE {
            CALLS.lock().unwrap().push((function, 0, 0));
            return XLRET_SUCCESS;
        }
        if function == XLF_UNREGISTER {
            assert_eq!(count, 1);
            // SAFETY: invoke supplies one live numeric argument and writable result.
            unsafe {
                let id = **args;
                assert_eq!(id.base_type(), XLTYPE_NUM);
                CALLS
                    .lock()
                    .unwrap()
                    .push((function, id.xltype, id.value.number as i32));
                *result = XLOPER12::boolean(true);
            }
            return XLRET_SUCCESS;
        }
        assert_eq!(function, XL_EVENT_REGISTER);
        assert_eq!(count, 2);
        // SAFETY: invoke supplies two live carriers; result is the writable host return slot.
        unsafe {
            let procedure = **args;
            let event = **args.add(1);
            assert_eq!(event.base_type(), XLTYPE_INT);
            CALLS
                .lock()
                .unwrap()
                .push((function, procedure.xltype, event.value.integer));
            *result = XLOPER12::integer(if procedure.base_type() == XLTYPE_NIL {
                0
            } else {
                -1657274367
            });
        }
        XLRET_SUCCESS
    }

    #[test]
    fn probe_preserves_nil_removal_and_retains_zero_as_an_observation() {
        // SAFETY: the callback has the exact ABI and static lifetime; no other
        // test invokes Excel or changes this override.
        unsafe { install_callback_for_abi_probe(mock_excel as *const () as *mut _) };
        assert!(events(false));
        assert!(events(true)); // Completed experiment calls, not a removal certificate.
        let calls = CALLS.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                (XL_EVENT_REGISTER, XLTYPE_STR, 1),
                (XL_FREE, 0, 0),
                (XL_EVENT_REGISTER, XLTYPE_STR, 2),
                (XL_FREE, 0, 0),
                (XL_EVENT_REGISTER, XLTYPE_NIL, 1),
                (XL_FREE, 0, 0),
                (XL_EVENT_REGISTER, XLTYPE_NIL, 2),
                (XL_FREE, 0, 0),
            ]
        );
        ACTIVE.store(1, SeqCst);
        assert!(!events(true));
        ACTIVE.store(0, SeqCst);
        assert_eq!(*CALLS.lock().unwrap(), calls);
        assert!(events_with_method(true, RemovalMethod::UnregisterId));
        assert_eq!(
            &CALLS.lock().unwrap()[calls.len()..],
            &[
                (XLF_UNREGISTER, XLTYPE_NUM, -1657274367),
                (XL_FREE, 0, 0),
                (XLF_UNREGISTER, XLTYPE_NUM, -1657274367),
                (XL_FREE, 0, 0),
            ]
        );
        // SAFETY: null restores the normal resolver after this serialized protocol test.
        unsafe { install_callback_for_abi_probe(std::ptr::null_mut()) };
    }

    #[test]
    fn raw_integer_bits_are_preserved() {
        assert_eq!(raw(&XLOPER12::integer(-1657274367))["bits"], "0x9d380001");
        assert_eq!(raw(&XLOPER12::integer(0))["value"], 0);
        assert_eq!(raw(&XLOPER12::nil()), json!({"xltype":256}));
    }
}
