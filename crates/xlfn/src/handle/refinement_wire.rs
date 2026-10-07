#[cfg(any(test, feature = "refinement"))]
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "refinement"), derive(Serialize))]
#[cfg_attr(any(test, feature = "refinement"), serde(rename_all = "camelCase"))]
pub(crate) struct TokenWire {
    pub(crate) session: u64,
    pub(crate) slot: u64,
    pub(crate) generation: u64,
}
