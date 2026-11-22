//! Native RTD transitions also executed by the Verus entry-state model.

macro_rules! finish_preparation {
    ($phase:expr, $entry:expr, $ticket:expr, $committed:expr, $count:ident => $consume:expr) => {{
        if $entry.runtime != $ticket.runtime || $entry.id != $ticket.id {
            None
        } else {
            Some(match $phase {
                SubscriptionPhase::Pending {
                    reservations: $count,
                    server,
                } => {
                    let reservations = $consume;
                    if $committed {
                        SubscriptionPhase::AwaitConnect {
                            reservations,
                            server,
                        }
                    } else {
                        SubscriptionPhase::Pending {
                            reservations,
                            server,
                        }
                    }
                }
                SubscriptionPhase::AwaitConnect {
                    reservations: $count,
                    server,
                } => SubscriptionPhase::AwaitConnect {
                    reservations: $consume,
                    server,
                },
                SubscriptionPhase::Connecting {
                    reservations: $count,
                    server,
                    connection,
                    commitment,
                } => {
                    let commitment = if $committed {
                        PendingCommitment::Committed
                    } else {
                        commitment
                    };
                    SubscriptionPhase::Connecting {
                        reservations: $consume,
                        server,
                        connection,
                        commitment,
                    }
                }
                SubscriptionPhase::Active {
                    reservations: $count,
                    server,
                    connection,
                } => SubscriptionPhase::Active {
                    reservations: $consume,
                    server,
                    connection,
                },
                SubscriptionPhase::Revoked {
                    reservations: $count,
                } => SubscriptionPhase::Revoked {
                    reservations: $consume,
                },
            })
        }
    }};
}

macro_rules! revoke_server {
    ($phase:expr, $server:expr) => {{
        match $phase {
            SubscriptionPhase::Pending { server, reservations }
            | SubscriptionPhase::AwaitConnect { server, reservations } => {
                if matches!(server, Some(observed) if observed == $server) {
                    Some(SubscriptionPhase::Revoked { reservations })
                } else {
                    None
                }
            }
            SubscriptionPhase::Connecting { server, reservations, .. }
                if server == $server =>
            {
                Some(SubscriptionPhase::Revoked { reservations })
            }
            _ => None,
        }
    }};
}

macro_rules! revoke_connection {
    ($phase:expr, $server:expr, $connection:expr) => {{
        match $phase {
            SubscriptionPhase::Connecting {
                reservations,
                server,
                connection,
                ..
            }
            | SubscriptionPhase::Active {
                reservations,
                server,
                connection,
            } => {
                if server == $server && connection == $connection {
                    Some(SubscriptionPhase::Revoked { reservations })
                } else {
                    None
                }
            }
            _ => None,
        }
    }};
}

pub(crate) use {finish_preparation, revoke_connection, revoke_server};
