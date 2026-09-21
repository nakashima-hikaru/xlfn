//! Shared destruction-completion ordering; effects retain their backend semantics.
macro_rules! complete_reclamation {
    ($receipt:ident, $guard:ident;
     $destroy:expr, $discharge:expr, $lock:expr, $notify:expr, $unlock:expr) => {{
        let $receipt = $destroy;
        $discharge;
        let $guard = $lock;
        $notify;
        $unlock;
    }};
}
pub(crate) use complete_reclamation;
