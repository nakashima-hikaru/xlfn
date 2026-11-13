//! Scoped intrusive thread-local lifecycle model.
//!
//! A borrowed capability cannot leave its invocation:
//! ```compile_fail
//! let r = lifecycle_stack_model::with_lifecycle(1, |access| access);
//! println!("{r:?}");
//! ```
//! Access cannot be sent to another thread:
//! ```compile_fail
//! lifecycle_stack_model::with_lifecycle(1, |access| {
//!     std::thread::scope(|scope| { scope.spawn(move || access.belongs_to(1)); });
//! });
//! ```
//!
//! Its registration owner is not accessible to the caller, and forgetting the
//! capability does not forget the registration.

use std::{cell::Cell, marker::PhantomPinned, pin::pin, ptr};

thread_local! { static HEAD: Cell<*const Frame> = const { Cell::new(ptr::null()) }; }

#[derive(Debug)]
struct Frame { runtime: usize, previous: *const Frame, _pinned: PhantomPinned }

#[derive(Debug)]
pub struct Access<'scope> { frame: &'scope Frame }
impl Access<'_> { pub fn belongs_to(&self, runtime: usize) -> bool { self.frame.runtime == runtime } }

struct Registration<'scope>(&'scope Frame);
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        HEAD.with(|head| {
            if !ptr::eq(head.get(), ptr::from_ref(self.0)) { std::process::abort(); }
            head.set(self.0.previous);
        });
    }
}

pub fn with_lifecycle<R>(runtime: usize, f: impl for<'scope> FnOnce(Access<'scope>) -> R) -> Option<R> {
    let previous = HEAD.with(|head| {
        let mut cursor = head.get();
        while !cursor.is_null() {
            // SAFETY: each published node is pinned in a still-live stack frame;
            // registration never escapes this function and pops in LIFO order.
            // Rust unwinding runs the pop; foreign nonlocal stack unwinding is excluded.
            let node = unsafe { &*cursor };
            if node.runtime == runtime { return None; }
            cursor = node.previous;
        }
        Some(head.get())
    })?;
    let frame = pin!(Frame { runtime, previous, _pinned: PhantomPinned });
    let frame = frame.as_ref().get_ref();
    // No user code or panicking operation between publish and guard construction.
    HEAD.with(|head| head.set(ptr::from_ref(frame)));
    let _registration = Registration(frame);
    Some(f(Access { frame }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_cycles_and_rollback() {
        assert_eq!(with_lifecycle(1, |a| {
            assert!(a.belongs_to(1));
            assert_eq!(with_lifecycle(2, |_| with_lifecycle(3, |_| with_lifecycle(1, |_| 9))), Some(Some(None)));
            assert_eq!(with_lifecycle(2, |_| 4), Some(4));
            assert_eq!(with_lifecycle(1, |_| 9), None);
        }), Some(()));
        assert_eq!(with_lifecycle(1, |_| 8), Some(8));
    }
    #[test]
    fn unwind_restores_head() {
        let caught = std::panic::catch_unwind(|| with_lifecycle(1, |_| with_lifecycle(2, |_| panic!("test"))));
        assert!(caught.is_err());
        assert_eq!(with_lifecycle(1, |_| 4), Some(4));
        assert!(HEAD.with(|h| h.get().is_null()));
    }
    #[test]
    fn forgotten_capability_does_not_forget_registration() {
        #[expect(
            clippy::forget_non_drop,
            reason = "test explicitly verifies forgetting Access leaves Registration intact"
        )]
        with_lifecycle(1, |access| std::mem::forget(access));
        assert!(HEAD.with(|h| h.get().is_null()));
        assert_eq!(with_lifecycle(1, |_| 4), Some(4));
    }
    #[test]
    fn thread_local_heads_are_independent() {
        with_lifecycle(1, |_| {
            assert_eq!(std::thread::spawn(|| with_lifecycle(1, |_| 4)).join().unwrap(), Some(4));
            assert_eq!(with_lifecycle(1, |_| 4), None);
        });
    }
    #[test]
    fn tls_head_is_one_pointer() { assert_eq!(std::mem::size_of::<Cell<*const Frame>>(), std::mem::size_of::<usize>()); }
}
