// module: macros/lib
use super::model::UdfPlan;
fn invalid(mut plan: UdfPlan) {
    plan.return_abi = xlfn_common::ReturnAbi::NumericArray12; // error: E0594
}
