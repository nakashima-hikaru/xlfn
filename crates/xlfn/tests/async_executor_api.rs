#![cfg(feature = "async")]

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn::prelude::*;
use xlfn::{AsyncExecutor, AsyncTask, RuntimeConfig};

struct ApplicationExecutor {
    drops: Arc<AtomicUsize>,
}

impl Drop for ApplicationExecutor {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::AcqRel);
    }
}

impl AsyncExecutor for ApplicationExecutor {
    type Reservation = ();

    fn start(&self) -> XllResult<()> {
        Ok(())
    }

    fn reserve(&self) -> XllResult<Self::Reservation> {
        Ok(())
    }

    fn submit<F>(&self, (): Self::Reservation, task: AsyncTask<F>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // This test executor accepts ownership and then destroys the task.
        // Lifecycle and completion accounting remain inside the opaque task.
        drop(task);
    }

    fn shutdown(&self) -> XllResult<()> {
        Ok(())
    }
}

struct ApplicationAddin;

impl Addin for ApplicationAddin {
    type SharedState = ();
    type LifecycleState = String;
    type Error = XllError;
    type Layers = ();
    type AsyncExecutor = ApplicationExecutor;

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(())
            .with_async_executor(ApplicationExecutor {
                drops: Arc::new(AtomicUsize::new(0)),
            })
            .with_lifecycle(String::new())
            .with_layers(()))
    }
}

#[test]
fn opened_owns_external_executor_across_builder_transformations() {
    let drops = Arc::new(AtomicUsize::new(0));
    let opened: OpenResult<ApplicationAddin> = Ok(Opened::new(())
        .with_async_executor(ApplicationExecutor {
            drops: Arc::clone(&drops),
        })
        .with_lifecycle(String::new())
        .with_layers(())
        .with_runtime_config(RuntimeConfig::new()));
    assert_eq!(drops.load(Ordering::Acquire), 0);
    drop(opened);
    assert_eq!(drops.load(Ordering::Acquire), 1);
}

#[test]
fn opaque_async_task_implements_send_future_without_builtin_executor() {
    fn require_task<T: std::future::Future<Output = ()> + Send + 'static>() {}
    require_task::<AsyncTask<std::future::Ready<()>>>();
}
