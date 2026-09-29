//! Raw ingress + real formula publication. No Excel/COM calls are timed.
use super::*;
use crate::value::input::PreparedArgument;
use crate::value::prepared_probe::{InputKind, Owned};

enum Prepared<'call> {
    Number(PreparedArgument<f64, f64>),
    Numbers(
        PreparedArgument<
            Matrix<f64>,
            <Matrix<f64> as crate::value::input::ExcelParameter<
                'call,
                crate::value::FormulaInputMode,
            >>::Prepared,
        >,
    ),
    Strings(
        PreparedArgument<
            Matrix<String>,
            <Matrix<String> as crate::value::input::ExcelParameter<
                'call,
                crate::value::FormulaInputMode,
            >>::Prepared,
        >,
    ),
}
impl Prepared<'_> {
    fn materialize(self) -> crate::XllResult<Owned> {
        match self {
            Self::Number(value) => value.materialize().map(Owned::Number),
            Self::Numbers(value) => value.materialize().map(Owned::Numbers),
            Self::Strings(value) => value.materialize().map(Owned::Strings),
        }
    }
}

pub struct TwoPhaseBenchmark {
    input: RawArgumentIngressBenchmark,
    kind: InputKind,
    handles: FormulaHandleService,
    pub factory_calls: usize,
    pub materializations: usize,
    revision: u32,
}

impl TwoPhaseBenchmark {
    pub fn new(kind: InputKind, elements: usize) -> Self {
        let input = match kind {
            InputKind::Number => RawArgumentIngressBenchmark::number(42.0),
            InputKind::Numbers => RawArgumentIngressBenchmark::number_matrix(elements, 1),
            InputKind::Strings => {
                RawArgumentIngressBenchmark::string_matrix(&vec!["日本語💡"; elements])
            }
        };
        Self {
            input,
            kind,
            handles: FormulaHandleService::try_new(8).unwrap(),
            factory_calls: 0,
            materializations: 0,
            revision: 0,
        }
    }

    pub fn reset(&self) {
        self.handles.terminate_all_topics();
    }

    /// Changes the final cell, preserving the shape and all other cells.
    pub fn change_last(&mut self) {
        self.revision += 1;
        self.set_last(
            self.revision as f64,
            if self.revision.is_multiple_of(2) {
                0x65e5
            } else {
                0x6708
            },
        );
    }

    fn set_last(&mut self, number: f64, unit: u16) {
        match self.kind {
            InputKind::Number => self.input.raw = xlfn_sys::XLOPER12::number(number),
            InputKind::Numbers => {
                let cells = self
                    .input
                    ._storage
                    .as_mut()
                    .unwrap()
                    .downcast_mut::<Vec<xlfn_sys::XLOPER12>>()
                    .unwrap();
                *cells.last_mut().unwrap() = xlfn_sys::XLOPER12::number(number);
                self.input.raw = array_root(cells.len(), 1, cells);
            }
            InputKind::Strings => {
                let (cells, strings) = self
                    .input
                    ._storage
                    .as_mut()
                    .unwrap()
                    .downcast_mut::<(Vec<xlfn_sys::XLOPER12>, Vec<Vec<u16>>)>()
                    .unwrap();
                strings.last_mut().unwrap()[1] = unit;
                // Refresh raw projections after taking mutable ownership borrows.
                for (cell, string) in cells.iter_mut().zip(strings.iter_mut()) {
                    *cell = xlfn_sys::XLOPER12 {
                        value: xlfn_sys::XLOPER12Value {
                            string: string.as_mut_ptr(),
                        },
                        xltype: xlfn_sys::XLTYPE_STR,
                    };
                }
                self.input.raw = array_root(1, cells.len(), cells);
            }
        }
    }

    pub fn run(&mut self, two_phase: bool) -> crate::XllResult<String> {
        let ingress = benchmark_ingress();
        let call = self.input.runtime.enter(&ingress)?;
        crate::call::with_excel_call_scope_and_call(&call, |call, scope| {
            let (fingerprint, prepared, owned) =
                if two_phase {
                    let mut arguments = crate::value::ArgumentContext::<
                        crate::value::FormulaInputMode,
                    >::new(call, scope, 1);
                    // SAFETY: fixture storage stays live through initializer execution.
                    let raw = unsafe { crate::value::XlValueRef::from_raw(&mut self.input.raw) }?;
                    let prepared = match self.kind {
                        InputKind::Number => Prepared::Number(arguments.prepare(0, "arg", raw)?),
                        InputKind::Numbers => Prepared::Numbers(arguments.prepare(0, "arg", raw)?),
                        InputKind::Strings => Prepared::Strings(arguments.prepare(0, "arg", raw)?),
                    };
                    (
                        InputFingerprint::from_bytes(arguments.finish()?.unwrap()),
                        Some(prepared),
                        None,
                    )
                } else {
                    let mut arguments = crate::value::ArgumentContext::<
                        crate::value::FormulaInputMode,
                    >::new(call, scope, 1);
                    // SAFETY: owned fixture storage lives through conversion.
                    let raw = unsafe { crate::value::XlValueRef::from_raw(&mut self.input.raw) }?;
                    let owned = match self.kind {
                        InputKind::Number => Owned::Number(arguments.decode::<f64>(0, "arg", raw)?),
                        InputKind::Numbers => {
                            Owned::Numbers(arguments.decode::<Matrix<f64>>(0, "arg", raw)?)
                        }
                        InputKind::Strings => {
                            Owned::Strings(arguments.decode::<Matrix<String>>(0, "arg", raw)?)
                        }
                    };
                    self.materializations += 1;
                    (
                        InputFingerprint::from_bytes(arguments.finish()?.unwrap()),
                        None,
                        Some(owned),
                    )
                };
            let key = HandleTopicKey::Formula(FormulaRevisionKey::new(
                FormulaCaller {
                    sheet_id: 1,
                    row: 1,
                    column: 1,
                },
                "BENCH.TWO_PHASE",
                fingerprint,
            ));
            self.handles
                .prepare_observed(
                    key,
                    || {
                        self.factory_calls += 1;
                        let value = if let Some(prepared) = prepared {
                            self.materializations += 1;
                            prepared.materialize()?
                        } else {
                            owned.unwrap()
                        };
                        std::hint::black_box(value);
                        Ok(BenchHandleObject { _payload: 0 })
                    },
                    |_, _| Ok(()),
                )
                .map(|prepared| prepared.into_token())
        })
    }
}

fn array_root(rows: usize, columns: usize, cells: &mut [xlfn_sys::XLOPER12]) -> xlfn_sys::XLOPER12 {
    xlfn_sys::XLOPER12 {
        value: xlfn_sys::XLOPER12Value {
            array: xlfn_sys::XLOPER12Array {
                rows: rows as i32,
                columns: columns as i32,
                values: cells.as_mut_ptr(),
            },
        },
        xltype: xlfn_sys::XLTYPE_MULTI,
    }
}

impl Drop for TwoPhaseBenchmark {
    fn drop(&mut self) {
        super::super::handle::cleanup_handle_runtime(&self.handles);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The benchmark intentionally keeps a process-lifetime runtime open.
    // Isolate native harness cases so that it cannot retain diagnostic-service
    // admission across unrelated lifecycle tests in a serialized libtest run.
    fn run_isolated(name: &str) -> bool {
        #[cfg(not(miri))]
        {
            const ENV: &str = "XLFN_TWO_PHASE_TEST";
            if std::env::var(ENV).as_deref() != Ok(name) {
                let full = format!("benchmark_support::ingress::two_phase::tests::{name}");
                let result = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &full, "--test-threads=1"])
                    .env(ENV, name)
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                return true;
            }
        }
        #[cfg(miri)]
        let _ = name;
        false
    }

    #[test]
    fn two_phase_identity_and_materialization_match_eager_ingress() {
        if run_isolated("two_phase_identity_and_materialization_match_eager_ingress") {
            return;
        }
        for (kind, elements) in [
            (InputKind::Number, 1),
            (InputKind::Numbers, 1000),
            (InputKind::Strings, 100),
        ] {
            let mut fixture = TwoPhaseBenchmark::new(kind, elements);
            let first = fixture.run(false).unwrap();
            let materializations = fixture.materializations;
            assert_eq!(first, fixture.run(true).unwrap());
            assert_eq!(fixture.factory_calls, 1);
            assert_eq!(fixture.materializations, materializations);
            fixture.change_last();
            let second = fixture.run(true).unwrap();
            assert_ne!(first, second);
            assert_eq!(fixture.factory_calls, 2);
            assert_eq!(fixture.materializations, materializations + 1);
            assert_eq!(second, fixture.run(false).unwrap());
        }
    }

    #[test]
    fn invalid_last_cell_is_rejected_before_warm_lookup() {
        if run_isolated("invalid_last_cell_is_rejected_before_warm_lookup") {
            return;
        }
        for kind in [InputKind::Numbers, InputKind::Strings] {
            let mut fixture = TwoPhaseBenchmark::new(kind, 3);
            fixture.run(false).unwrap();
            fixture.set_last(f64::INFINITY, 0xd800);
            let eager = fixture.run(false).unwrap_err();
            let prepared = fixture.run(true).unwrap_err();
            assert_eq!(format!("{eager:?}"), format!("{prepared:?}"));
            assert_eq!(fixture.factory_calls, 1);
        }
    }
}
