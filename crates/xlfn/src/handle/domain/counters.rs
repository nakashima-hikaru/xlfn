//! Checked retirement counter transitions shared by production and Verus.
//! None of these failures is a retry/rejection: overflow or underflow is fatal.
#[cfg(verus_only)]
use vstd::prelude::*;
#[cfg(not(verus_only))]
macro_rules! verus { ($($item:tt)*) => { $($item)* }; }
verus! {
#[cfg_attr(verus_only, verifier::allow(autoderive_clone_without_spec))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CountStep<T> { Success(T), FailStop }
}
macro_rules! add_logic {
    ($state:ident, $amount:ident, $word:ty) => {
        if $amount > <$word>::MAX - $state {
            CountStep::FailStop
        } else {
            CountStep::Success($state + $amount)
        }
    };
}
macro_rules! subtract_logic {
    ($state:ident, $amount:ident) => {
        if $amount > $state {
            CountStep::FailStop
        } else {
            CountStep::Success($state - $amount)
        }
    };
}
macro_rules! width {
    ($module:ident, $word:ty) => {
        pub(crate) mod $module {
            use super::*;
            #[cfg(not(verus_only))]
            #[inline]
            pub(crate) fn add(state: $word, amount: $word) -> CountStep<$word> { add_logic!(state, amount, $word) }
            #[cfg(not(verus_only))]
            #[inline]
            pub(crate) fn subtract(state: $word, amount: $word) -> CountStep<$word> { subtract_logic!(state, amount) }
            #[cfg(verus_only)]
            verus! {
                pub(crate) open spec fn add_spec(state: $word, amount: $word) -> CountStep<$word> { add_logic!(state, amount, $word) }
                pub(crate) open spec fn subtract_spec(state: $word, amount: $word) -> CountStep<$word> { subtract_logic!(state, amount) }
                pub(crate) fn add(state: $word, amount: $word) -> (result: CountStep<$word>)
                    ensures result == add_spec(state, amount),
                        match result {
                            CountStep::Success(next) => next as nat == state as nat + amount as nat,
                            CountStep::FailStop => state as nat + amount as nat > <$word>::MAX as nat,
                        },
                { add_logic!(state, amount, $word) }
                pub(crate) fn subtract(state: $word, amount: $word) -> (result: CountStep<$word>)
                    ensures result == subtract_spec(state, amount),
                        match result {
                            CountStep::Success(next) => next as nat + amount as nat == state as nat,
                            CountStep::FailStop => amount > state,
                        },
                { subtract_logic!(state, amount) }
            }
        }
    };
}
#[cfg(any(verus_only, target_pointer_width = "32"))]
width!(word32, u32);
#[cfg(any(verus_only, target_pointer_width = "64"))]
width!(word64, u64);
#[cfg(all(not(verus_only), target_pointer_width = "32"))]
use word32 as native;
#[cfg(all(not(verus_only), target_pointer_width = "64"))]
use word64 as native;
#[cfg(not(verus_only))]
#[inline]
pub(crate) fn add(state: usize, amount: usize) -> CountStep<usize> {
    match native::add(state as _, amount as _) {
        CountStep::Success(next) => CountStep::Success(next as usize),
        CountStep::FailStop => CountStep::FailStop,
    }
}
#[cfg(not(verus_only))]
#[inline]
pub(crate) fn subtract(state: usize, amount: usize) -> CountStep<usize> {
    match native::subtract(state as _, amount as _) {
        CountStep::Success(next) => CountStep::Success(next as usize),
        CountStep::FailStop => CountStep::FailStop,
    }
}
