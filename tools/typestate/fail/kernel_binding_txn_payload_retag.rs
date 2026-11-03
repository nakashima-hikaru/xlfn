// module: kernel/lib
use crate::thread_affine::{BindingOccupied, BindingTxn};

type AnyLifetimeFn = for<'a> fn(&'a ());
type StaticOnlyFn = fn(&'static ());

fn retag<'slot>(
    transaction: BindingTxn<'slot, AnyLifetimeFn, BindingOccupied>,
) -> BindingTxn<'slot, StaticOnlyFn, BindingOccupied> {
    transaction // error: E0308
}
