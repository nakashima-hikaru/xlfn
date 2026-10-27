// module: value
use super::input::ReadyArgument;

fn invalid(prepared: ReadyArgument<f64>) {
    let _ = prepared.materialize();
    let _ = prepared.materialize(); // error: E0382
}
