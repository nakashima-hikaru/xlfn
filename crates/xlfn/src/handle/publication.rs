//! Linear cold-path publication protocol for formula handles.
//!
//! The handle store and topic table remain separate ownership domains.  This
//! module owns the transaction that coordinates them, so a provisional topic
//! cannot be committed without its binding and single-flight reservation.

use super::runtime::{FormulaHandleService, PreparedHandleObject};
use super::{ExcelHandleObject, HandleTopicKey, Initialization, PublishedTopic};
use crate::XllResult;
use crate::generation::TopicGeneration;
use std::sync::Arc;

/// Owns the single-flight marker for one cold topic preparation.
///
/// The marker is removed by `commit_publication` on success. If any earlier
/// step fails, dropping this reservation removes the marker and wakes all
/// waiters.
pub(super) struct TopicReservation<'runtime> {
    runtime: &'runtime FormulaHandleService,
    key: HandleTopicKey,
    initialization: Arc<Initialization>,
    active: bool,
}

impl<'runtime> TopicReservation<'runtime> {
    pub(super) fn new(
        runtime: &'runtime FormulaHandleService,
        key: HandleTopicKey,
        initialization: Arc<Initialization>,
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

    fn commit(mut self) {
        self.active = false;
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
    pub(super) transaction: ProvisionalPublicationTxn<'runtime>,
    pub(super) token: String,
    pub(super) binding_id: super::HandleId,
    pub(super) object_id: super::ObjectId,
    pub(super) allocation: ObjectAllocation,
}

pub(super) struct ProvisionalPublicationTxn<'runtime> {
    runtime: &'runtime FormulaHandleService,
    key: HandleTopicKey,
    generation: TopicGeneration,
    reservation: TopicReservation<'runtime>,
    provisional: ProvisionalPublication<'runtime>,
}

pub(super) struct ObservedPublicationTxn<'runtime> {
    runtime: &'runtime FormulaHandleService,
    key: HandleTopicKey,
    generation: TopicGeneration,
    reservation: TopicReservation<'runtime>,
    provisional: ProvisionalPublication<'runtime>,
}

impl<'runtime> PublicationReservation<'runtime> {
    pub(super) fn new(
        runtime: &'runtime FormulaHandleService,
        key: HandleTopicKey,
        generation: TopicGeneration,
        initialization: Arc<Initialization>,
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
        prepared: PreparedHandleObject,
    ) -> XllResult<InsertedPublication<'runtime>> {
        let (token, binding_id, object_id, reused) = match prepared {
            PreparedHandleObject::New(value) => self
                .runtime
                .store
                .insert_pending::<T>(value.into_shared())?,
            PreparedHandleObject::Existing(object) => self
                .runtime
                .store
                .insert_existing::<T>(object.into_shared())?,
        };
        let provisional = ProvisionalPublication::new(
            self.runtime,
            self.key,
            token.clone(),
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
                reservation,
                provisional,
            },
            token,
            binding_id,
            object_id,
            allocation: if reused {
                ObjectAllocation::Reused
            } else {
                ObjectAllocation::Fresh
            },
        })
    }
}

impl<'runtime> ProvisionalPublicationTxn<'runtime> {
    pub(super) fn publish_and_observe(
        self,
        publication: triomphe::Arc<PublishedTopic>,
        lifetime_key: Arc<str>,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
        on_linearized: impl FnOnce(),
    ) -> XllResult<ObservedPublicationTxn<'runtime>> {
        let token = &self.provisional.token;
        self.runtime.topics.insert_provisional(
            self.key,
            self.generation,
            triomphe::Arc::clone(&publication),
            on_linearized,
        )?;
        self.runtime
            .topics
            .is_current(self.key, self.generation, token)?;
        observe(&lifetime_key, token)?;
        self.runtime
            .topics
            .is_current(self.key, self.generation, token)?;
        let Self {
            runtime,
            key,
            generation,
            reservation,
            provisional,
        } = self;
        Ok(ObservedPublicationTxn {
            runtime,
            key,
            generation,
            reservation,
            provisional,
        })
    }
}

impl ObservedPublicationTxn<'_> {
    pub(super) fn commit(self, publication: &triomphe::Arc<PublishedTopic>) -> XllResult<()> {
        let Self {
            runtime,
            key,
            generation,
            reservation,
            provisional,
        } = self;
        runtime.commit_publication(key, generation, &reservation.initialization, publication)?;
        provisional.commit();
        reservation.commit();
        Ok(())
    }
}
