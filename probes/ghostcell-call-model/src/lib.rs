//! A minimal model of an invocation-owned, append-only handle read domain.
//!
//! Handles cannot escape the invocation.
//! ```compile_fail
//! use ghostcell_call_model::*;
//! let domain = Domain::new(7);
//! let leaked = with_storage_and_state(&domain, |domain, storage, token| {
//!     DecodeContext::new(storage, token).resolve(domain)
//! });
//! assert_eq!(*leaked, 7);
//! ```
//!
//! A domain created inside the invocation cannot outlive the storage owner.
//! ```compile_fail
//! use ghostcell_call_model::*;
//! with_storage(|storage, token| {
//!     let domain = Domain::new(7);
//!     let mut ctx = DecodeContext::new(storage, token);
//!     let handle = ctx.resolve(&domain);
//!     assert_eq!(*handle, 7);
//! });
//! ```
//!
//! Tokens from separate fresh brands cannot access the same cell.
//! ```compile_fail
//! use ghost_cell::{GhostCell, GhostToken};
//! GhostToken::new(|mut outer| {
//!     let cell = GhostCell::new(7);
//!     let _ = cell.borrow_mut(&mut outer);
//!     GhostToken::new(|mut inner| { *cell.borrow_mut(&mut inner) = 8; });
//! });
//! ```
use ghost_cell::{GhostCell, GhostToken};
use std::{cell::Cell, marker::PhantomData, ptr::NonNull, rc::Rc};

pub struct Domain {
    value: usize,
    readers: Rc<Cell<usize>>,
    entries: Cell<usize>,
}
impl Domain {
    pub fn new(value: usize) -> Self {
        Self {
            value,
            readers: Rc::new(Cell::new(0)),
            entries: Cell::new(0),
        }
    }
    pub fn readers(&self) -> usize {
        self.readers.get()
    }
    pub fn entries(&self) -> usize {
        self.entries.get()
    }
    unsafe fn enter_owned(&self) -> Permit {
        self.readers.set(self.readers.get() + 1);
        self.entries.set(self.entries.get() + 1);
        Permit {
            domain: NonNull::from(self),
        }
    }
}
impl Drop for Domain {
    fn drop(&mut self) {
        assert_eq!(self.readers.get(), 0, "owner dropped while a permit lives");
    }
}
struct Permit {
    domain: NonNull<Domain>,
}
impl Drop for Permit {
    fn drop(&mut self) {
        // SAFETY: only DecodeContext::resolve constructs these; the generative
        // invocation requires its domain for the invariant storage borrow.
        let domain = unsafe { self.domain.as_ref() };
        domain.readers.set(domain.readers.get() - 1);
    }
}

// The scratch reference is only a drop-order observation probe; actual xlfn
// scratch allocation does not need to depend on the domain owner.
struct Scratch {
    teardown_probe: Option<Rc<Cell<usize>>>,
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(readers) = &self.teardown_probe {
            assert_eq!(readers.get(), 0);
        }
    }
}
pub struct CallScope<'call> {
    scratch: Scratch,
    invariant: PhantomData<&'call mut &'call ()>,
}
pub struct CallStorage<'brand, 'call> {
    // Declaration order is destruction order: permits release before scratch.
    permits: GhostCell<'brand, Vec<Permit>>,
    scope: CallScope<'call>,
}
impl<'brand, 'call> CallStorage<'brand, 'call> {
    fn new(probe: Option<Rc<Cell<usize>>>) -> Self {
        Self {
            permits: GhostCell::new(Vec::new()),
            scope: CallScope {
                scratch: Scratch {
                    teardown_probe: probe,
                },
                invariant: PhantomData,
            },
        }
    }
}
#[derive(Clone, Copy)]
struct Witness<'call> {
    _scope: &'call CallScope<'call>,
    domain: NonNull<Domain>,
}
pub struct Handle<'call> {
    witness: Witness<'call>,
}
impl std::ops::Deref for Handle<'_> {
    type Target = usize;
    fn deref(&self) -> &usize {
        // SAFETY: witness can only be created after retaining this domain's
        // permit in its enclosing storage. Storage cannot be moved/dropped
        // while the invariant call borrow lives. Domain lives even longer.
        &unsafe { self.witness.domain.as_ref() }.value
    }
}
pub struct DecodeContext<'brand, 'call> {
    storage: &'call CallStorage<'brand, 'call>,
    token: GhostToken<'brand>,
}
impl<'brand, 'call> DecodeContext<'brand, 'call> {
    pub fn new(storage: &'call CallStorage<'brand, 'call>, token: GhostToken<'brand>) -> Self {
        Self { storage, token }
    }
    pub fn resolve(&mut self, domain: &'call Domain) -> Handle<'call> {
        let ptr = NonNull::from(domain);
        {
            let permits = self.storage.permits.borrow_mut(&mut self.token);
            if !permits.iter().any(|permit| permit.domain == ptr) {
                // SAFETY: requiring the external domain for invariant 'call
                // excludes domains local to the invocation operation. The
                // constructor destroys storage before releasing that owner.
                permits.push(unsafe { domain.enter_owned() });
            }
        }
        Handle {
            witness: Witness {
                _scope: &self.storage.scope,
                domain: ptr,
            },
        }
    }
    pub fn finish(self) -> ReturnContext<'call> {
        ReturnContext {
            scope: &self.storage.scope,
        }
    }
}
pub struct ReturnContext<'call> {
    scope: &'call CallScope<'call>,
}
impl ReturnContext<'_> {
    pub fn has_probe(&self) -> bool {
        self.scope.scratch.teardown_probe.is_some()
    }
}
pub fn with_storage<R>(
    operation: impl for<'brand, 'call> FnOnce(
        &'call CallStorage<'brand, 'call>,
        GhostToken<'brand>,
    ) -> R,
) -> R {
    GhostToken::new(|token| {
        let storage = CallStorage::new(None);
        operation(&storage, token)
    })
}
pub fn with_storage_and_state<S, R>(
    state: &S,
    operation: impl for<'brand, 'call> FnOnce(
        &'call S,
        &'call CallStorage<'brand, 'call>,
        GhostToken<'brand>,
    ) -> R,
) -> R {
    GhostToken::new(|token| {
        let storage = CallStorage::new(None);
        operation(state, &storage, token)
    })
}
pub fn with_probe<R>(
    domain: &Domain,
    operation: impl for<'brand, 'call> FnOnce(
        &'call Domain,
        &'call CallStorage<'brand, 'call>,
        GhostToken<'brand>,
    ) -> R,
) -> R {
    GhostToken::new(|token| {
        let storage = CallStorage::new(Some(Rc::clone(&domain.readers)));
        operation(domain, &storage, token)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn two_handles_same_domain_survive_context_finish() {
        let domain = Domain::new(7);
        with_probe(&domain, |domain, storage, token| {
            let mut ctx = DecodeContext::new(storage, token);
            let first = ctx.resolve(domain);
            let second = ctx.resolve(domain);
            let result = ctx.finish();
            assert!(result.has_probe());
            assert_eq!(*first + *second, 14);
            assert_eq!(domain.readers(), 1);
            assert_eq!(domain.entries(), 1);
        });
        assert_eq!(domain.readers(), 0);
    }
    #[test]
    fn independent_domains_and_append_only_growth() {
        let domains: Vec<_> = (0..128).map(Domain::new).collect();
        with_storage_and_state(&domains, |domains, storage, token| {
            let mut ctx = DecodeContext::new(storage, token);
            let first = ctx.resolve(&domains[0]);
            let handles: Vec<_> = domains.iter().map(|domain| ctx.resolve(domain)).collect();
            assert_eq!(*first, 0);
            assert_eq!(handles.iter().map(|h| **h).sum::<usize>(), (0..128).sum());
            assert!(domains.iter().all(|d| d.readers() == 1 && d.entries() == 1));
        });
        assert!(domains.iter().all(|d| d.readers() == 0));
    }
    #[test]
    fn panic_releases_permits_before_scratch() {
        let domain = Domain::new(7);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_probe(&domain, |domain, storage, token| {
                let mut ctx = DecodeContext::new(storage, token);
                let first = ctx.resolve(domain);
                let second = ctx.resolve(domain);
                assert_eq!(*first + *second, 14);
                panic!("expected panic");
            });
        }));
        assert!(result.is_err());
        assert_eq!(domain.readers(), 0);
    }
    #[test]
    fn fresh_invocations_reacquire_permit() {
        let domain = Domain::new(3);
        for _ in 0..2 {
            with_probe(&domain, |domain, storage, token| {
                assert_eq!(*DecodeContext::new(storage, token).resolve(domain), 3);
                assert_eq!(domain.readers(), 1);
            });
            assert_eq!(domain.readers(), 0);
        }
        assert_eq!(domain.entries(), 2);
    }
    #[test]
    fn layouts_do_not_add_token_or_cell_storage() {
        assert_eq!(std::mem::size_of::<GhostToken<'_>>(), 0);
        assert_eq!(
            std::mem::size_of::<GhostCell<'_, Vec<Permit>>>(),
            std::mem::size_of::<Vec<Permit>>()
        );
        assert_eq!(
            std::mem::size_of::<DecodeContext<'_, '_>>(),
            std::mem::size_of::<usize>()
        );
        assert_eq!(
            std::mem::size_of::<Handle<'_>>(),
            2 * std::mem::size_of::<usize>()
        );
    }
}
