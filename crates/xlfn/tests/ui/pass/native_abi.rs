use xlfn::prelude::*;

struct TestAddin;

#[excel_addin(name = "Native ABI", id = "native-abi", category = "Test")]
impl Addin for TestAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

#[excel_function(thread_safe)]
fn native(number: f64, enabled: bool, count: i32, text: String, borrowed: &str) -> f64 {
    number + f64::from(count) + if enabled { (text.len() + borrowed.len()) as f64 } else { 0.0 }
}

#[excel_function]
fn strict(
    #[excel_arg(strict)] number: f64,
    #[excel_arg(strict)] enabled: bool,
    #[excel_arg(strict)] count: i32,
    #[excel_arg(strict)] text: String,
    #[excel_arg(strict)] borrowed: &str,
) -> f64 {
    native(number, enabled, count, text, borrowed)
}

#[excel_function]
fn presence(
    #[excel_arg(default = 1.0)] number: f64,
    #[excel_arg(blank = convert)] enabled: bool,
    #[excel_arg(missing = error)] count: i32,
    #[excel_arg(missing = "convert")] text: String,
) -> f64 {
    number + f64::from(count) + if enabled { text.len() as f64 } else { 0.0 }
}

type NumberAlias = f64;

#[excel_function]
fn fallback(number: NumberAlias, count: i64, optional: Option<f64>) -> f64 {
    number + count as f64 + optional.unwrap_or_default()
}

macro_rules! forward_type {
    ($ty:ty) => {
        #[excel_function]
        fn forwarded(number: $ty) -> f64 { number }
    };
}
forward_type!(::core::primitive::f64);

#[excel_function(input_semantics = strict)]
fn strict_function(number: NumberAlias, scalar: f64, text: String, values: NumericMatrixRef<'_>) -> f64 {
    number + scalar + text.len() as f64 + values.iter().sum::<f64>()
}

fn main() {
    use xlfn::__private::v1::XLOPER12;
    type Value = *mut XLOPER12;
    let _: unsafe extern "system" fn(f64, i16, i32, *const u16, *const u16) -> Value = xll_native;
    let _: unsafe extern "system" fn(Value, Value, Value, Value, Value) -> Value = xll_strict;
    let _: unsafe extern "system" fn(Value, Value, Value, Value) -> Value = xll_presence;
    let _: unsafe extern "system" fn(Value, Value, Value) -> Value = xll_fallback;
    let _: unsafe extern "system" fn(f64) -> Value = xll_forwarded;
    let _: unsafe extern "system" fn(Value, Value, Value, *const xlfn::__private::v1::FP12) -> Value = xll_strict_function;
}
