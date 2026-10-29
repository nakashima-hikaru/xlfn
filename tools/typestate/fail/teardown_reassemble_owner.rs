// module: runtime/shutdown/pipeline
use super::{ExecutionDrained, TeardownTxn};
use crate::runtime::capabilities::ShutdownDeps;
use crate::runtime::shutdown::{FinalRemoval, RemovalOwner};

fn invalid<'runtime, A: crate::Addin>(
    deps: ShutdownDeps<'runtime, A>,
    owner: RemovalOwner<'runtime, A, FinalRemoval>,
    stage: ExecutionDrained,
) {
    let _: TeardownTxn<'runtime, A, FinalRemoval, ExecutionDrained> =
        TeardownTxn::new(deps, owner, stage); // error: E0599
}
