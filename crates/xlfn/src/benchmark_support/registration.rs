//! Rust-side registration preparation and deterministic host-call overhead.

use crate::host_callback::HostCallbackSession;
use crate::registration::preflight::{PreparedRegistrationSet, preflight_registration};
use crate::registration::{
    ArgumentDescriptor, HostRegistrar, RegistrationDescriptor, RegistrationHost,
    RegistrationSignature,
};
use std::cell::Cell;
use std::hint::black_box;
use std::sync::OnceLock;
use xlfn_sys::{
    XL_FREE, XL_GET_NAME, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER, XLOPER12, XLOPER12Value,
    XLRET_FAILED, XLRET_SUCCESS, XLTYPE_STR,
};

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
struct Calls {
    module_name: usize,
    evaluate: usize,
    register: usize,
    free: usize,
    unexpected: usize,
}

thread_local! {
    static CALLS: Cell<Calls> = Cell::new(Calls::default());
}

fn module_name() -> &'static [u16] {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    NAME.get_or_init(|| {
        let name = if cfg!(windows) {
            "C:\\benchmark\\registration\\installed-addins\\representative-long-module-path\\benchmark.xll"
        } else {
            "/benchmark/registration/installed-addins/representative-long-module-path/benchmark.xll"
        };
        std::iter::once(name.encode_utf16().count() as u16)
            .chain(name.encode_utf16())
            .collect()
    })
}

unsafe extern "system" fn registration_callback(
    function: i32,
    _argument_count: i32,
    _arguments: *mut *mut XLOPER12,
    result: *mut XLOPER12,
) -> i32 {
    let mut calls = CALLS.get();
    let value = match function {
        XL_FREE => {
            calls.free = calls.free.wrapping_add(1);
            CALLS.set(calls);
            return XLRET_SUCCESS;
        }
        XL_GET_NAME => {
            calls.module_name = calls.module_name.wrapping_add(1);
            XLOPER12 {
                value: XLOPER12Value {
                    string: module_name().as_ptr().cast_mut(),
                },
                xltype: XLTYPE_STR,
            }
        }
        XLF_EVALUATE => {
            calls.evaluate = calls.evaluate.wrapping_add(1);
            XLOPER12::error(XLERR_NAME)
        }
        XLF_REGISTER => {
            calls.register = calls.register.wrapping_add(1);
            XLOPER12::number(calls.register as f64)
        }
        _ => {
            calls.unexpected = calls.unexpected.wrapping_add(1);
            CALLS.set(calls);
            return XLRET_FAILED;
        }
    };
    CALLS.set(calls);
    if result.is_null() {
        return XLRET_FAILED;
    }
    // SAFETY: Excel's callback ABI provides writable result storage for the
    // non-release operations above; the module-name allocation is permanent.
    unsafe { result.write(value) };
    XLRET_SUCCESS
}

fn descriptors() -> &'static [RegistrationDescriptor] {
    static DESCRIPTORS: OnceLock<Vec<RegistrationDescriptor>> = OnceLock::new();
    DESCRIPTORS.get_or_init(|| {
        (0..5_000)
            .map(|index| RegistrationDescriptor {
                export_name: Box::leak(format!("benchmark_extra_{index:04}").into_boxed_str()),
                excel_name: Box::leak(format!("BENCH.EXTRA.{index:04}").into_boxed_str()),
                signature: RegistrationSignature {
                    execution: xlfn_common::ExecutionKind::ThreadSafe,
                    arguments: &[crate::registration::ArgumentAbi::Float64],
                    return_abi: crate::registration::ReturnAbi::Value12,
                    volatile: false,
                },
                category: "Benchmark",
                description: "Registration scaling fixture",
                help_topic: "",
                visibility: xlfn_common::FunctionVisibility::Public,
                arguments: &[ArgumentDescriptor {
                    name: "value",
                    description: "Input value",
                }],
            })
            .collect()
    })
}

/// Measures production Rust registration paths with a deterministic Excel
/// callback. Host name lookup, actual Excel registration, DLL loading and
/// user startup work are excluded. Fixtures keep one argument per function.
pub struct RegistrationBenchmark {
    descriptors: &'static [RegistrationDescriptor],
    prepared: PreparedRegistrationSet,
}

impl RegistrationBenchmark {
    pub fn new(count: usize) -> Self {
        assert!((1..=5_000).contains(&count));
        let descriptors = &descriptors()[..count];
        let prepared = preflight_registration(descriptors).expect("valid registration fixture");
        module_name();
        super::get_benchmark_runtime();
        // SAFETY: the callback has Excel's exact ABI and lives for the complete
        // benchmark process. This fixture is used by a dedicated benchmark.
        unsafe {
            xlfn_sys::install_callback_for_abi_probe(
                registration_callback as *const () as *mut std::ffi::c_void,
            );
        }
        let fixture = Self {
            descriptors,
            prepared,
        };
        fixture.run_registration();
        assert_eq!(
            CALLS.get(),
            Calls {
                module_name: 1,
                evaluate: count,
                register: count,
                free: count * 2 + 1,
                unexpected: 0,
            },
            "each registration must keep conflict lookup, registration and result release",
        );
        fixture
    }

    pub fn run_preflight(&self) {
        black_box(preflight_registration(black_box(self.descriptors)).expect("valid descriptors"));
    }

    /// Includes module-name lookup, conflict checks, callback argument
    /// construction, registration-result decoding and every result release.
    pub fn run_registration(&self) {
        Self::register(black_box(&self.prepared));
    }

    /// Includes preparation and registration in the same observation.
    pub fn run_total(&self) {
        let prepared =
            preflight_registration(black_box(self.descriptors)).expect("valid descriptors");
        Self::register(&prepared);
    }

    fn register(prepared: &PreparedRegistrationSet) {
        CALLS.set(Calls::default());
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        let registrar = HostRegistrar::connect(&host)
            .unwrap_or_else(|_| panic!("benchmark module lookup must succeed"));
        black_box(
            registrar
                .register_all(&host, prepared)
                .unwrap_or_else(|_| panic!("benchmark registration must succeed")),
        );
    }
}
