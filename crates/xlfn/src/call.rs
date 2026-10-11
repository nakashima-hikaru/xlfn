//! State owned by one Excel-visible call.
//!
//! A call scope is the single root for resources that may be borrowed by
//! synchronous input conversion. The scope is generative, so values that
//! contain its lifetime cannot escape the generated Excel call. The scratch
//! allocator deliberately exposes only operations whose results are either
//! borrowed strings or `Copy` slices; arbitrary destructor-bearing values do
//! not belong in this call-local storage.

use crate::XllResult;
use std::cell::{Cell, OnceCell};
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;

const LARGE_TYPED_COLLECTION_BYTES: usize = 64 * 1024;
const LARGE_COLLECTION_TEXT_CAPACITY: usize = 64 * 1024;

pub(crate) struct CallScratch {
    arena: AssertUnwindSafe<bumpalo::Bump>,
    strings: AssertUnwindSafe<OnceCell<Box<bumpalo::Bump>>>,
    large_collection: AssertUnwindSafe<Cell<bool>>,
}

impl CallScratch {
    fn new() -> Self {
        Self {
            arena: AssertUnwindSafe(bumpalo::Bump::new()),
            strings: AssertUnwindSafe(OnceCell::new()),
            large_collection: AssertUnwindSafe(Cell::new(false)),
        }
    }

    #[cfg(all(test, feature = "bench-internals"))]
    pub(crate) fn allocated_bytes(&self) -> usize {
        self.arena.allocated_bytes()
            + self
                .strings
                .get()
                .map_or(0, |strings| strings.allocated_bytes())
    }

    pub(crate) fn decode_utf16<'call>(
        &'call self,
        units: &[u16],
        argument: &'static str,
    ) -> XllResult<&'call str> {
        let decoder = crate::utf16::Utf16Decoder::new(units);
        // Scalar and small collection text share the primary arena. A large
        // typed collection can fill a matrix-sized chunk before its first
        // string, so only that case creates a separate text arena. Its bounded
        // initial capacity avoids a chain of tiny growing chunks; numeric-only
        // collections never create it. Both arenas live until scope teardown.
        let strings = if self.large_collection.get() {
            self.strings
                .get_or_init(|| {
                    Box::new(bumpalo::Bump::with_capacity(LARGE_COLLECTION_TEXT_CAPACITY))
                })
                .as_ref()
        } else {
            &self.arena.0
        };
        let output =
            strings.alloc_slice_fill_with(decoder.utf8_len(), |_| std::mem::MaybeUninit::uninit());
        decoder.decode_into(output, argument)
    }

    pub(crate) fn collect_copy<T: Copy>(
        &self,
        len: usize,
        mut build: impl FnMut(usize) -> XllResult<T>,
    ) -> XllResult<&[T]> {
        if len.saturating_mul(std::mem::size_of::<T>()) >= LARGE_TYPED_COLLECTION_BYTES {
            self.large_collection.set(true);
        }
        let mut values = bumpalo::collections::Vec::with_capacity_in(len, &self.arena.0);
        for index in 0..len {
            values.push(build(index)?);
        }
        Ok(values.into_bump_slice())
    }
}

#[cfg(feature = "handles")]
mod permits;
#[cfg(feature = "handles")]
type HandlePermits = permits::Retained<crate::handle::HandleDomainPermit>;

#[cfg(feature = "handles")]
#[derive(Clone, Copy)]
enum HandleWitnessSource<'scope> {
    Scope {
        _scope: &'scope CallScope<'scope>,
        domain: std::ptr::NonNull<crate::handle::HandleReadDomain>,
    },
    #[cfg(test)]
    Permit(&'scope crate::handle::HandleDomainPermit),
}

/// A real borrow of the append-only call scope or the owning permit.
/// Scope construction stays private to the code that retains its permits.
#[derive(Clone, Copy)]
#[cfg(feature = "handles")]
pub(crate) struct HandleDomainWitness<'scope> {
    source: HandleWitnessSource<'scope>,
}
#[cfg(feature = "handles")]
impl<'scope> HandleDomainWitness<'scope> {
    #[cfg(test)]
    pub(crate) fn from_permit(permit: &'scope crate::handle::HandleDomainPermit) -> Self {
        Self {
            source: HandleWitnessSource::Permit(permit),
        }
    }
    #[inline]
    pub(crate) fn domain(&self) -> std::ptr::NonNull<crate::handle::HandleReadDomain> {
        match self.source {
            HandleWitnessSource::Scope { domain, .. } => domain,
            #[cfg(test)]
            HandleWitnessSource::Permit(permit) => permit.domain(),
        }
    }
}

/// A generative lifetime token for call-local input storage.
///
/// This token does not authorize Excel callbacks. Host authority is carried
/// separately by the frame created at an actual Excel entrypoint.
#[doc(hidden)]
pub struct CallScope<'call> {
    scratch: CallScratch,
    #[cfg(feature = "handles")]
    handle_permits: std::cell::RefCell<HandlePermits>,
    lifetime: PhantomData<&'call mut &'call ()>,
}

impl<'call> CallScope<'call> {
    fn new() -> Self {
        Self {
            scratch: CallScratch::new(),
            #[cfg(feature = "handles")]
            handle_permits: std::cell::RefCell::new(HandlePermits::Empty),
            lifetime: PhantomData,
        }
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
    #[cfg(feature = "handles")]
    pub(crate) fn enter_handle_domain(
        &'call self,
        domain: &'call crate::handle::HandleReadDomain,
    ) -> XllResult<crate::handle::HandleDomainWitness<'call>> {
        let domain_ptr = std::ptr::NonNull::from(domain);
        let mut permits = self.handle_permits.borrow_mut();
        match &*permits {
            HandlePermits::Empty => {}
            HandlePermits::Single(permit) => {
                if permit.domain() == domain_ptr {
                    return Ok(HandleDomainWitness {
                        source: HandleWitnessSource::Scope {
                            _scope: self,
                            domain: domain_ptr,
                        },
                    });
                }
            }
            HandlePermits::Multiple(list) => {
                if list.iter().any(|p| p.domain() == domain_ptr) {
                    return Ok(HandleDomainWitness {
                        source: HandleWitnessSource::Scope {
                            _scope: self,
                            domain: domain_ptr,
                        },
                    });
                }
            }
        }
        // SAFETY: the generative scope constructors keep this scope inside the
        // operation's enclosing owner frame. Requiring `domain` for the
        // invariant brand excludes owners local to that operation. The domain
        // therefore remains alive until the constructor drops every permit.
        let permit = unsafe { domain.enter_owned()? };
        permits.insert(permit);
        Ok(HandleDomainWitness {
            source: HandleWitnessSource::Scope {
                _scope: self,
                domain: domain_ptr,
            },
        })
    }
}

/// Runs an operation under a fresh lifetime that cannot escape in its result.
/// This memory scope never grants or resets Excel callback authority.
#[doc(hidden)]
pub fn with_excel_call_scope<R>(
    operation: impl for<'scope> FnOnce(&'scope CallScope<'scope>) -> R,
) -> R {
    let scope = CallScope::new();
    operation(&scope)
}

/// Runs an operation under a fresh call scope while borrowing existing state.
/// This memory scope never grants or resets Excel callback authority.
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
#[cfg(feature = "bench-internals")]
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

/// Reborrows existing entrypoint authority alongside a fresh memory scope.
/// Unlike the public lifetime helpers, this cannot be invoked without the
/// private session already issued by the Excel entrypoint.
pub(crate) fn with_excel_callback_scope_and_call<A: crate::Addin, R>(
    call: &crate::runtime::CallGuard<'_, A>,
    callbacks: &crate::host_callback::HostCallbackSession,
    operation: impl for<'scope> FnOnce(
        &'scope crate::runtime::CallGuard<'_, A>,
        &'scope CallScope<'scope>,
        &'scope crate::host_callback::HostCallbackSession,
    ) -> R,
) -> R {
    let scope = CallScope::new();
    operation(call, &scope, callbacks)
}

#[cfg(test)]
mod scratch_tests {
    use super::{CallScratch, LARGE_TYPED_COLLECTION_BYTES};

    #[test]
    fn large_numeric_collection_does_not_allocate_text_storage() {
        let scratch = CallScratch::new();
        let length = LARGE_TYPED_COLLECTION_BYTES / std::mem::size_of::<u64>();
        let values = scratch
            .collect_copy(length, |index| Ok(index as u64))
            .unwrap();
        assert_eq!(values.last(), Some(&((length - 1) as u64)));
        assert!(scratch.strings.get().is_none());
    }

    #[test]
    fn strings_stay_borrowable_when_large_collection_changes_text_storage() {
        let scratch = CallScratch::new();
        let small = scratch
            .decode_utf16(&[b'o' as u16, b'k' as u16], "small")
            .unwrap();
        assert!(scratch.strings.get().is_none());
        let length = LARGE_TYPED_COLLECTION_BYTES / std::mem::size_of::<u64>();
        let values = scratch
            .collect_copy(length, |index| Ok(index as u64))
            .unwrap();
        let large = scratch
            .decode_utf16(&[0x65e5, 0xd83d, 0xdca1], "large")
            .unwrap();
        assert!(scratch.strings.get().is_some());
        assert_eq!(small, "ok");
        assert_eq!(large, "日💡");
        assert_eq!(values[length - 1], (length - 1) as u64);
    }

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
