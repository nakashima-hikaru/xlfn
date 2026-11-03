// module: runtime
use super::{CanonicalCompletion, FinalRemoval};
use crate::lifecycle::{LifecycleAccess, LifecycleControl};

fn invalid<A: crate::Addin>(lifecycle: &LifecycleControl<'_, A>, control: &mut LifecycleAccess<'_, A>, completion: CanonicalCompletion<'_, A, FinalRemoval>) {
    let _ = lifecycle.finish_final_removal(control, completion);
    let _ = lifecycle.finish_final_removal(control, completion); // error: E0382
}
