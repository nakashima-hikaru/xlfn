fn main() {
    let state = ();
    xlfn::__private::v1::with_excel_call_scope_and_state(&state, |state, scope| {
        let _context = xlfn::MacroSheetContext::<()>::new(state, scope);
    });
}
