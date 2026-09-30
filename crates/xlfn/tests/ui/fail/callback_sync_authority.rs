static RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

fn main() {
    let _ = xlfn::__private::v1::sync_udf::<(), f64, _>(
        &RUNTIME,
        "mint",
        "MINT",
        0,
        |state, frame| {
            let _context = xlfn::__private::v1::macro_sheet_context::<(), _>(frame, state);
            unreachable!()
        },
    );
}
