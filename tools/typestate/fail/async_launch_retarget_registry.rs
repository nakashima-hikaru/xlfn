// module: async_udf
use crate::runtime::AsyncCallGuard;

fn invalid<A: crate::Addin>(guard: &mut AsyncCallGuard<'_, A>, other: &'static super::AsyncRuntime<A::AsyncExecutor>) {
    guard.async_runtime = other; // error: E0616
}
