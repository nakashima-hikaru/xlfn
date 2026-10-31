// module: kernel/rotating_read_domain
use super::transition::PendingRotation;
use super::RotatingRetirementDomain;
fn consume(pending: PendingRotation<'_, 1>) -> usize {
    let (_authority, value) = pending.wait().run(|_| 42);
    value
}
fn queues(owner: &RotatingRetirementDomain<1, usize>) {
    owner.quiesce(|retired| retired.take(|mut queue| std::mem::take(&mut *queue))).unwrap();
    let closed = owner.seal_and_wait();
    closed.take_queues(|_, _| ());
    closed.take_queues(|_, _| ());
}
