// module: runtime
use crate::generation::RuntimeGeneration;
use crate::lifecycle::{LifecycleAccess, LifecycleControl};

fn invalid<A: crate::Addin>(authority: &LifecycleControl<'_, A>, access: &mut LifecycleAccess<'_, A>, generation: RuntimeGeneration) {
    let committed = authority.finish_open_state(access, generation).unwrap();
    access.wait_for_change(); // error: E0499
    drop(committed);
}
