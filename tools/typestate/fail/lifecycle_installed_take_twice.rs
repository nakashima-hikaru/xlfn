// module: runtime
use super::capabilities::InstalledLifecycle;

fn invalid<A: crate::Addin>(installed: InstalledLifecycle<'_, A>) {
    let _first = installed.take();
    let _second = installed.take(); // error: E0382
}
