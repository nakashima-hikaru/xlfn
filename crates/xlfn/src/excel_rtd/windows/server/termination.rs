//! Owns server/termination pairing from request through completion.
use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

pub(super) struct ServerTerminationOwner<'server, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
{
    server: &'server RtdServer<E, C>,
    request: ServerTerminationRequest<'server>,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> RtdServer<E, C> {
    pub(super) fn request_terminal(
        &self,
    ) -> Result<ServerTerminationOwner<'_, E, C>, ServerCloseError> {
        Ok(ServerTerminationOwner {
            server: self,
            request: ServerOperationBarrier::request_termination(&self.operations)?,
        })
    }

    pub(super) fn terminate(&self) -> i32 {
        let request = match self.request_terminal() {
            Ok(request) => request,
            Err(_) => return E_FAIL,
        };
        request.finish()
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> ServerTerminationOwner<'_, E, C> {
    pub(super) fn finish(self) -> i32 {
        finish_termination_request(self.server, self.request)
    }
}

fn finish_termination_request<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    server: &RtdServer<E, C>,
    request: ServerTerminationRequest<'_>,
) -> i32 {
    match request {
        ServerTerminationRequest::Complete | ServerTerminationRequest::InProgress => S_OK,
        ServerTerminationRequest::Synchronous(termination) => {
            // SAFETY: COM retains `this` through this synchronous request.
            let access = from_termination(server, termination);
            let joined = match access.join_worker() {
                Ok(joined) => joined,
                Err(_) => return E_FAIL,
            };
            joined.teardown_and_remove_active(false)
        }
        ServerTerminationRequest::Deferred(reservation) => {
            // The consuming startup owner holds the gate lock through spawn,
            // handle registration and commit. No detached ticket can be lost
            // before a separate caller commits the termination phase.
            // SAFETY: COM retains this server through the complete request.
            let start = match reservation.start(server) {
                Ok(start) => start,
                Err(_) => return E_FAIL,
            };
            match start.spawn() {
                Ok(()) => S_OK,
                Err(_) => E_FAIL,
            }
        }
    }
}

type ExactServerType<E, C> = std::marker::PhantomData<fn(E, C) -> (E, C)>;

/// The worker receives the server lifetime and its exact completion ticket as
/// one private payload; it cannot substitute a target or concrete server type.
pub(in crate::excel_rtd::windows) struct DeferredTerminationWork<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
> {
    reference: OwnedServerReference,
    ticket: DeferredTerminationTicket,
    _catalog: ExactServerType<E, C>,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> DeferredTerminationWork<E, C> {
    pub(in crate::excel_rtd::windows) fn new(
        binding: super::super::server_gate::DeferredServerTicket<'_, E, C>,
    ) -> Self {
        let (server, ticket) = binding.into_parts();
        // SAFETY: the checked startup binding retains the requesting COM call.
        // The new reference covers this exact ticket's deferred worker.
        let reference = unsafe { OwnedServerReference::acquire(server) };
        Self {
            reference,
            ticket,
            _catalog: std::marker::PhantomData,
        }
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> DeferredTerminationWork<E, C> {
    pub(in crate::excel_rtd::windows) fn spawn(
        self,
    ) -> std::io::Result<std::thread::JoinHandle<()>> {
        spawn_deferred_termination(self)
    }
}

fn spawn_deferred_termination<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    work: DeferredTerminationWork<E, C>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    #[cfg(test)]
    if FAIL_DEFERRED_TERMINATION_SPAWN.swap(false, Ordering::AcqRel) {
        return Err(std::io::Error::other(
            "injected deferred RTD termination spawn failure",
        ));
    }

    std::thread::Builder::new()
        .name("xlfn-rtd-termination".to_owned())
        .spawn(move || deferred_termination_worker(work))
}

fn deferred_termination_worker<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    work: DeferredTerminationWork<E, C>,
) {
    let DeferredTerminationWork {
        reference, ticket, ..
    } = work;
    let this = reference.pointer.get() as *mut RtdServer<E, C>;
    // Keep the internal reference outside the unwind boundary. It remains live
    // until all cleanup and phase signaling have completed.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let result = ticket.with_completion(|termination| {
            #[cfg(test)]
            if PANIC_DEFERRED_TERMINATION_CLEANUP.swap(false, Ordering::AcqRel) {
                panic!("injected deferred RTD cleanup panic");
            }

            // Deferred cleanup intentionally retains ACTIVE_SERVER. The next
            // xlAutoRemove or ensure_server joins this worker before removing that
            // final module-lifetime reference.
            // SAFETY: the worker's reference retains this exact server.
            let access = from_termination(unsafe { &*this }, termination);
            let status = access.teardown_resources();
            if status != S_OK {
                crate::diagnostics::report_no_unwind(
                    "IRtdServer::deferred termination",
                    &XllError::WindowsApi {
                        function: "IRtdServer::ServerTerminate",
                        code: status,
                    },
                );
            }
            drop(access);
        });
        if let Err(error) = result {
            crate::diagnostics::report_no_unwind(
                "IRtdServer::deferred termination wait",
                &XllError::Internal {
                    diagnostic_id: match error {
                        ServerCloseError::WaitFailed(status) => {
                            crate::diagnostics::id::DiagnosticId::RTD_WINDOW_STATUS
                                .with_low_u32(status as u32)
                        }
                        _ => crate::diagnostics::id::DiagnosticId::RTD_WINDOW_FAILURE,
                    },
                },
            );
            // Reopening after a committed spawn would strand a completed
            // worker handle on a reusable server. A live event cannot fail.
            std::process::abort();
        }
    }));

    if let Err(payload) = outcome {
        crate::diagnostics::report_no_unwind("IRtdServer::deferred termination", &XllError::Panic);
        drop(reference);
        std::panic::resume_unwind(payload);
    }
    drop(reference);
}

fn from_termination<'server, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    server: &'server RtdServer<E, C>,
    termination: ServerTermination<'server>,
) -> QuiescentServerAccess<'server, E, C> {
    let drained = termination.into_drained();
    // SAFETY: the borrowed server is retained by this call's COM owner.
    let reference = unsafe { OwnedServerReference::acquire(server) };
    QuiescentServerAccess {
        server,
        drained,
        _reference: Some(reference),
    }
}
