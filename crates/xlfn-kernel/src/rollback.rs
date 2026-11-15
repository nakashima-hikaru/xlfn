//! Consuming rollback for an owner that stays intact until it is transferred.

use std::mem::ManuallyDrop;

/// Resolves an abandoned ownership transfer.
pub trait Rollback {
    /// Consumes the owner and performs its recovery action.
    fn rollback(self);
}

/// Owns one live value until completion transfers it or drop rolls it back.
#[must_use = "complete the transfer or drop the guard to roll it back"]
pub struct RollbackGuard<T: Rollback> {
    value: ManuallyDrop<T>,
}

impl<T: Rollback> RollbackGuard<T> {
    pub fn new(value: T) -> Self {
        Self {
            value: ManuallyDrop::new(value),
        }
    }

    pub fn get(&self) -> &T {
        &self.value
    }

    pub fn get_mut(&mut self) -> &mut T {
        &mut self.value
    }

    /// Disarms rollback and returns the owner for the next step.
    pub fn complete(self) -> T {
        let mut guard = ManuallyDrop::new(self);
        // SAFETY: the live guard owns an initialized value. The outer
        // ManuallyDrop prevents the guard's destructor after the move.
        unsafe { ManuallyDrop::take(&mut guard.value) }
    }
}

impl<T: Rollback> Drop for RollbackGuard<T> {
    fn drop(&mut self) {
        // SAFETY: complete suppresses this destructor, so the field still
        // owns its initialized value. Neither the guard nor the field drops
        // the value again if rollback unwinds.
        let value = unsafe { ManuallyDrop::take(&mut self.value) };
        value.rollback();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Owner<'a> {
        rollbacks: &'a Cell<u32>,
        drops: &'a Cell<u32>,
        panic_on_rollback: bool,
    }

    impl Rollback for Owner<'_> {
        fn rollback(self) {
            self.rollbacks.set(self.rollbacks.get() + 1);
            assert!(!self.panic_on_rollback, "rollback panicked");
        }
    }

    impl Drop for Owner<'_> {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    #[test]
    fn miri_completion_transfers_a_live_owner() {
        let rollbacks = Cell::new(0);
        let drops = Cell::new(0);
        let guard = RollbackGuard::new(Owner {
            rollbacks: &rollbacks,
            drops: &drops,
            panic_on_rollback: false,
        });

        let owner = guard.complete();
        assert_eq!(drops.get(), 0);
        drop(owner);
        assert_eq!(drops.get(), 1);
        assert_eq!(rollbacks.get(), 0);
    }

    #[test]
    fn miri_rollback_consumes_the_owner_even_if_it_panics() {
        let rollbacks = Cell::new(0);
        let drops = Cell::new(0);
        for panic_on_rollback in [false, true] {
            let mut guard = RollbackGuard::new(Owner {
                rollbacks: &rollbacks,
                drops: &drops,
                panic_on_rollback: false,
            });
            guard.get_mut().panic_on_rollback = panic_on_rollback;
            assert_eq!(guard.get().panic_on_rollback, panic_on_rollback);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(guard)));
            assert_eq!(result.is_err(), panic_on_rollback);
            assert_eq!(rollbacks.get(), 1);
            assert_eq!(drops.get(), 1);
            rollbacks.set(0);
            drops.set(0);
        }
    }
}
