// module: kernel/lib
use crate::thread_affine::BindingState;

type AnyLifetimeFn = for<'a> fn(&'a ());
type StaticOnlyFn = fn(&'static ());

fn retag<'slot>(state: BindingState<'slot, AnyLifetimeFn>) -> BindingState<'slot, StaticOnlyFn> {
    state // error: E0308
}
