pub(crate) use crate::error::ExcelCallbackStatus;

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
