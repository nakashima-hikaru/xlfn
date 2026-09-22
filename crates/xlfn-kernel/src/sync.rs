//! Non-poisoning synchronization primitives backed by `std::sync`.
//!
//! Replaces `parking_lot` with standard library synchronization while preserving
//! non-poisoning semantics: panicking threads do not poison mutexes or rwlocks,
//! and unwinding does not cause cascading lock poisoning.

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::{PoisonError, TryLockError};
use std::time::Duration;

/// A mutual exclusion primitive that does not poison on panic.
#[derive(Default)]
pub struct Mutex<T: ?Sized>(std::sync::Mutex<T>);

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    pub const fn new(val: T) -> Self {
        Self(std::sync::Mutex::new(val))
    }

    /// Consumes the mutex, returning the underlying data.
    pub fn into_inner(self) -> T {
        self.0.into_inner().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Acquires a mutex, blocking the current thread until it is able to do so.
    /// Poisoning is ignored, returning the inner guard even if another thread panicked.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        MutexGuard {
            guard: Some(guard),
            mutex: self,
        }
    }

    /// Attempts to acquire this lock without blocking.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        match self.0.try_lock() {
            Ok(guard) => Some(MutexGuard {
                guard: Some(guard),
                mutex: self,
            }),
            Err(TryLockError::Poisoned(err)) => Some(MutexGuard {
                guard: Some(err.into_inner()),
                mutex: self,
            }),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// Returns a mutable reference to the underlying data.
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => f.debug_struct("Mutex").field("data", &&*guard).finish(),
            None => {
                struct LockedPlaceholder;
                impl fmt::Debug for LockedPlaceholder {
                    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("<locked>")
                    }
                }
                f.debug_struct("Mutex")
                    .field("data", &LockedPlaceholder)
                    .finish()
            }
        }
    }
}

/// An RAII implementation of a "scoped lock" of a mutex.
pub struct MutexGuard<'a, T: ?Sized> {
    pub(crate) guard: Option<std::sync::MutexGuard<'a, T>>,
    pub(crate) mutex: &'a Mutex<T>,
}

impl<'a, T: ?Sized> MutexGuard<'a, T> {
    /// Returns a reference to the `Mutex` that this guard is locking.
    pub fn mutex(this: &Self) -> &'a Mutex<T> {
        this.mutex
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.guard.as_ref().expect("valid guard").deref()
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard.as_mut().expect("valid guard").deref_mut()
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// A reader-writer lock that does not poison on panic.
#[derive(Default)]
pub struct RwLock<T: ?Sized>(std::sync::RwLock<T>);

impl<T> RwLock<T> {
    /// Creates a new instance of an `RwLock<T>` which is unlocked.
    pub const fn new(val: T) -> Self {
        Self(std::sync::RwLock::new(val))
    }

    /// Consumes the lock, returning the underlying data.
    pub fn into_inner(self) -> T {
        self.0.into_inner().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: ?Sized> RwLock<T> {
    /// Locks this `RwLock` with shared read access, blocking the current thread until acquired.
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        let guard = self.0.read().unwrap_or_else(PoisonError::into_inner);
        RwLockReadGuard {
            guard,
            rwlock: self,
        }
    }

    /// Locks this `RwLock` with exclusive write access, blocking the current thread until acquired.
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        let guard = self.0.write().unwrap_or_else(PoisonError::into_inner);
        RwLockWriteGuard {
            guard,
            rwlock: self,
        }
    }

    /// Attempts to acquire this `RwLock` with shared read access.
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        match self.0.try_read() {
            Ok(guard) => Some(RwLockReadGuard {
                guard,
                rwlock: self,
            }),
            Err(TryLockError::Poisoned(err)) => Some(RwLockReadGuard {
                guard: err.into_inner(),
                rwlock: self,
            }),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// Attempts to lock this `RwLock` with exclusive write access.
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        match self.0.try_write() {
            Ok(guard) => Some(RwLockWriteGuard {
                guard,
                rwlock: self,
            }),
            Err(TryLockError::Poisoned(err)) => Some(RwLockWriteGuard {
                guard: err.into_inner(),
                rwlock: self,
            }),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// Returns a mutable reference to the underlying data.
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_read() {
            Some(guard) => f.debug_struct("RwLock").field("data", &&*guard).finish(),
            None => {
                struct LockedPlaceholder;
                impl fmt::Debug for LockedPlaceholder {
                    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("<locked>")
                    }
                }
                f.debug_struct("RwLock")
                    .field("data", &LockedPlaceholder)
                    .finish()
            }
        }
    }
}

/// RAII structure used to release the shared read access of a lock when dropped.
pub struct RwLockReadGuard<'a, T: ?Sized> {
    guard: std::sync::RwLockReadGuard<'a, T>,
    rwlock: &'a RwLock<T>,
}

impl<'a, T: ?Sized> RwLockReadGuard<'a, T> {
    /// Returns a reference to the `RwLock` that this guard is locking.
    pub fn rwlock(this: &Self) -> &'a RwLock<T> {
        this.rwlock
    }
}

impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.guard.deref()
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLockReadGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// RAII structure used to release the exclusive write access of a lock when dropped.
pub struct RwLockWriteGuard<'a, T: ?Sized> {
    guard: std::sync::RwLockWriteGuard<'a, T>,
    rwlock: &'a RwLock<T>,
}

impl<'a, T: ?Sized> RwLockWriteGuard<'a, T> {
    /// Returns a reference to the `RwLock` that this guard is locking.
    pub fn rwlock(this: &Self) -> &'a RwLock<T> {
        this.rwlock
    }
}

impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.guard.deref()
    }
}

impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard.deref_mut()
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLockWriteGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// Result of a conditional wait with timeout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    /// Returns whether the wait timed out before the condition was met.
    #[inline]
    pub fn timed_out(self) -> bool {
        self.0
    }
}

/// A Condition Variable for thread coordination without lock poisoning.
#[derive(Default)]
pub struct Condvar(std::sync::Condvar);

impl Condvar {
    /// Creates a new condition variable.
    pub const fn new() -> Self {
        Self(std::sync::Condvar::new())
    }

    /// Blocks the current thread until this condition variable receives a notification.
    pub fn wait<T>(&self, guard: &mut MutexGuard<'_, T>) {
        let inner = guard.guard.take().expect("valid mutex guard");
        let next = self.0.wait(inner).unwrap_or_else(PoisonError::into_inner);
        guard.guard = Some(next);
    }

    /// Blocks the current thread until this condition variable receives a notification or the timeout expires.
    pub fn wait_for<T>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        timeout: Duration,
    ) -> WaitTimeoutResult {
        let inner = guard.guard.take().expect("valid mutex guard");
        let (next, res) = self
            .0
            .wait_timeout(inner, timeout)
            .unwrap_or_else(PoisonError::into_inner);
        guard.guard = Some(next);
        WaitTimeoutResult(res.timed_out())
    }

    /// Repeatedly waits on the condition variable while the predicate returns `true`.
    pub fn wait_while<T>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        mut condition: impl FnMut(&mut T) -> bool,
    ) {
        while condition(guard) {
            self.wait(guard);
        }
    }

    /// Repeatedly waits on the condition variable while the predicate returns `true`, up to `timeout`.
    pub fn wait_while_for<T>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        mut condition: impl FnMut(&mut T) -> bool,
        timeout: Duration,
    ) -> WaitTimeoutResult {
        let start = std::time::Instant::now();
        while condition(guard) {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return WaitTimeoutResult(true);
            }
            let remaining = timeout - elapsed;
            let inner = guard.guard.take().expect("valid mutex guard");
            let (next, res) = self
                .0
                .wait_timeout(inner, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            guard.guard = Some(next);
            if res.timed_out() {
                break;
            }
        }
        WaitTimeoutResult(condition(guard))
    }

    /// Wakes up one blocked thread on this condvar.
    #[inline]
    pub fn notify_one(&self) {
        self.0.notify_one();
    }

    /// Wakes up all blocked threads on this condvar.
    #[inline]
    pub fn notify_all(&self) {
        self.0.notify_all();
    }
}

impl fmt::Debug for Condvar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
