// module: runtime/open_txn
use super::{InitializedOpenCore, ServiceInputs};

fn duplicate_inputs<A: crate::Addin>(core: InitializedOpenCore<A>, inputs: ServiceInputs<A>) {
    let _ = core.initialized(inputs); // error: E0599
}
