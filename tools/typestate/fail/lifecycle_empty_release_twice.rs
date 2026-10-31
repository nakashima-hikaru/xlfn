// module: runtime
use super::capabilities::EmptyLifecycle;

fn invalid<A: crate::Addin>(empty: EmptyLifecycle<'_, A>) {
    let _first = empty.release();
    let _second = empty.release(); // error: E0382
}
