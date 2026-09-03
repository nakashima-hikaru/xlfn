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
        bumpalo::collections::String::from_utf16_in(units, &self.arena.0)
            .map(|value| value.into_bump_str())
            .map_err(|_| crate::XllError::input(argument, crate::error::InputError::InvalidUtf16))
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
#[doc(hidden)]
pub struct CallScope<'call> {
    callbacks: HostCallbackSession,
    scratch: CallScratch,
    handle_permit: std::cell::OnceCell<crate::handle::HandleDomainPermit>,
    lifetime: PhantomData<&'call mut &'call ()>,
}

impl<'call> CallScope<'call> {
    pub(crate) fn new() -> Self {
        Self {
            callbacks: HostCallbackSession::new(),
            scratch: CallScratch::new(),
            handle_permit: std::cell::OnceCell::new(),
            lifetime: PhantomData,
        }
    }

    pub(crate) fn callbacks(&self) -> &HostCallbackSession {
        &self.callbacks
    }

    pub(crate) fn scratch(&'call self) -> &'call CallScratch {
        &self.scratch
    }

    #[inline]
    pub(crate) fn enter_handle_domain(
        &self,
        domain: &crate::handle::HandleReadDomain,
    ) -> XllResult<()> {
        if self.handle_permit.get().is_some() {
            return Ok(());
        }
        let permit = domain.enter()?;
        let _ = self.handle_permit.set(permit);
        Ok(())
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
#[cfg(test)]
pub(crate) fn with_excel_call_scope_and_state<S, R>(
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
