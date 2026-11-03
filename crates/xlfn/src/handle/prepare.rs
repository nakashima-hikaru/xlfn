use xlfn_kernel::drain_gate::{DEFAULT_STRIPE_COUNT, StripedDrainGate, StripedDrainPermit};

pub(crate) struct HandlePrepareState {
    admission: StripedDrainGate<DEFAULT_STRIPE_COUNT>,
}

impl HandlePrepareState {
    pub(crate) const fn new() -> Self {
        Self {
            admission: StripedDrainGate::new_open(),
        }
    }

    pub(crate) fn try_enter(&self) -> Option<StripedDrainPermit<'_, DEFAULT_STRIPE_COUNT>> {
        self.admission.try_enter_current().ok()
    }

    pub(crate) fn close_admission(&self) {
        drop(self.admission.control().seal());
    }

    pub(crate) fn wait_for_idle(&self) {
        // Counter registration synchronizes the final release with parking.
        drop(self.admission.control().wait_idle());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_admission_rejects_new_prepares_and_drains_existing_ones() {
        let state = HandlePrepareState::new();
        let active = state.try_enter().expect("prepare admission starts open");

        state.close_admission();
        assert!(state.try_enter().is_none());

        drop(active);
        state.wait_for_idle();
    }

    #[test]
    fn concurrent_final_release_wakes_all_prepare_waiters() {
        for _ in 0..if cfg!(miri) { 2 } else { 32 } {
            let state = HandlePrepareState::new();
            let active = state.try_enter().unwrap();
            state.close_admission();
            let ready = std::sync::Barrier::new(3);
            std::thread::scope(|scope| {
                let (sent, received) = std::sync::mpsc::channel();
                for _ in 0..2 {
                    let ready = &ready;
                    let state = &state;
                    let sent = sent.clone();
                    scope.spawn(move || {
                        ready.wait();
                        state.wait_for_idle();
                        sent.send(()).unwrap();
                    });
                }
                ready.wait();
                assert!(received.try_recv().is_err());
                drop(active);
                for _ in 0..2 {
                    received
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                }
            });
        }
    }
}
