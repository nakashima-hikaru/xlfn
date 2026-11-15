// module: runtime
use crate::lifecycle::{CanonicalGenerationPublished, LifecycleAccess, LifecycleControl};

fn invalid<A: crate::Addin>(authority: &LifecycleControl<'_, A>, access: &mut LifecycleAccess<'_, A>, published: CanonicalGenerationPublished<'_, A>) {
    let committed = authority.finish_open_state(access, published).unwrap();
    access.wait_for_change(); // error: E0499
    drop(committed);
}
