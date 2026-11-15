#[cfg(any(target_os = "windows", test))]
use super::FormulaHandleService;
use super::FormulaLifetimeGeneration;
#[cfg(any(target_os = "windows", test))]
use super::HandleTopicKey;
#[cfg(any(target_os = "windows", test))]
use crate::XllResult;
#[cfg(any(target_os = "windows", test))]
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FormulaObserverId {
    pub(crate) generation: FormulaLifetimeGeneration,
    pub(crate) topic_id: i32,
}

#[cfg(any(target_os = "windows", test))]
/// Provisional Excel topic assignment borrowing the handle runtime that
/// created it. The borrow keeps commit and rollback on the same live runtime
/// without a temporary `Weak` upgrade.
pub(crate) struct HandleConnection<'runtime> {
    disposition: ConnectionDisposition<'runtime>,
    token: String,
}

#[cfg(any(target_os = "windows", test))]
enum ConnectionDisposition<'runtime> {
    Existing,
    Reserved(RollbackGuard<ReservedConnection<'runtime>>),
}

#[cfg(any(target_os = "windows", test))]
struct ReservedConnection<'runtime> {
    runtime: &'runtime FormulaHandleService,
    owner: FormulaObserverId,
    key: HandleTopicKey,
}

#[cfg(any(target_os = "windows", test))]
impl Rollback for ReservedConnection<'_> {
    fn rollback(self) {
        self.runtime.rollback_connection(self.owner, self.key);
    }
}

#[cfg(any(target_os = "windows", test))]
impl HandleConnection<'_> {
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn commit(self) -> XllResult<()> {
        if let ConnectionDisposition::Reserved(reservation) = self.disposition {
            let reserved = reservation.get();
            reserved
                .runtime
                .commit_connection(reserved.owner, reserved.key)?;
            reservation.complete();
        }
        Ok(())
    }
}

#[cfg(any(target_os = "windows", test))]
impl FormulaHandleService {
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn connect_transaction(
        &self,
        lifetime_generation: FormulaLifetimeGeneration,
        topic_id: i32,
        lifetime_key: &str,
    ) -> XllResult<HandleConnection<'_>> {
        let owner = FormulaObserverId {
            generation: lifetime_generation,
            topic_id,
        };
        let (key, token, created) =
            self.connect_inner(lifetime_generation, topic_id, lifetime_key)?;
        Ok(HandleConnection {
            disposition: if created {
                ConnectionDisposition::Reserved(RollbackGuard::new(ReservedConnection {
                    runtime: self,
                    owner,
                    key,
                }))
            } else {
                ConnectionDisposition::Existing
            },
            token,
        })
    }
}
