// module: macros/lib
use super::model::{UdfPlan, UdfSpec};
fn observe(spec: &UdfSpec, plan: &UdfPlan) {
    let _ = (&spec.execution, &plan.execution, plan.return_abi);
}
fn lower(spec: UdfSpec) -> UdfPlan { super::model::lower(spec) }
