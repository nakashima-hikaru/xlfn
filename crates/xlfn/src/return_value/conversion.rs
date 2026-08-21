//! Typed callback status and cleanup-debt conversion values.

use xlfn_sys::{XLRET_ABORT, XLRET_SUCCESS, XLRET_UNCALCED};

/// Represents the terminal or recoverable status returned by Excel C API callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExcelCallbackStatus {
    Success,
    Abort,
    Uncalced,
    Failed(i32),
}

impl ExcelCallbackStatus {
    pub(crate) fn from_raw(status: i32) -> Self {
        match status {
            XLRET_SUCCESS => Self::Success,
            XLRET_ABORT => Self::Abort,
            XLRET_UNCALCED => Self::Uncalced,
            other => Self::Failed(other),
        }
    }

    pub(crate) fn permits_callback(self) -> bool {
        !matches!(self, Self::Abort | Self::Uncalced)
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Abort | Self::Uncalced)
    }

    pub(crate) fn raw_code(self) -> i32 {
        match self {
            Self::Success => XLRET_SUCCESS,
            Self::Abort => XLRET_ABORT,
            Self::Uncalced => XLRET_UNCALCED,
            Self::Failed(code) => code,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RegistrationDebt {
    pub(crate) id: u64,
    pub(crate) symbol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct GitCookieDebt {
    pub(crate) cookie: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RegistryKeyDebt {
    pub(crate) key_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallbackCleanupDebt {
    pub(crate) status: ExcelCallbackStatus,
}

#[derive(Debug, Default)]
pub(crate) struct CleanupDebtSet {
    pub(crate) registrations: Vec<RegistrationDebt>,
    pub(crate) git_cookies: Vec<GitCookieDebt>,
    pub(crate) registry_keys: Vec<RegistryKeyDebt>,
}

impl CleanupDebtSet {
    pub(crate) fn is_empty(&self) -> bool {
        self.registrations.is_empty()
            && self.git_cookies.is_empty()
            && self.registry_keys.is_empty()
    }
}
