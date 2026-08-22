//! Passive lifecycle state vocabulary shared by the runtime and boundaries.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LifecyclePhase {
    Closed = 0,
    Opening = 1,
    Open = 2,
    Closing = 3,
    OpenRollbackPending = 4,
    Quarantined = 5,
}

impl LifecyclePhase {
    pub(crate) fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::Closed,
            1 => Self::Opening,
            2 => Self::Open,
            3 => Self::Closing,
            4 => Self::OpenRollbackPending,
            5 => Self::Quarantined,
            _ => std::process::abort(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum HostLifecycleIntent {
    None = 0,
    ExplicitRemovalRequested = 1,
    ExplicitRemovalComplete = 2,
}
