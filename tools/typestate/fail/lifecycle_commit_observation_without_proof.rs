// module: runtime
use crate::lifecycle::LifecycleAccess;

fn invalid<A: crate::Addin>(observer: &super::observer::RuntimeObserver, deps: &super::OpenDeps<'_, A>, access: &LifecycleAccess<'_, A>) {
    observer.commit_open(deps, access); // error: E0308
}
