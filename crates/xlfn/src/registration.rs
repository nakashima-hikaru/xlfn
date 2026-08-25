pub(crate) mod host;
pub(crate) mod ledger;
pub(crate) mod preflight;
#[allow(
    unsafe_code,
    reason = "Excel registration ABI is isolated in this leaf module"
)]
pub(crate) mod registrar;
pub(crate) mod schema;

pub(crate) use ledger::{
    EventRegistration, ExcelNameKey, HostMutationJournal, MetadataDebt, MetadataDebtRetryResult,
    PendingRegistration, RegistrationCertainty, RegistrationCleanupState,
    RegistrationTransactionError, UnknownRegistrationState, UnregisterResult,
};
pub(crate) use preflight::preflight_registration;
pub(crate) use registrar::HostRegistrar;
pub use schema::{ArgumentAbi, ArgumentDescriptor};
pub(crate) use schema::{RegistrationDescriptor, RegistrationId, RegistrationSignature};
