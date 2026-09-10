//! State owned by one Excel-visible call.
//!
//! A call scope is the single root for resources that may be borrowed by
//! synchronous input conversion. The scope is generative, so values that
//! contain its lifetime cannot escape the generated Excel call. The scratch
//! allocator deliberately exposes only operations whose results are either
//! borrowed strings or `Copy` slices; arbitrary destructor-bearing values do
//! not belong in this call-local storage.

use crate::XllResult;
use crate::host_callback::HostCallbackSession;
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;

pub(crate) struct CallScratch {
    arena: AssertUnwindSafe<bumpalo::Bump>,
}

impl CallScratch {
    fn new() -> Self {
        Self {
            arena: AssertUnwindSafe(bumpalo::Bump::new()),
        }
    }

    pub(crate) fn decode_utf16<'call>(
        &'call self,
        units: &[u16],
        argument: &'static str,
    ) -> XllResult<&'call str> {
        // Reduction (rather than a short-circuit loop) lets the compiler
        // vectorize ASCII detection, including long text columns.
        let bits = units.iter().copied().fold(0, |bits, unit| bits | unit);
        if bits < 0x80 {
            let bytes = self
                .arena
                .alloc_slice_fill_iter(units.iter().map(|&unit| unit as u8));
            return Ok(std::str::from_utf8(bytes).expect("ASCII units are valid UTF-8"));
        }
        // Every UTF-16 unit emits at most three UTF-8 bytes (a surrogate
        // pair emits four). The array input budget already charges this
        // bound, so reserving it avoids growth and copying for Unicode text.
        let capacity = units.len() * 3;
        let mut decoded = bumpalo::collections::String::with_capacity_in(capacity, &self.arena.0);
        for character in char::decode_utf16(units.iter().copied()) {
            decoded.push(character.map_err(|_| {
                crate::XllError::input(argument, crate::error::InputError::InvalidUtf16)
            })?);
        }
        Ok(decoded.into_bump_str())
    }

    pub(crate) fn collect_copy<T: Copy>(
        &self,
        len: usize,
        mut build: impl FnMut(usize) -> XllResult<T>,
    ) -> XllResult<&[T]> {
        let mut values = bumpalo::collections::Vec::with_capacity_in(len, &self.arena.0);
        for index in 0..len {
            values.push(build(index)?);
        }
        Ok(values.into_bump_slice())
    }
}

/// A generative lifetime token for one generated Excel call boundary.
enum HandlePermits {
    Empty,
    Single(crate::handle::HandleDomainPermit),
    Multiple(Vec<crate::handle::HandleDomainPermit>),
}

/// A generative lifetime token for one generated Excel call boundary.
#[doc(hidden)]
pub struct CallScope<'call> {
    callbacks: HostCallbackSession,
    scratch: CallScratch,
    handle_permits: std::cell::RefCell<HandlePermits>,
    lifetime: PhantomData<&'call mut &'call ()>,
}

impl<'call> CallScope<'call> {
    fn new() -> Self {
        Self {
            callbacks: HostCallbackSession::new(),
            scratch: CallScratch::new(),
            handle_permits: std::cell::RefCell::new(HandlePermits::Empty),
            lifetime: PhantomData,
        }
    }

    pub(crate) fn callbacks(&self) -> &HostCallbackSession {
        &self.callbacks
    }

    pub(crate) fn scratch(&'call self) -> &'call CallScratch {
        &self.scratch
    }

    /// Enters the handle read domain for this call scope and returns a witness
    /// valid for the scope's invariant `'call` lifetime.
    ///
    /// Scopes can only be constructed by the generative closure entry points
    /// below. Requiring the domain for that entire brand prevents owners
    /// created inside the closure from being admitted: they could otherwise
    /// be destroyed before the scope releases its retained reader permits.
    #[inline]
    pub(crate) fn enter_handle_domain(
        &'call self,
        domain: &'call crate::handle::HandleReadDomain,
    ) -> XllResult<crate::handle::HandleDomainWitness<'call>> {
        let domain_ptr = std::ptr::NonNull::from(domain);
        let mut permits = self.handle_permits.borrow_mut();
        match &*permits {
            HandlePermits::Empty => {}
            HandlePermits::Single(permit) => {
                if permit.domain == domain_ptr {
                    // SAFETY: an active permit for `domain` is retained in `self.handle_permits`
                    // for the invariant brand lifetime of this `CallScope`.
                    return Ok(unsafe {
                        crate::handle::HandleDomainWitness::new_unchecked(domain_ptr)
                    });
                }
            }
            HandlePermits::Multiple(list) => {
                if list.iter().any(|p| p.domain == domain_ptr) {
                    // SAFETY: an active permit for `domain` is retained in `self.handle_permits`
                    // for the invariant brand lifetime of this `CallScope`.
                    return Ok(unsafe {
                        crate::handle::HandleDomainWitness::new_unchecked(domain_ptr)
                    });
                }
            }
        }
        // SAFETY: the generative scope constructors keep this scope inside the
        // operation's enclosing owner frame. Requiring `domain` for the
        // invariant brand excludes owners local to that operation. The domain
        // therefore remains alive until the constructor drops every permit.
        let permit = unsafe { domain.enter_owned()? };
        match std::mem::replace(&mut *permits, HandlePermits::Empty) {
            HandlePermits::Empty => {
                *permits = HandlePermits::Single(permit);
            }
            HandlePermits::Single(existing) => {
                *permits = HandlePermits::Multiple(vec![existing, permit]);
            }
            HandlePermits::Multiple(mut list) => {
                list.push(permit);
                *permits = HandlePermits::Multiple(list);
            }
        }
        // SAFETY: `permit` was stored in `self.handle_permits` and remains active for
        // the entire invariant brand lifetime of this `CallScope`.
        Ok(unsafe { crate::handle::HandleDomainWitness::new_unchecked(domain_ptr) })
    }
}

/// Runs an operation under a fresh lifetime that cannot escape in its result.
#[doc(hidden)]
pub fn with_excel_call_scope<R>(
    operation: impl for<'scope> FnOnce(&'scope CallScope<'scope>) -> R,
) -> R {
    let scope = CallScope::new();
    operation(&scope)
}

/// Runs an operation under a fresh call scope while borrowing existing state.
#[doc(hidden)]
pub fn with_excel_call_scope_and_state<S, R>(
    state: &S,
    operation: impl for<'scope> FnOnce(&'scope S, &'scope CallScope<'scope>) -> R,
) -> R {
    let scope = CallScope::new();
    operation(state, &scope)
}

/// Runs an operation under a fresh call scope while reborrowing the already
/// entered runtime call guard for exactly that scope. Generation-scoped
/// services therefore come from the guard's pinned publication without
/// extending the guard's borrow into the generated call frame.
pub(crate) fn with_excel_call_scope_and_call<A: crate::Addin, R>(
    call: &crate::runtime::CallGuard<'_, A>,
    operation: impl for<'scope> FnOnce(
        &'scope crate::runtime::CallGuard<'_, A>,
        &'scope CallScope<'scope>,
    ) -> R,
) -> R {
    let scope = CallScope::new();
    operation(call, &scope)
}

#[cfg(test)]
mod scratch_tests {
    use super::CallScratch;

    proptest::proptest! {
        #[test]
        fn scratch_decoding_matches_standard_utf16(
            units in proptest::collection::vec(proptest::prelude::any::<u16>(), 0..256),
        ) {
            let scratch = CallScratch::new();
            let actual = scratch.decode_utf16(&units, "text");
            match String::from_utf16(&units) {
                Ok(expected) => proptest::prop_assert_eq!(actual.unwrap(), expected),
                Err(_) => proptest::prop_assert!(matches!(actual, Err(crate::XllError::Input {
                    argument: "text", reason: crate::error::InputError::InvalidUtf16,
                })), "invalid UTF-16 must retain its argument and error kind"),
            }
        }

        #[test]
        fn scratch_decoding_round_trips_valid_unicode(text in ".{0,256}") {
            let units = text.encode_utf16().collect::<Vec<_>>();
            let scratch = CallScratch::new();
            proptest::prop_assert_eq!(scratch.decode_utf16(&units, "text").unwrap(), text);
        }
    }
}
