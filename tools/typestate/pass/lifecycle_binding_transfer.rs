// module: runtime
use super::capabilities::InstalledLifecycle;

fn dispose<A: crate::Addin>(installed: InstalledLifecycle<'_, A>) -> Option<()> {
    installed.take().ok()?.dispose().ok()?.release().ok()
}
