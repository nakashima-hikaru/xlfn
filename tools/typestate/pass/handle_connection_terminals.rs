// module: excel_rtd/windows/server
fn ordered(runtime: &crate::handle::FormulaHandleService, generation: crate::handle::FormulaLifetimeGeneration, key: &str) -> crate::XllResult<()> {
    let connection = runtime.connect_transaction(generation, 1, key)?;
    let _token = connection.token();
    connection.rollback();
    runtime.connect_transaction(generation, 1, key)?.commit()
}
