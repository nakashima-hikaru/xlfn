use xlfn::prelude::*;
use xlfn::value::IntoExcel;

#[allow(non_camel_case_types, reason = "exercise a reserved Rust keyword as an add-in name")]
struct r#gen;

#[excel_addin]
impl r#Addin for r#gen {
    type SharedState = ();
    type LifecycleState = ();
    type r#Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext<Self>) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(()))
    }
}

#[allow(non_camel_case_types, reason = "exercise raw enum variant names")]
#[derive(ExcelEnum)]
enum Keyword {
    r#type,
    #[excel_enum(name = "r#match")]
    Match,
}

#[excel_function]
fn r#type(r#match: f64) -> f64 {
    r#match
}

#[excel_function]
fn explicit_context(
    #[excel_context(r#main_thread)] _context: MainThreadContext<'_, r#gen>,
) -> f64 {
    4.0
}

fn main() {
    assert_eq!(r#type(3.0), 3.0);
    assert!(matches!(
        Keyword::r#type.into_excel().unwrap(),
        xlfn::value::ExcelCellOutput::String(text) if text == "type"
    ));
    assert!(matches!(
        Keyword::Match.into_excel().unwrap(),
        xlfn::value::ExcelCellOutput::String(text) if text == "r#match"
    ));
}
