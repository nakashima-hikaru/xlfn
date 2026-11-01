// module: macros/lib
use super::model::UdfPlan;
fn invalid(plan: UdfPlan) {
    let _ = plan.data; // error: E0616
}
