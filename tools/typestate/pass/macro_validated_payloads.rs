// module: macros/lib
fn observe(plan: &super::model::UdfPlan) {
    if let Some(context) = plan.execution.context() {
        let _ = (context.ty(), context.addin());
    }
    for argument in &plan.arguments {
        let _ = argument.abi();
        if let Some(presence) = argument.conversion.checked_presence() {
            let _ = (&presence.blank, &presence.missing);
        }
    }
}
