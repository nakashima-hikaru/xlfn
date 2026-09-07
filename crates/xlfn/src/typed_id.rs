//! Shared representation code for distinct internal nonzero identities.
//!
//! Call sites select only the constructor/accessor they need. Promotion,
//! increments, and other domain rules remain in the individual type's module.

macro_rules! nonzero_u64_id {
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            $(#[$new_attribute:meta])*
            $new_visibility:vis fn new;
            $(
                $(#[$get_attribute:meta])*
                $get_visibility:vis fn get;
            )?
        }
    ) => {
        $(#[$attribute])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        $visibility struct $name(::core::num::NonZeroU64);

        impl $name {
            $(#[$new_attribute])*
            $new_visibility const fn new(raw: u64) -> Option<Self> {
                match ::core::num::NonZeroU64::new(raw) {
                    Some(raw) => Some(Self(raw)),
                    None => None,
                }
            }

            $(
                $(#[$get_attribute])*
                $get_visibility const fn get(self) -> u64 {
                    self.0.get()
                }
            )?
        }
    };
}

pub(crate) use nonzero_u64_id;
