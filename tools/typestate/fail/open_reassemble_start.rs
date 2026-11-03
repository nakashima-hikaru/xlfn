// module: runtime
use super::capabilities::OpenDeps;
use super::open_txn::{Begun, OpeningTxn};
use crate::generation::OpenAttemptId;
use crate::module_runtime::ModuleOpening;

fn invalid<A: crate::Addin>(deps: OpenDeps<'_, A>, attempt: OpenAttemptId, module: ModuleOpening) {
    let _: OpeningTxn<'_, A, Begun<A>> = OpeningTxn::new_begun(deps, attempt, module); // error: E0599
}
