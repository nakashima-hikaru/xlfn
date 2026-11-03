// module: kernel/lib
use crate::thread_affine::ThreadAffineSlot;

type AnyLifetimeFn = for<'a> fn(&'a ());
type StaticOnlyFn = fn(&'static ());

fn retag(slot: &ThreadAffineSlot<AnyLifetimeFn>) -> &ThreadAffineSlot<StaticOnlyFn> {
    slot // error: E0308
}
