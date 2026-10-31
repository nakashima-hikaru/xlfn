// module: runtime
use super::capabilities::TakenLifecycle;

fn invalid<A: crate::Addin>(taken: TakenLifecycle<'_, A>) {
    taken.release(); // error: E0599
}
