// module: runtime/shutdown/pipeline
use crate::runtime::capabilities::{OpenDeps, RollbackRequest};
use crate::generation::RuntimeGeneration;
fn invalid<'a, A: crate::Addin>(deps: OpenDeps<'a, A>, generation: RuntimeGeneration) {
    let _ = RollbackRequest { deps, generation, installed: None }; // error: E0451
}
