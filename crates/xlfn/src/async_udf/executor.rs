//! Executor extension contract independent of task lifecycle and delivery.

use super::AsyncTask;
use crate::addin::NoAsyncExecutor;
use crate::{XllError, XllResult};
use std::future::Future;

/// Schedules framework-owned async tasks on an application-selected executor.
///
/// The framework starts the executor once during opening, before registering
/// worksheet functions. It first reserves executor capacity and task-registry
/// admission, then publishes an [`AsyncTask`] through infallible `submit`.
/// Reservations must own every executor resource needed by publication.
///
/// Cancellation, calculation generations, completion and Excel delivery remain
/// framework responsibilities. Before shutdown the framework closes task
/// admission, cancels registered tasks and waits for their final destruction.
/// `shutdown` must drain executor-owned scheduler callbacks and pollers before
/// returning success. It is also called after a partially failed `start`.
/// A shutdown error retains the executor and prevents a stopped certificate.
pub trait AsyncExecutor: Send + Sync + 'static {
    /// Owned publication capability issued before a task is registered.
    type Reservation: Send + 'static;

    /// Starts this previously idle executor for one add-in opening.
    fn start(&self) -> XllResult<()>;

    /// Reserves capacity before task registration or publication can occur.
    fn reserve(&self) -> XllResult<Self::Reservation>;

    /// Consumes a reservation and transfers an admitted task to the executor.
    ///
    /// This operation cannot report failure after framework publication. A
    /// concurrent executor failure must destroy the task safely so its registry
    /// ownership can complete.
    /// The concrete future stays inline; executors can store it in their own
    /// task allocation. Executors that need heterogeneous storage may erase
    /// its type at that storage boundary.
    fn submit<F>(&self, reservation: Self::Reservation, task: AsyncTask<F>)
    where
        F: Future<Output = ()> + Send + 'static;

    /// Drains executor resources after the framework's task registry is empty.
    fn shutdown(&self) -> XllResult<()>;
}

impl AsyncExecutor for NoAsyncExecutor {
    type Reservation = std::convert::Infallible;

    fn start(&self) -> XllResult<()> {
        Ok(())
    }

    fn reserve(&self) -> XllResult<Self::Reservation> {
        Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::ASYNC_SPAWN,
        })
    }

    fn submit<F>(&self, reservation: Self::Reservation, _task: AsyncTask<F>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        match reservation {}
    }

    fn shutdown(&self) -> XllResult<()> {
        Ok(())
    }
}
