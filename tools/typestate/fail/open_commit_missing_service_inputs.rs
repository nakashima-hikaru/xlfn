// module: runtime/open_txn
use super::{HostOpeningState, InitializedOpenCore, OpenCore, commit_inner};
use crate::runtime::capabilities::{InstalledLifecycle, OpenDeps};

fn missing_inputs<'runtime, A: crate::Addin>(
    deps: OpenDeps<'runtime, A>,
    core: OpenCore<A>,
    host: HostOpeningState,
    binding: InstalledLifecycle<'runtime, A>,
) {
    let _ = commit_inner(deps, core, host, binding); // error: E0308
}

fn unprepared_inputs<'runtime, A: crate::Addin>(
    deps: OpenDeps<'runtime, A>,
    core: InitializedOpenCore<A>,
    host: HostOpeningState,
    binding: InstalledLifecycle<'runtime, A>,
) {
    let _ = commit_inner(deps, core, host, binding); // error: E0308
}
