//! Paired Q/native input conversion with identical call and identity setup.

use super::{BenchmarkAddin, benchmark_ingress, get_benchmark_runtime};
use crate::value::{ArgumentContext, FormulaInputMode, PlainInputMode, XlValueRef};

enum Input {
    Numbers,
    Boolean,
    Integer,
    Text,
}

/// Compares native argument admission with strict XLOPER12 admission.
/// Excel's preprocessing and generated-wrapper dispatch are outside this probe.
pub struct NativeArgumentIngressBenchmark {
    runtime: &'static crate::runtime::Runtime<BenchmarkAddin>,
    input: Input,
    raw: Vec<xlfn_sys::XLOPER12>,
    text: Vec<u16>,
}

impl NativeArgumentIngressBenchmark {
    pub fn numbers(count: usize) -> Self {
        assert!([1, 2, 4, 8].contains(&count));
        Self {
            runtime: get_benchmark_runtime(),
            input: Input::Numbers,
            raw: (0..count)
                .map(|index| xlfn_sys::XLOPER12::number(index as f64 + 0.5))
                .collect(),
            text: Vec::new(),
        }
    }

    pub fn boolean() -> Self {
        Self {
            runtime: get_benchmark_runtime(),
            input: Input::Boolean,
            raw: vec![xlfn_sys::XLOPER12::boolean(true)],
            text: Vec::new(),
        }
    }

    pub fn integer() -> Self {
        Self {
            runtime: get_benchmark_runtime(),
            input: Input::Integer,
            raw: vec![xlfn_sys::XLOPER12::integer(i32::MAX)],
            text: Vec::new(),
        }
    }

    pub fn string(value: &str) -> Self {
        let text: Vec<_> = std::iter::once(u16::try_from(value.encode_utf16().count()).unwrap())
            .chain(value.encode_utf16())
            .collect();
        assert!(text.len() <= xlfn_common::EXCEL_STRING_LIMIT + 1);
        Self {
            runtime: get_benchmark_runtime(),
            input: Input::Text,
            raw: vec![xlfn_sys::XLOPER12::nil()],
            text,
        }
    }

    /// Runs one call. `identity` models a handle producer; `materialize=false`
    /// models its warm input phase, after complete validation and hashing.
    pub fn run(&mut self, native: bool, identity: bool, materialize: bool) {
        if matches!(self.input, Input::Text) {
            self.raw[0] = xlfn_sys::XLOPER12 {
                value: xlfn_sys::XLOPER12Value {
                    string: self.text.as_mut_ptr(),
                },
                xltype: xlfn_sys::XLTYPE_STR,
            };
        }
        let ingress = benchmark_ingress();
        let call = self
            .runtime
            .enter(&ingress)
            .expect("benchmark runtime must be open");
        crate::call::with_excel_call_scope_and_call(&call, |call, scope| {
            macro_rules! run {
                ($mode:ty) => {{
                    let mut arguments = ArgumentContext::<$mode>::new(call, scope, self.raw.len());
                    for (index, raw) in self.raw.iter_mut().enumerate() {
                        macro_rules! prepare_q {
                            ($ty:ty) => {{
                                // SAFETY: fixture-owned payloads remain live and immutable during this call.
                                let value = unsafe { XlValueRef::from_raw(raw) }.unwrap();
                                arguments.prepare::<$ty>(index, "arg", value).unwrap()
                            }};
                        }
                        match self.input {
                            Input::Numbers => {
                                let prepared = if native {
                                    // SAFETY: the Numbers constructor initializes the numeric union field.
                                    let number = unsafe { raw.value.number };
                                    arguments.native_f64(index, "arg", std::hint::black_box(number)).unwrap()
                                } else {
                                    prepare_q!(f64).materialize().unwrap()
                                };
                                std::hint::black_box(prepared);
                            }
                            Input::Boolean => {
                                let prepared = if native {
                                    arguments.native_bool(index, "arg", std::hint::black_box(1)).unwrap()
                                } else {
                                    prepare_q!(bool).materialize().unwrap()
                                };
                                std::hint::black_box(prepared);
                            }
                            Input::Integer => {
                                let prepared = if native {
                                    arguments.native_i32(index, "arg", std::hint::black_box(i32::MAX)).unwrap()
                                } else {
                                    prepare_q!(i32).materialize().unwrap()
                                };
                                std::hint::black_box(prepared);
                            }
                            Input::Text => {
                                fn consume<T, P>(prepared: crate::__private::v1::PreparedArgument<T, P>, materialize: bool) {
                                    if materialize {
                                        std::hint::black_box(prepared.materialize().unwrap());
                                    } else {
                                        std::hint::black_box(prepared);
                                    }
                                }
                                if native {
                                    // SAFETY: text owns a valid counted UTF-16 buffer for this call.
                                    consume(unsafe { arguments.prepare_native_string(index, "arg", std::hint::black_box(self.text.as_ptr())) }.unwrap(), materialize);
                                } else {
                                    consume(prepare_q!(String), materialize);
                                }
                            }
                        }
                    }
                    std::hint::black_box(arguments.finish().unwrap());
                }};
            }
            if identity {
                run!(FormulaInputMode);
            } else {
                run!(PlainInputMode);
            }
        });
    }
}
