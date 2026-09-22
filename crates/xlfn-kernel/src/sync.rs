//! Non-poisoning synchronization primitives backed by `parking_lot`.

pub use parking_lot::{
    Condvar, MappedMutexGuard, MappedRwLockReadGuard, MappedRwLockWriteGuard, Mutex, MutexGuard,
    RwLock, RwLockReadGuard, RwLockWriteGuard, WaitTimeoutResult,
};
