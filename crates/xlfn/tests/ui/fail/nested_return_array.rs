use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function(name = "FAIL.NESTED.ARRAY")]
fn bad() -> Matrix<Matrix<f64>> {
    Matrix::new(
        1,
        1,
        vec![Matrix::new(1, 1, vec![1.0]).expect("valid inner matrix")],
    )
    .expect("valid outer matrix")
}

fn main() {}
