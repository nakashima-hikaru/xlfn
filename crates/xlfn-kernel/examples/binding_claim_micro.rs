use std::hint::black_box;
use std::time::Instant;
use xlfn_kernel::thread_affine::{BindingState, ThreadAffineSlot};
fn main() {
    let slot = ThreadAffineSlot::<u64>::new();
    const N: usize = 500_000;
    let start = Instant::now();
    for i in 0..N {
        let access = slot.bind_current().unwrap();
        let BindingState::Empty(txn) = slot.binding(&access).unwrap() else {
            panic!("empty")
        };
        let txn = txn.install(black_box(i as u64)).unwrap();
        let (v, txn) = txn.take().unwrap();
        black_box(v);
        txn.release().unwrap();
    }
    println!("{:.3}", start.elapsed().as_nanos() as f64 / N as f64);
}
