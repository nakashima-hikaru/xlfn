use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TokenWire {
    pub(crate) session: u64,
    pub(crate) slot: u64,
    pub(crate) generation: u64,
}
