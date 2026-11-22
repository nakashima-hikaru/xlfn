// module: runtime/open_txn
use super::{InitializedOpenCore, RawServiceInputs};

fn duplicate_inputs<A: crate::Addin>(core: InitializedOpenCore<A>, inputs: RawServiceInputs<A>) {
    let _ = core.initialized(inputs); // error: E0599
}
