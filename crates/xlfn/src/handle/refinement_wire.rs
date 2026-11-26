#[cfg(any(test, feature = "refinement"))]
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "refinement"), derive(Serialize))]
#[cfg_attr(any(test, feature = "refinement"), serde(rename_all = "camelCase"))]
pub(crate) struct TokenWire {
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) session: u64,
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) slot: u64,
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) generation: std::num::NonZeroU64,
}
