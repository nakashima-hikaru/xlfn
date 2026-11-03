use xlfn::value::MatrixRef;

fn escape<'call>(matrix: MatrixRef<'call, f64>) -> Option<&'static f64> {
    matrix.get(0, 0)
}

fn main() {}
