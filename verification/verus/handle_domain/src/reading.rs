//! Shared native read ordering with a real initialized allocation permission.
//! Publication-to-owner and native atomic state sampling remain adapter premises.
use vstd::prelude::*;
use super::observations::{bindings, Observation, borrow_observed};
use super::heap_permission::HeapPermission;
use super::rotation::gate_permits::admission;
verus! {
pub struct Record { pub id: u64 }
pub struct Witness<'a> {
    pub domain: *const u8,
    pub permit: Tracked<&'a admission::permits>,
}
pub struct Reader<'a> {
    pub value: &'a Record,
    pub permit: Tracked<&'a admission::permits>,
}
pub enum ReadResult<'a> { FailStop, Stale, Live(Reader<'a>) }
pub struct Publication {
    pub domain: *const u8,
    pub pointer: *mut Record,
    pub present: bool,
    pub sampled_live: bool,
    pub gates: Ghost<Set<vstd::tokens::InstanceId>>,
}
// Publication metadata is not borrowed by the returned reader. The independent
// observation protects memory while publication ownership moves to retirement.
pub fn read<'a>(publication: &Publication, witness: Witness<'a>, id: u64,
    Tracked(instance): Tracked<&'a bindings::Instance<HeapPermission<Record>>>,
    Tracked(observation): Tracked<&'a Observation<'_, Record>>)
    -> (result: ReadResult<'a>)
    requires observation.node_id() == instance.id(), observation.memory().is_init(),
        observation.domain() == publication.gates@, observation.gate_id() == witness.permit@.instance_id(),
        publication.present ==> observation.memory().ptr() == publication.pointer,
        witness.domain.addr() == publication.domain.addr() ==> publication.gates@.contains(witness.permit@.instance_id()),
    ensures
        witness.domain.addr() != publication.domain.addr() ==> result is FailStop,
        witness.domain.addr() == publication.domain.addr() ==> !(result is FailStop),
        result is Live ==> publication.present && publication.sampled_live
            && result->Live_0.value.id == id
            && *result->Live_0.value == observation.memory().value()
            && result->Live_0.permit == witness.permit,
        witness.domain.addr() == publication.domain.addr() && publication.present && publication.sampled_live
            && observation.memory().value().id == id ==> result is Live,
{
    let instance_arg = Tracked(instance);
    let observation_arg = Tracked(observation);
    super::read_protocol::read_binding!(pointer, value;
        witness.domain.addr() == publication.domain.addr(),
        return ReadResult::FailStop,
        authorized_load(publication, &witness),
        return ReadResult::Stale,
        borrow_loaded(publication, pointer, &witness, instance_arg, observation_arg),
        value.id == id && publication.sampled_live,
        return ReadResult::Stale,
        ReadResult::Live(Reader { value, permit: witness.permit })
    )
}
pub fn authorized_load(publication: &Publication, witness: &Witness<'_>) -> (pointer: Option<*mut Record>)
    requires witness.domain.addr() == publication.domain.addr(),
        publication.gates@.contains(witness.permit@.instance_id()),
    ensures pointer == if publication.present { Some(publication.pointer) } else { None },
{ if publication.present { Some(publication.pointer) } else { None } }
pub fn borrow_loaded<'a>(publication: &Publication, pointer: *mut Record, witness: &Witness<'_>,
    Tracked(instance): Tracked<&'a bindings::Instance<HeapPermission<Record>>>,
    Tracked(observation): Tracked<&'a Observation<'_, Record>>) -> (value: &'a Record)
    requires observation.node_id() == instance.id(), observation.memory().is_init(),
        publication.present, pointer == observation.memory().ptr(),
        witness.domain.addr() == publication.domain.addr(),
        publication.gates@.contains(witness.permit@.instance_id()),
    ensures *value == observation.memory().value(),
{ borrow_observed(pointer as *const Record, Tracked(instance), Tracked(observation)) }
}

verus! {
/// Positive witness: retirement does not require ending an existing observation.
/// The same shared reader executes after publication ownership has moved.
pub fn read_during_retirement<'scope>(publication: &Publication, witness: Witness<'scope>, id: u64,
    Tracked(instance): Tracked<&bindings::Instance<HeapPermission<Record>>>,
    Tracked(published): Tracked<bindings::published<HeapPermission<Record>>>,
    Tracked(count): Tracked<&mut bindings::observing<HeapPermission<Record>>>)
    -> (result: (bool, Tracked<bindings::retired<HeapPermission<Record>>>))
    requires published.instance_id() == instance.id(), old(count).instance_id() == instance.id(),
        published.value().is_init(), published.value().ptr() == publication.pointer,
        publication.present, publication.sampled_live, published.value().value().id == id,
        publication.gates@ == instance.domain(), instance.domain().contains(witness.permit@.instance_id()),
        witness.domain.addr() == publication.domain.addr(),
    ensures result.0, result.1@.instance_id() == instance.id(), result.1@.value() == published.value(),
        final(count).instance_id() == instance.id(), final(count).value() == old(count).value(),
{
    let tracked observation = Observation::observe(instance, &published, count, witness.permit.get());
    let tracked (Tracked(retired), Tracked(history)) = instance.retire(published.value(), published);
    let result = read(publication, witness, id, Tracked(instance), Tracked(&observation));
    let accepted = match result { ReadResult::Live(_) => true, _ => false };
    proof { observation.end(instance, count); }
    (accepted, Tracked(retired))
}
}

verus! {
/// Shared read driven by an entry in the exhaustive conserving ledger.
pub fn read_covered<'a>(publication: &Publication, witness: Witness<'a>, id: u64,
    Tracked(instance): Tracked<&'a bindings::Instance<HeapPermission<Record>>>,
    Tracked(ledger): Tracked<&'a super::coverage::ObservationLedger<'_, Record>>, Ghost(key): Ghost<nat>)
    -> (result: ReadResult<'a>)
    requires ledger.inv(), ledger.contains(key), ledger.node_id() == instance.id(),
        ledger.memory(key).is_init(), ledger.domain() == publication.gates@,
        ledger.gate_id(key) == witness.permit@.instance_id(),
        publication.present ==> ledger.memory(key).ptr() == publication.pointer,
        witness.domain.addr() == publication.domain.addr() ==> publication.gates@.contains(witness.permit@.instance_id()),
    ensures result is Live ==> *result->Live_0.value == ledger.memory(key).value()
        && result->Live_0.value.id == id && result->Live_0.permit == witness.permit,
        witness.domain.addr() == publication.domain.addr() && publication.present && publication.sampled_live
            && ledger.memory(key).value().id == id ==> result is Live,
{
    let tracked observation = ledger.observation(key);
    read(publication, witness, id, Tracked(instance), Tracked(observation))
}
}
