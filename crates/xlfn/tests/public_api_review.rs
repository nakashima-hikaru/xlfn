//! Public caller contracts established before the 0.2 release.
use std::error::Error;
use xlfn::error::{IntoXllError, UdfError};
use xlfn::prelude::*;
use xlfn::value::{CalendarDate, ExcelDateSystem, Shape};

struct ReviewAddin;
#[excel_addin(name = "API review", id = "api-review")]
impl Addin for ReviewAddin {
    type SharedState = ();
    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

type Price = f64;
#[excel_function]
fn ordinary(value: f64) -> f64 {
    value
}
#[excel_function]
fn aliased(value: Price) -> f64 {
    value
}
#[excel_function]
fn native_alias(#[excel_arg(native = f64)] value: Price) -> f64 {
    value
}

type Context<'a, A> = xlfn::addin::MainThreadContext<'a, A>;
#[excel_function]
fn contextual(#[excel_context(main_thread)] _context: Context<'_, ReviewAddin>) -> f64 {
    1.0
}

#[test]
fn spelling_does_not_change_the_default_scalar_abi() {
    type Value = *mut xlfn::__private::v1::XLOPER12;
    let _: unsafe extern "system" fn(Value) -> Value = xll_ordinary;
    let _: unsafe extern "system" fn(Value) -> Value = xll_aliased;
    let _: unsafe extern "system" fn(f64) -> Value = xll_native_alias;
    let _: unsafe extern "system" fn() -> Value = xll_contextual;
}

#[test]
fn application_causes_survive_as_bounded_boundary_snapshots() {
    let error = UdfError::new(ExcelError::NotAvailable, "market data unavailable")
        .with_source(std::io::Error::other("upstream disconnected"));
    assert!(
        error
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .is_some()
    );
    let snapshot = error.into_xll_error();
    assert_eq!(snapshot.to_excel_error(), ExcelError::NotAvailable);
    assert_eq!(
        snapshot.source().unwrap().to_string(),
        "upstream disconnected"
    );
    assert_eq!(
        snapshot.clone().source().unwrap().to_string(),
        "upstream disconnected"
    );
}

#[derive(Debug)]
struct Cycle;
impl std::fmt::Display for Cycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&"界".repeat(2000))
    }
}
impl Error for Cycle {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self)
    }
}

#[test]
fn cyclic_and_large_application_causes_have_fixed_storage_bounds() {
    let snapshot = UdfError::new(ExcelError::Value, "界".repeat(4000))
        .with_source(Cycle)
        .into_xll_error();
    assert!(snapshot.to_string().len() <= 4096);
    let mut count = 0;
    let mut source = snapshot.source();
    while let Some(cause) = source {
        count += 1;
        assert!(cause.to_string().len() <= 1024);
        source = cause.source();
    }
    assert_eq!(count, 8);
}

#[test]
fn completed_arrays_and_dates_have_named_shapes() {
    let mixed = xlfn::output::ExcelArrayOutput::from_f64(1, 2, &[1.0, 2.0]).unwrap();
    let numeric = xlfn::output::NumericArrayOutput::from_f64(1, 2, &[1.0, 2.0]).unwrap();
    assert_eq!(
        mixed.shape(),
        Shape {
            rows: 1,
            columns: 2
        }
    );
    assert_eq!(mixed.shape(), numeric.shape());
    assert!(xlfn::output::ExcelArrayOutput::from_f64(1, 2, &[f64::NAN, 1.0]).is_err());
    assert!(xlfn::output::NumericArrayOutput::from_f64(1, 2, &[f64::NAN, 1.0]).is_err());
    let serial = ExcelSerialDate::from_ymd(2026, 10, 5, ExcelDateSystem::Windows1900).unwrap();
    assert_eq!(
        serial.to_ymd().unwrap(),
        CalendarDate {
            year: 2026,
            month: 10,
            day: 5
        }
    );
}
