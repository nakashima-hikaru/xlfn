use std::{alloc::{GlobalAlloc, Layout, System}, cell::RefCell, hint::black_box, mem::ManuallyDrop, sync::atomic::{AtomicUsize, Ordering}, time::Instant};
use smallvec::SmallVec;
struct Count;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 { ALLOCS.fetch_add(1, Ordering::Relaxed); unsafe { System.alloc(layout) } }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) { unsafe { System.dealloc(ptr, layout) } }
    unsafe fn realloc(&self, ptr: *mut u8, old: Layout, new: usize) -> *mut u8 { ALLOCS.fetch_add(1, Ordering::Relaxed); unsafe { System.realloc(ptr, old, new) } }
}
#[global_allocator] static ALLOC: Count = Count;
thread_local! { static STACK: ManuallyDrop<RefCell<SmallVec<[usize;1]>>> = const { ManuallyDrop::new(RefCell::new(SmallVec::new_const())) }; }
struct Pop(usize);
impl Drop for Pop { fn drop(&mut self) { STACK.with(|s| { let mut s = s.borrow_mut(); assert_eq!(s.pop(), Some(self.0)); if s.is_empty() && s.spilled() { *s = SmallVec::new(); } }); } }
fn with_small<R>(id: usize, f: impl FnOnce() -> R) -> Option<R> { STACK.with(|s| { let mut s=s.borrow_mut(); if s.contains(&id) { return None; } s.push(id); Some(()) })?; let _guard=Pop(id); Some(f()) }
#[inline(never)] fn intr(depth: usize) { if depth != 0 { lifecycle_stack_model::with_lifecycle(depth, |_| intr(depth-1)).unwrap(); } else {black_box(());} }
#[inline(never)] fn small(depth: usize) { if depth != 0 { with_small(depth, || small(depth-1)).unwrap(); } else {black_box(());} }
fn main() {
    const N: usize=200_000;
    println!("smallvec TLS bytes={} intrusive TLS head bytes={}", std::mem::size_of::<ManuallyDrop<RefCell<SmallVec<[usize;1]>>>>(), std::mem::size_of::<std::cell::Cell<*const ()>>());
    for depth in [1,2,8] { for (name,run) in [("intrusive",intr as fn(usize)),("smallvec",small as fn(usize))] {
        run(depth); ALLOCS.store(0,Ordering::Relaxed);let start=Instant::now();for _ in 0..N {run(black_box(depth));}let elapsed=start.elapsed(); let allocs=ALLOCS.load(Ordering::Relaxed);println!("{name} depth={depth} ns={:.2} allocs/call={:.2}",elapsed.as_nanos() as f64/N as f64,allocs as f64/N as f64);
    }}
}
