// module: lifecycle/state
fn invalid<A: crate::Addin>(resources: super::CloseResources<A>) {
    resources.finish_closed(); // error: E0599
}
