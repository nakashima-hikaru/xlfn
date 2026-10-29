use xlfn::output::{XlArrayBuilder, XlArrayOutput};
use xlfn::prelude::*;
use xlfn::value::XlArrayRef;

#[excel_function(name = "TEST.COW.TEXT", thread_safe)]
fn cow_text(value: &str) -> std::borrow::Cow<'_, str> {
    std::borrow::Cow::Borrowed(value)
}

#[excel_function(name = "TEST.COW.OWNED", thread_safe)]
fn cow_owned() -> std::borrow::Cow<'static, str> {
    std::borrow::Cow::Owned(String::from("価格 💡"))
}

#[allow(dead_code)]
fn array() -> xlfn::XllResult<XlArrayOutput> {
    let mut out = XlArrayBuilder::new(1, 2)?;
    out.push_f64(1.0)?;
    out.push("two")?;
    out.finish()
}

struct State;

struct OutputArrayAddin;

#[excel_addin(name = "Output Array", id = "output-array", category = "Test")]
impl Addin for OutputArrayAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(
        _: &OpenContext<Self>,
    ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State))
    }
}

#[excel_function(name = "TEST.BORROWED.ARRAY", thread_safe)]
fn sum(values: XlArrayRef<'_>) -> XllResult<f64> {
    values
        .cells()
        .try_fold(0.0, |sum, cell| Ok(sum + cell.as_f64()?))
}

#[excel_function(name = "TEST.DIRECT.ARRAY", thread_safe)]
fn normalize(values: XlArrayRef<'_>) -> XllResult<XlArrayOutput> {
    let mean = values
        .cells()
        .try_fold(0.0, |sum, cell| -> XllResult<f64> {
            Ok(sum + cell.as_f64()?)
        })?
        / values.len() as f64;
    let xlfn::error::Shape { rows, columns } = values.shape();
    let mut output = XlArrayBuilder::new(rows, columns)?;
    for cell in values.cells() {
        output.push_f64(cell.as_f64()? - mean)?;
    }
    output.finish()
}

fn main() {}
