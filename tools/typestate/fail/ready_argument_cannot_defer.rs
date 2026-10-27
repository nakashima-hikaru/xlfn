// module: value
use super::input::{PreparedArgument, ReadyArgument};

fn invalid() {
    let _: ReadyArgument<f64> = PreparedArgument::prepared(()); // error: E0308
}
