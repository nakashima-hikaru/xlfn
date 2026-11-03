// module: registration
use super::HostMutationJournal;
fn invalid(mut journal: HostMutationJournal) {
    journal.pending_registrations.clear(); // error: E0616
    journal.metadata_debt.clear(); // error: E0616
    journal.unknown_registrations.clear(); // error: E0616
}
