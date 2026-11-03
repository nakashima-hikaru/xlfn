// module: macros/lib
fn retag(argument: &mut super::model::ArgumentPlan) {
    argument.abi = xlfn_common::ArgumentAbi::Reference12; // error: E0615
}
