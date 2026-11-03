// module: runtime
use crate::generation::OpenAttemptId;
use crate::lifecycle::{CommittedOpen, LifecycleAccess};

fn invalid<A: crate::Addin>(access: &LifecycleAccess<'_, A>, attempt: OpenAttemptId) {
    let _ = CommittedOpen { _access: access, attempt }; // error: E0451
}
