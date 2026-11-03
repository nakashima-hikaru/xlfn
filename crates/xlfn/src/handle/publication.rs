//! Linear cold-path publication protocol for formula handles.
//!
//! The handle store and topic table remain separate ownership domains.  This
//! module owns the transaction that coordinates them, so a provisional topic
//! cannot be committed without its binding and single-flight reservation.

use super::runtime::{FormulaHandleService, PreparedHandleObject};
use super::{
    ExcelHandleObject, HandleTopicKey, InitializationPtr, PublishedTopic, PublishedTopicPtr,
};
use crate::XllResult;
use crate::generation::TopicGeneration;

/// Owns the single-flight marker for one cold topic preparation.
///
/// The marker is removed by `commit_publication` on success. If any earlier
/// step fails, dropping this reservation removes the marker and wakes all
/// waiters.
pub(super) struct TopicReservation<'runtime> {
    runtime: &'runtime FormulaHandleService,
    key: HandleTopicKey,
    initialization: InitializationPtr,
    active: bool,
}

impl<'runtime> TopicReservation<'runtime> {
    pub(super) fn new(
        runtime: &'runtime FormulaHandleService,
        key: HandleTopicKey,
        initialization: InitializationPtr,
    ) -> Self {
        Self {
            runtime,
            key,
            initialization,
            active: true,
        }
    }

    pub(super) fn commit(mut self) {
        self.active = false;
    }
}

impl Drop for TopicReservation<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if self
            .runtime
            .topics
            .finish_initialization(self.key, &self.initialization)
        {
            self.runtime
                .refinement
                .observe_finish_initializer(self.initialization.refinement_id);
        }
        self.initialization.complete();
    }
}

/// Owns a binding and its provisional topic until publication is committed.
struct ProvisionalPublication<'runtime> {
    runtime: &'runtime FormulaHandleService,
    key: HandleTopicKey,
    token: String,
    refinement_id: u64,
    active: bool,
}

impl<'runtime> ProvisionalPublication<'runtime> {
    fn new(
        runtime: &'runtime FormulaHandleService,
        key: HandleTopicKey,
        token: String,
        refinement_id: u64,
    ) -> Self {
        Self {
            runtime,
            key,
            token,
            refinement_id,
            active: true,
        }
    }

    fn commit(mut self) -> String {
        self.active = false;
        std::mem::take(&mut self.token)
    }
}

impl Drop for ProvisionalPublication<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let token_wire = self.runtime.refinement_token(&self.token);
        let refinement = &self.runtime.refinement;
        let key = self.key;
        let refinement_id = self.refinement_id;
        let removed = self
            .runtime
            .topics
            .remove_topic_if_token(self.key, &self.token, || {
                refinement.observe_withdraw_and_invalidate(&key, refinement_id, token_wire);
            })
            .is_some();
        let _ = self.runtime.store.remove_and_drop_with_observer(
            &self.token,
            "handle publication rollback",
            move |reusable| {
                if removed {
                    refinement.observe_rollback_pending(&key, refinement_id, reusable, token_wire);
                }
            },
        );
    }
}

/// The cold publication protocol owns both storage reservations. The
/// underlying handle store and topic table remain separate, but callers can
/// no longer commit one side without carrying the other side through the
/// same linear transaction.
pub(crate) struct PublicationReservation<'runtime> {
    runtime: &'runtime FormulaHandleService,
    key: HandleTopicKey,
    generation: TopicGeneration,
    reservation: TopicReservation<'runtime>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ObjectAllocation {
    Fresh,
    Reused,
}

pub(crate) struct InsertedPublication<'runtime> {
    transaction: ProvisionalPublicationTxn<'runtime>,
    allocation: ObjectAllocation,
}

pub(super) struct ProvisionalPublicationTxn<'runtime> {
    runtime: &'runtime FormulaHandleService,
    key: HandleTopicKey,
    generation: TopicGeneration,
    // Drop order is a synchronization invariant: withdraw the provisional
    // publication before releasing the single-flight reservation and waking
    // waiters.
    provisional: ProvisionalPublication<'runtime>,
    reservation: TopicReservation<'runtime>,
}

pub(super) struct ObservedPublicationTxn<'runtime> {
    runtime: &'runtime FormulaHandleService,
    key: HandleTopicKey,
    generation: TopicGeneration,
    publication: PublishedTopicPtr,
    provisional: ProvisionalPublication<'runtime>,
    reservation: TopicReservation<'runtime>,
}

impl<'runtime> PublicationReservation<'runtime> {
    pub(super) fn new(
        runtime: &'runtime FormulaHandleService,
        key: HandleTopicKey,
        generation: TopicGeneration,
        initialization: InitializationPtr,
    ) -> Self {
        Self {
            runtime,
            key,
            generation,
            reservation: TopicReservation::new(runtime, key, initialization),
        }
    }

    pub(super) fn insert_object<T: ExcelHandleObject>(
        self,
        prepared: PreparedHandleObject<'_, T>,
    ) -> XllResult<InsertedPublication<'runtime>> {
        // Produce and publish through the reservation's own store. A pending
        // binding from an unrelated registry cannot be supplied here.
        let pending = match prepared {
            PreparedHandleObject::New(value) => self.runtime.store.erase(value)?,
            PreparedHandleObject::Existing(alias) => {
                alias.into_pending_object_binding(&self.runtime.store)?
            }
        };
        let (token, _binding_id, _object_id, reused) = pending.publish()?;
        let provisional = ProvisionalPublication::new(
            self.runtime,
            self.key,
            token,
            self.reservation.initialization.refinement_id,
        );
        let Self {
            runtime,
            key,
            generation,
            reservation,
        } = self;
        Ok(InsertedPublication {
            transaction: ProvisionalPublicationTxn {
                runtime,
                key,
                generation,
                provisional,
                reservation,
            },
            allocation: if reused {
                ObjectAllocation::Reused
            } else {
                ObjectAllocation::Fresh
            },
        })
    }
}

impl<'runtime> InsertedPublication<'runtime> {
    pub(super) fn token(&self) -> &str {
        &self.transaction.provisional.token
    }

    pub(super) fn allocation(&self) -> &ObjectAllocation {
        &self.allocation
    }

    pub(super) fn publish_and_observe(
        self,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
        on_linearized: impl FnOnce(PublishedTopicPtr, &str),
    ) -> XllResult<ObservedPublicationTxn<'runtime>> {
        let transaction = self.transaction;
        let token = &transaction.provisional.token;
        let lifetime_key = transaction.key.format_lifetime_key();
        // Publication identity comes from this binding and topic reservation;
        // callers cannot inject a different token, key or allocation owner.
        let publication = PublishedTopic::new(token.clone(), lifetime_key.clone());
        let publication = transaction.runtime.topics.insert_provisional(
            transaction.key,
            transaction.generation,
            publication,
            |pointer| on_linearized(pointer, &lifetime_key),
        )?;
        transaction
            .runtime
            .topics
            .is_current(transaction.key, transaction.generation, token)?;
        // The caller owns this key independently of the topic, which observation
        // may remove reentrantly.
        observe(&lifetime_key, token)?;
        transaction
            .runtime
            .topics
            .is_current(transaction.key, transaction.generation, token)?;
        let ProvisionalPublicationTxn {
            runtime,
            key,
            generation,
            provisional,
            reservation,
        } = transaction;
        Ok(ObservedPublicationTxn {
            runtime,
            key,
            generation,
            publication,
            provisional,
            reservation,
        })
    }
}

impl ObservedPublicationTxn<'_> {
    pub(super) fn commit(self) -> XllResult<String> {
        let Self {
            runtime,
            key,
            generation,
            publication,
            provisional,
            reservation,
        } = self;
        runtime.commit_publication(
            key,
            generation,
            &reservation.initialization,
            publication,
            &provisional.token,
        )?;
        let token = provisional.commit();
        reservation.commit();
        Ok(token)
    }
}
