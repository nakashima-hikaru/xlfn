// module: macros/lib
use super::model::UdfSpec;
fn invalid(mut spec: UdfSpec) {
    spec.execution = super::model::ExecutionSpec::ThreadSafe { context_ty: None }; // error: E0594
}
