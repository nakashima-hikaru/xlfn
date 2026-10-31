// module: kernel/lib
use crate::thread_affine::ThreadAffineAccess;

type AnyLifetimeFn = for<'a> fn(&'a ());
type StaticOnlyFn = fn(&'static ());

fn retag<'slot>(
    access: ThreadAffineAccess<'slot, AnyLifetimeFn>,
) -> ThreadAffineAccess<'slot, StaticOnlyFn> {
    access // error: E0308
}
