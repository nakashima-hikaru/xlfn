// module: runtime
use super::capabilities::TakenLifecycle;

fn invalid<A: crate::Addin>(taken: TakenLifecycle<'_, A>) {
    let _empty = taken.empty; // error: E0616
}
