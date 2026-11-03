// module: value
use super::input::{FormulaInputMode, PreparedArgument, PreparedParameter, ReadyArgument};

fn ready() -> crate::XllResult<f64> {
    ReadyArgument::<f64>::Ready(7.0).materialize()
}

fn deferred<'call>() -> crate::XllResult<f64> {
    let prepared: PreparedParameter<'call, f64, FormulaInputMode> = PreparedArgument::prepared(7.0);
    prepared.materialize()
}
