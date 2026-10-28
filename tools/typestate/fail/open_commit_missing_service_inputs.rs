// module: runtime/open_txn
use super::{OpenCore, commit_inner};

fn missing_inputs<A: crate::Addin>(
    deps: crate::runtime::capabilities::OpenDeps<'_, A>,
    core: OpenCore<A>,
    journal: &mut crate::registration::HostMutationJournal,
) {
    let _ = commit_inner(deps, core, journal); // error: E0308
}
