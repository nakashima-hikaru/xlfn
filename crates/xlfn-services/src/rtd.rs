//! Host-independent RTD entry ownership, identity indexing and quota accounting.

pub mod index;
pub mod quota;
mod transitions;

use transitions::{finish_preparation, revoke_connection, revoke_server};

use std::num::NonZeroUsize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingCommitment {
    Uncommitted,
    Committed,
}

/// A transport key's identity is never reused, even when the same formula
/// starts another preparation after its old connection was revoked.
#[derive(Clone, Copy, PartialEq, Eq)]
struct EntryIncarnation<Id> {
    runtime: u64,
    id: Id,
}

pub struct PrepareTicket<Id>(EntryIncarnation<Id>);

impl<Id: Copy> PrepareTicket<Id> {
    pub fn id(&self) -> Id {
        self.0.id
    }
}

enum SubscriptionPhase<Server, Connection> {
    Pending {
        reservations: Option<NonZeroUsize>,
        server: Option<Server>,
    },
    AwaitConnect {
        reservations: Option<NonZeroUsize>,
        server: Option<Server>,
    },
    Connecting {
        reservations: Option<NonZeroUsize>,
        server: Server,
        connection: Connection,
        commitment: PendingCommitment,
    },
    Active {
        server: Server,
        connection: Connection,
        reservations: Option<NonZeroUsize>,
    },
    /// Unindexed and unable to connect. Until all old preparations return,
    /// this entry still owns its pending and topic-byte quota.
    Revoked { reservations: Option<NonZeroUsize> },
}

/// This affine ticket is issued by the entry which begins the connection;
/// only its exact incarnation and server/connection generations may use it.
pub struct ConnectionTicket<Id, Server, Connection> {
    entry: EntryIncarnation<Id>,
    server: Server,
    connection: Connection,
}

impl<Id, Server, Connection: Copy> ConnectionTicket<Id, Server, Connection> {
    pub fn generation(&self) -> Connection {
        self.connection
    }
}

pub enum PreparationFinish {
    Keep,
    Remove,
}

/// Owns one entry's phase and affine preparation and connection tickets.
/// Generations are supplied by the transport/runtime; this state never invokes it.
pub struct EntryState<Id, Server, Connection> {
    phase: SubscriptionPhase<Server, Connection>,
    incarnation: EntryIncarnation<Id>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryError {
    ActiveKeyDuplicate,
    Closing,
    ReservationOverflow,
    ServerGenerationMismatch,
    ConnectionInFlight,
}

impl<Id: Copy + Eq, Server: Copy + Eq, Connection: Copy + Eq> EntryState<Id, Server, Connection> {
    /// Starts an incarnation with its first preparation already reserved.
    /// The caller must issue a fresh `(runtime, id)` pair, never reusing one
    /// that belonged to another entry in this process.
    pub fn new_pending(runtime: u64, id: Id) -> (Self, PrepareTicket<Id>) {
        let incarnation = EntryIncarnation { runtime, id };
        (
            Self {
                incarnation,
                phase: SubscriptionPhase::Pending {
                    reservations: Some(NonZeroUsize::MIN),
                    server: None,
                },
            },
            PrepareTicket(incarnation),
        )
    }

    pub fn is_connected(&self) -> bool {
        matches!(
            self.phase,
            SubscriptionPhase::Connecting { .. } | SubscriptionPhase::Active { .. }
        )
    }

    pub fn is_active(&self) -> bool {
        matches!(self.phase, SubscriptionPhase::Active { .. })
    }

    pub fn is_identifiable(&self) -> bool {
        !matches!(self.phase, SubscriptionPhase::Revoked { .. })
    }

    pub fn counts_as_pending(&self) -> bool {
        !matches!(
            self.phase,
            SubscriptionPhase::Active {
                reservations: None,
                ..
            }
        )
    }

    pub fn can_remove(&self) -> bool {
        matches!(
            self.phase,
            SubscriptionPhase::Pending {
                reservations: None,
                ..
            } | SubscriptionPhase::Revoked { reservations: None }
        )
    }

    pub fn server_generation(&self) -> Option<Server> {
        match &self.phase {
            SubscriptionPhase::Pending { server, .. }
            | SubscriptionPhase::AwaitConnect { server, .. } => *server,
            SubscriptionPhase::Connecting { server, .. }
            | SubscriptionPhase::Active { server, .. } => Some(*server),
            SubscriptionPhase::Revoked { .. } => None,
        }
    }

    pub fn connection_generation(&self) -> Option<Connection> {
        match &self.phase {
            SubscriptionPhase::Pending { .. }
            | SubscriptionPhase::AwaitConnect { .. }
            | SubscriptionPhase::Revoked { .. } => None,
            SubscriptionPhase::Connecting { connection, .. }
            | SubscriptionPhase::Active { connection, .. } => Some(*connection),
        }
    }

    pub fn add_reservation(&mut self) -> Result<PrepareTicket<Id>, EntryError> {
        let reservations = match &mut self.phase {
            SubscriptionPhase::Pending { reservations, .. }
            | SubscriptionPhase::AwaitConnect { reservations, .. }
            | SubscriptionPhase::Connecting { reservations, .. } => reservations,
            SubscriptionPhase::Active { .. } => {
                return Err(EntryError::ActiveKeyDuplicate);
            }
            SubscriptionPhase::Revoked { .. } => return Err(EntryError::Closing),
        };
        *reservations = Some(
            reservations
                .map_or(Some(NonZeroUsize::MIN), |count| count.checked_add(1))
                .ok_or(EntryError::ReservationOverflow)?,
        );
        Ok(PrepareTicket(self.incarnation))
    }

    pub fn finish_preparation(
        &mut self,
        ticket: PrepareTicket<Id>,
        committed: bool,
    ) -> PreparationFinish {
        let next = finish_preparation!(
            self.phase, self.incarnation, ticket.0, committed,
            reservations => {
                let Some(count) = reservations else {
                    xlfn_kernel::invariant::fail_stop();
                };
                NonZeroUsize::new(count.get() - 1)
            }
        );
        let Some(next) = next else {
            return PreparationFinish::Keep;
        };
        self.phase = next;
        if self.can_remove() {
            PreparationFinish::Remove
        } else {
            PreparationFinish::Keep
        }
    }

    pub fn claim_server(&mut self, generation: Server) -> Result<(), EntryError> {
        let server = match &mut self.phase {
            SubscriptionPhase::Pending { server, .. }
            | SubscriptionPhase::AwaitConnect { server, .. } => server,
            SubscriptionPhase::Connecting { server, .. }
            | SubscriptionPhase::Active { server, .. } => {
                if *server != generation {
                    return Err(EntryError::ServerGenerationMismatch);
                }
                return Ok(());
            }
            SubscriptionPhase::Revoked { .. } => return Err(EntryError::Closing),
        };
        if let Some(existing) = *server {
            if existing != generation {
                return Err(EntryError::ServerGenerationMismatch);
            }
        } else {
            *server = Some(generation);
        }
        Ok(())
    }

    pub fn begin_connection(
        &mut self,
        server: Server,
        connection: Connection,
    ) -> Result<ConnectionTicket<Id, Server, Connection>, EntryError> {
        let (reservations, commitment, existing_server) = match &self.phase {
            SubscriptionPhase::Pending {
                reservations,
                server,
            } => (*reservations, PendingCommitment::Uncommitted, *server),
            SubscriptionPhase::AwaitConnect {
                reservations,
                server,
            } => (*reservations, PendingCommitment::Committed, *server),
            SubscriptionPhase::Connecting { .. } => {
                return Err(EntryError::ConnectionInFlight);
            }
            SubscriptionPhase::Active { .. } => {
                return Err(EntryError::ActiveKeyDuplicate);
            }
            SubscriptionPhase::Revoked { .. } => return Err(EntryError::Closing),
        };
        if let Some(existing) = existing_server
            && existing != server
        {
            return Err(EntryError::ServerGenerationMismatch);
        }
        self.phase = SubscriptionPhase::Connecting {
            reservations,
            server,
            connection,
            commitment,
        };
        Ok(ConnectionTicket {
            entry: self.incarnation,
            server,
            connection,
        })
    }

    pub fn rollback_connection(
        &mut self,
        ticket: &ConnectionTicket<Id, Server, Connection>,
    ) -> bool {
        let (reservations, server, commitment) = match &self.phase {
            SubscriptionPhase::Connecting {
                reservations,
                server,
                connection,
                commitment,
            } if ticket.entry == self.incarnation
                && *connection == ticket.connection
                && *server == ticket.server =>
            {
                (*reservations, Some(*server), *commitment)
            }
            _ => return false,
        };
        self.phase = match commitment {
            PendingCommitment::Uncommitted => SubscriptionPhase::Pending {
                reservations,
                server,
            },
            PendingCommitment::Committed => SubscriptionPhase::AwaitConnect {
                reservations,
                server,
            },
        };
        true
    }

    pub fn finish_connection(&mut self, ticket: &ConnectionTicket<Id, Server, Connection>) -> bool {
        let (reservations, server) = match &self.phase {
            SubscriptionPhase::Connecting {
                reservations,
                server,
                connection,
                ..
            } if ticket.entry == self.incarnation
                && *connection == ticket.connection
                && *server == ticket.server =>
            {
                (*reservations, *server)
            }
            _ => return false,
        };
        self.phase = SubscriptionPhase::Active {
            server,
            connection: ticket.connection,
            reservations,
        };
        true
    }

    pub fn revoke_for_server_termination(&mut self, server: Server) -> bool {
        let Some(next) = revoke_server!(self.phase, server) else {
            return false;
        };
        self.phase = next;
        true
    }

    pub fn cleanup_connection(&mut self, server: Server, connection: Connection) -> (bool, bool) {
        let Some(next) = revoke_connection!(self.phase, server, connection) else {
            return (false, false);
        };
        self.phase = next;
        (true, self.can_remove())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type State = EntryState<u32, u32, u32>;

    #[test]
    fn matching_connection_finishes_and_last_preparation_releases_pending_charge() {
        let (mut entry, first) = State::new_pending(1, 1);
        let second = entry.add_reservation().unwrap();
        assert!(entry.counts_as_pending());
        assert!(entry.is_identifiable());
        entry.claim_server(1).unwrap();
        let connection = entry.begin_connection(1, 3).unwrap();
        assert!(entry.finish_connection(&connection));
        assert!(entry.counts_as_pending());
        assert!(matches!(
            entry.finish_preparation(second, true),
            PreparationFinish::Keep
        ));
        assert!(entry.counts_as_pending());
        assert!(matches!(
            entry.finish_preparation(first, false),
            PreparationFinish::Keep
        ));
        assert!(!entry.counts_as_pending());
        assert!(entry.is_active());
        assert_eq!(entry.cleanup_connection(1, 3), (true, true));
        assert!(!entry.is_identifiable());
        assert!(entry.can_remove());
    }

    #[test]
    fn reused_generation_numbers_cannot_authorize_new_entry_or_runtime() {
        let (mut entry, old_prepare) = State::new_pending(1, 1);
        let old_connection = entry.begin_connection(1, 1).unwrap();
        assert!(entry.revoke_for_server_termination(1));
        assert!(matches!(
            entry.begin_connection(2, 1),
            Err(EntryError::Closing)
        ));
        assert!(entry.counts_as_pending());
        assert!(!entry.is_identifiable());
        assert!(!entry.can_remove());

        // Same server/connection and subscription ID, but a new runtime.
        let (mut replacement, current_prepare) = State::new_pending(2, 1);
        let current_connection = replacement.begin_connection(1, 1).unwrap();
        assert!(!replacement.finish_connection(&old_connection));
        assert!(!replacement.rollback_connection(&old_connection));
        assert!(matches!(
            replacement.finish_preparation(old_prepare, true),
            PreparationFinish::Keep
        ));
        assert!(replacement.finish_connection(&current_connection));
        assert!(replacement.counts_as_pending());
        assert!(matches!(
            replacement.finish_preparation(current_prepare, true),
            PreparationFinish::Keep
        ));
        assert!(!replacement.counts_as_pending());

        // Within that runtime a distinct entry cannot claim the same ticket.
        let (mut next_entry, _) = State::new_pending(2, 2);
        let current = next_entry.begin_connection(1, 1).unwrap();
        assert!(!next_entry.finish_connection(&current_connection));
        assert!(!next_entry.rollback_connection(&current_connection));
        assert!(next_entry.finish_connection(&current));
    }

    #[test]
    fn revoked_entry_never_reestablishes_commitment_even_with_live_preparation() {
        let (mut old, old_ticket) = State::new_pending(1, 7);
        let (mut new, new_ticket) = State::new_pending(1, 8);
        let other_old = old.add_reservation().unwrap();
        old.claim_server(11).unwrap();
        assert!(old.revoke_for_server_termination(11));

        assert!(matches!(
            new.finish_preparation(other_old, true),
            PreparationFinish::Keep
        ));
        assert!(matches!(new.phase, SubscriptionPhase::Pending { .. }));
        assert!(matches!(
            old.finish_preparation(old_ticket, true),
            PreparationFinish::Keep
        ));
        assert!(matches!(old.phase, SubscriptionPhase::Revoked { .. }));
        assert!(!old.is_identifiable());
        assert!(matches!(
            new.finish_preparation(new_ticket, false),
            PreparationFinish::Remove
        ));
    }

    #[test]
    fn rollback_preserves_commitment_and_disconnected_pending_retains_obligations() {
        let (mut entry, pending) = State::new_pending(1, 1);
        assert!(matches!(
            entry.finish_preparation(pending, true),
            PreparationFinish::Keep
        ));
        entry.claim_server(5).unwrap();
        assert_eq!(
            entry.claim_server(7),
            Err(EntryError::ServerGenerationMismatch)
        );
        let prior = entry.begin_connection(5, 8).unwrap();
        assert!(entry.rollback_connection(&prior));
        assert!(
            !entry.can_remove(),
            "committed entry still awaits connection"
        );
        let preparation = entry.add_reservation().unwrap();
        let current = entry.begin_connection(5, 9).unwrap();
        assert!(!entry.finish_connection(&prior));
        assert!(!entry.rollback_connection(&prior));
        assert_eq!(entry.cleanup_connection(7, 9), (false, false));
        assert_eq!(entry.cleanup_connection(5, 9), (true, false));
        assert!(!entry.finish_connection(&current));
        assert!(matches!(
            entry.finish_preparation(preparation, true),
            PreparationFinish::Remove
        ));
    }
}
