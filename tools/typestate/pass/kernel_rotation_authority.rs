// module: kernel/rotating_read_domain
use super::transition::PendingRotation;
use super::RotatingRetirementDomain;
fn consume(pending: PendingRotation<'_, 1>) -> usize {
    let (_authority, value) = pending.wait().run(|_| 42);
    value
}
fn queues(owner: &RotatingRetirementDomain<1, Vec<usize>>) {
    owner.register_retired(|_, mut registration| registration.append(1));
    for batch in owner.quiesce().unwrap().into_iter().flatten() {
        assert!(batch.belongs_to(owner));
        drop(batch.into_inner());
    }
    let closed = owner.seal_and_wait();
    drop(closed.take_queues());
    drop(closed.take_queues());
}
