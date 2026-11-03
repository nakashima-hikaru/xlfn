// module: runtime
use crate::lifecycle::CompletedRemovalIntent;

fn invalid<A: crate::Addin>(intent: CompletedRemovalIntent<'_, A>) {
    intent.clear();
    intent.clear(); // error: E0382
}
