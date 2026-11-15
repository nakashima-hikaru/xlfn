// module: runtime/open_txn
use super::{GenerationPublished, ModuleOpened, PublicationReady, ServicesPrepared};
use crate::generation::RemovalEpoch;

fn issue_twice(opened: ModuleOpened<'_, ()>) {
    let _ = opened.issue(RemovalEpoch::new(0));
    let _ = opened.issue(RemovalEpoch::new(0)); // error: E0382
}

fn ready_twice(prepared: ServicesPrepared<'_, ()>) {
    let _ = prepared.ready();
    let _ = prepared.ready(); // error: E0382
}

fn publish_twice(ready: PublicationReady<'_, ()>) {
    let _ = ready.publish();
    let _ = ready.publish(); // error: E0382
}

fn commit_twice(published: GenerationPublished<'_, ()>) {
    let _ = published.commit();
    let _ = published.commit(); // error: E0382
}
