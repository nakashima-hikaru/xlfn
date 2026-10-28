// module: handle
use super::runtime::{FormulaHandleService, FormulaHandleServiceSealed};

fn invalid(generation: crate::generation::RuntimeGeneration, service: Box<FormulaHandleService>) {
    let _ = FormulaHandleServiceSealed { generation, service }; // error: E0451
}
