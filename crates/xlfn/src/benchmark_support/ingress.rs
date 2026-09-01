use super::handle::benchmark_revision_key;
use super::*;

/// Benchmark harness for measuring raw Excel argument ingress conversion costs
/// with and without semantic identity fingerprinting.
pub struct RawArgumentIngressBenchmark {
    runtime: &'static crate::runtime::Runtime<()>,
    raw: xlfn_sys::XLOPER12,
    _storage: Option<Box<dyn std::any::Any>>,
}

impl RawArgumentIngressBenchmark {
    pub fn number(value: f64) -> Self {
        Self {
            runtime: get_benchmark_runtime(),
            raw: xlfn_sys::XLOPER12::number(value),
            _storage: None,
        }
    }

    pub fn string(value: &str) -> Self {
        let encoded = value.encode_utf16().collect::<Vec<_>>();
        let mut u16_chars: Vec<u16> = Vec::with_capacity(encoded.len() + 1);
        u16_chars.push(encoded.len() as u16);
        u16_chars.extend(encoded);
        let raw = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                string: u16_chars.as_ptr() as *mut u16,
            },
            xltype: xlfn_sys::XLTYPE_STR,
        };
        Self {
            runtime: get_benchmark_runtime(),
            raw,
            _storage: Some(Box::new(u16_chars)),
        }
    }

    pub fn string_matrix(values: &[&str]) -> Self {
        let mut strings = values
            .iter()
            .map(|value| {
                std::iter::once(value.encode_utf16().count() as u16)
                    .chain(value.encode_utf16())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut cells = strings
            .iter_mut()
            .map(|string| xlfn_sys::XLOPER12 {
                value: xlfn_sys::XLOPER12Value {
                    string: string.as_mut_ptr(),
                },
                xltype: xlfn_sys::XLTYPE_STR,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let raw = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                array: xlfn_sys::XLOPER12Array {
                    rows: 1,
                    columns: cells.len() as i32,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: xlfn_sys::XLTYPE_MULTI,
        };
        Self {
            runtime: get_benchmark_runtime(),
            raw,
            _storage: Some(Box::new((cells, strings))),
        }
    }

    pub fn mixed_cells() -> Self {
        let mut text = vec![3_u16, '猫' as u16, 'A' as u16, 'B' as u16];
        let mut cells = vec![
            xlfn_sys::XLOPER12::number(1.0),
            xlfn_sys::XLOPER12 {
                value: xlfn_sys::XLOPER12Value {
                    string: text.as_mut_ptr(),
                },
                xltype: xlfn_sys::XLTYPE_STR,
            },
            xlfn_sys::XLOPER12::nil(),
        ]
        .into_boxed_slice();
        let raw = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                array: xlfn_sys::XLOPER12Array {
                    rows: 1,
                    columns: cells.len() as i32,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: xlfn_sys::XLTYPE_MULTI,
        };
        Self {
            runtime: get_benchmark_runtime(),
            raw,
            _storage: Some(Box::new((cells, text))),
        }
    }

    pub fn number_matrix(rows: usize, columns: usize) -> Self {
        let len = rows * columns;
        let mut cells = (0..len)
            .map(|i| xlfn_sys::XLOPER12::number(i as f64))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let raw = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                array: xlfn_sys::XLOPER12Array {
                    rows: rows as i32,
                    columns: columns as i32,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: xlfn_sys::XLTYPE_MULTI,
        };
        Self {
            runtime: get_benchmark_runtime(),
            raw,
            _storage: Some(Box::new(cells)),
        }
    }

    pub fn number_vec(len: usize) -> Self {
        // Excel worksheet rows support up to 1,048,576 elements while columns support up to 16,384.
        // We use an N x 1 column vector representation so 100k+ element 1D vectors fit within Excel dimensions.
        Self::number_matrix(len, 1)
    }

    pub fn handle() -> Self {
        let runtime = get_benchmark_runtime();
        let key = benchmark_revision_key("BENCH.INGRESS.HANDLE", 1);
        let token = runtime
            .with_formula_handle_service(|handle_runtime| {
                handle_runtime.prepare_observed(
                    key,
                    || Ok(BenchHandleObject { _payload: 42 }),
                    |_, _| Ok(()),
                )
            })
            .expect("benchmark handle runtime must initialize")
            .expect("benchmark handle preparation must succeed")
            .into_token();
        let mut u16_chars: Vec<u16> = Vec::with_capacity(token.len() + 1);
        u16_chars.push(token.len() as u16);
        u16_chars.extend(token.encode_utf16());
        let raw = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                string: u16_chars.as_ptr() as *mut u16,
            },
            xltype: xlfn_sys::XLTYPE_STR,
        };
        Self {
            runtime,
            raw,
            _storage: Some(Box::new(u16_chars)),
        }
    }

    pub fn run_plain<T>(&mut self)
    where
        T: for<'call> ExcelParameter<'call, crate::value::PlainInputMode>,
    {
        let ingress = benchmark_ingress();
        let call = self
            .runtime
            .enter(&ingress)
            .expect("benchmark runtime must be open");
        crate::call::with_excel_call_scope_and_call(&call, |call, scope| {
            let mut arguments =
                crate::value::ArgumentContext::<crate::value::PlainInputMode>::new(call, scope, 1);
            // SAFETY: self.raw points to valid benchmark storage that remains live.
            let value = unsafe {
                crate::value::argument_from_raw_with_arguments::<crate::value::PlainInputMode, T>(
                    &mut arguments,
                    0,
                    "arg",
                    &mut self.raw,
                )
            }
            .expect("benchmark raw argument ingress must succeed");
            std::hint::black_box(&value);
            let _ = arguments.finish();
        })
    }

    pub fn run_with_identity<T>(&mut self) -> [u8; 32]
    where
        T: for<'call> ExcelParameter<'call, crate::value::FormulaInputMode>,
    {
        let ingress = benchmark_ingress();
        let call = self
            .runtime
            .enter(&ingress)
            .expect("benchmark runtime must be open");
        crate::call::with_excel_call_scope_and_call(&call, |call, scope| {
            let mut arguments =
                crate::value::ArgumentContext::<crate::value::FormulaInputMode>::new(
                    call, scope, 1,
                );
            // SAFETY: self.raw points to valid benchmark storage that remains live.
            let value =
                unsafe {
                    crate::value::argument_from_raw_with_arguments::<
                        crate::value::FormulaInputMode,
                        T,
                    >(&mut arguments, 0, "arg", &mut self.raw)
                }
                .expect("benchmark raw argument ingress with identity must succeed");
            std::hint::black_box(&value);
            arguments
                .finish()
                .expect("formula revision fingerprint must finish")
                .expect("formula revision return must produce fingerprint")
        })
    }

    pub fn run_borrowed_str(&mut self) {
        let ingress = benchmark_ingress();
        let call = self
            .runtime
            .enter(&ingress)
            .expect("benchmark runtime must be open");
        crate::call::with_excel_call_scope_and_call(&call, |call, scope| {
            let mut arguments =
                crate::value::ArgumentContext::<crate::value::PlainInputMode>::new(call, scope, 1);
            // SAFETY: self.raw points to valid benchmark storage that remains live.
            let value =
                unsafe {
                    crate::value::argument_from_raw_with_arguments::<
                        crate::value::PlainInputMode,
                        &str,
                    >(&mut arguments, 0, "arg", &mut self.raw)
                }
                .expect("benchmark borrowed string ingress must succeed");
            std::hint::black_box(value);
            let _ = arguments.finish();
        })
    }

    pub fn run_borrowed_matrix_str(&mut self) {
        let ingress = benchmark_ingress();
        let call = self
            .runtime
            .enter(&ingress)
            .expect("benchmark runtime must be open");
        crate::call::with_excel_call_scope_and_call(&call, |call, scope| {
            let mut arguments =
                crate::value::ArgumentContext::<crate::value::PlainInputMode>::new(call, scope, 1);
            // SAFETY: self.raw points to valid benchmark storage that remains live.
            let value = unsafe {
                crate::value::argument_from_raw_with_arguments::<
                    crate::value::PlainInputMode,
                    crate::value::MatrixRef<'_, &str>,
                >(&mut arguments, 0, "arg", &mut self.raw)
            }
            .expect("benchmark borrowed string matrix ingress must succeed");
            std::hint::black_box(value);
            let _ = arguments.finish();
        })
    }

    pub fn run_borrowed_mixed_cells(&mut self) {
        let ingress = benchmark_ingress();
        let call = self
            .runtime
            .enter(&ingress)
            .expect("benchmark runtime must be open");
        crate::call::with_excel_call_scope_and_call(&call, |call, scope| {
            let mut arguments =
                crate::value::ArgumentContext::<crate::value::PlainInputMode>::new(call, scope, 1);
            // SAFETY: self.raw points to valid benchmark storage that remains live.
            let value = unsafe {
                crate::value::argument_from_raw_with_arguments::<
                    crate::value::PlainInputMode,
                    crate::value::MatrixRef<'_, crate::value::ExcelCellRef<'_>>,
                >(&mut arguments, 0, "arg", &mut self.raw)
            }
            .expect("benchmark borrowed mixed-cell ingress must succeed");
            std::hint::black_box(value);
            let _ = arguments.finish();
        })
    }

    pub fn run_handle_plain<T>(&mut self)
    where
        T: ExcelHandleObject,
    {
        let ingress = benchmark_ingress();
        let call = self
            .runtime
            .enter(&ingress)
            .expect("benchmark runtime must be open");
        crate::call::with_excel_call_scope_and_call(&call, |call, scope| {
            let mut arguments =
                crate::value::ArgumentContext::<crate::value::PlainInputMode>::new(call, scope, 1);
            // SAFETY: self.raw points to valid benchmark storage that remains live.
            let value = unsafe {
                crate::value::argument_from_raw_with_arguments::<
                    crate::value::PlainInputMode,
                    crate::handle::Handle<'_, T>,
                >(&mut arguments, 0, "arg", &mut self.raw)
            }
            .expect("benchmark raw handle ingress must succeed");
            std::hint::black_box(&value);
            let _ = arguments.finish();
        })
    }

    pub fn run_handle_with_identity<T>(&mut self) -> [u8; 32]
    where
        T: ExcelHandleObject,
    {
        let ingress = benchmark_ingress();
        let call = self
            .runtime
            .enter(&ingress)
            .expect("benchmark runtime must be open");
        crate::call::with_excel_call_scope_and_call(&call, |call, scope| {
            let mut arguments =
                crate::value::ArgumentContext::<crate::value::FormulaInputMode>::new(
                    call, scope, 1,
                );
            // SAFETY: self.raw points to valid benchmark storage that remains live.
            let value = unsafe {
                crate::value::argument_from_raw_with_arguments::<
                    crate::value::FormulaInputMode,
                    crate::handle::Handle<'_, T>,
                >(&mut arguments, 0, "arg", &mut self.raw)
            }
            .expect("benchmark raw handle ingress with identity must succeed");
            std::hint::black_box(&value);
            arguments
                .finish()
                .expect("formula revision fingerprint must finish")
                .expect("formula revision return must produce fingerprint")
        })
    }
}
