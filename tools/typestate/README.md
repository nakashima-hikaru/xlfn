# Private lifecycle compile checks

Run `python3 tools/check_typestate.py` from any directory. The checker copies the
current tracked and nonignored working files into a temporary workspace, injects
each fixture as an internal module, and runs an offline, locked `cargo check`
with `async,rtd,handles,cache`. The default target is the rustc host. Windows COM
fixtures run on Windows and are explicitly reported as skipped on other hosts.
`--target x86_64-pc-windows-msvc` includes them in a cross-target check when the
Windows native dependency toolchain is configured; `--extra-features` can enable
additional dependency features such as `blake3/pure` for compile-only checks.
CI runs the suite on both Linux and Windows. These are compile checks, not live
Excel execution tests. The original source tree is never edited. An optional
`--report PATH` retains toolchain, input hash, targets, commands, build settings,
skipped fixtures, and diagnostics.

Positive callers run first. Negative callers must produce the exact error code
at each line marked `// error: E....`; unrelated errors fail the check. Internal
injection verifies the actual method and field restrictions without publishing
test APIs or accepting an ancestor-module privacy error as typestate evidence.

Open transactions carry mandatory service inputs after initialization. Fixtures
reject missing or repeated initialization inputs, commit before initialization
or lifecycle installation, and committing twice. Async response delivery consumes
the pending handle authority; its fixtures reject a second delivery attempt.
Executor close retains the runtime and executor through publication-reader and
task drain. Only the drained stage exposes executor shutdown; abandoning any
stage restores the sealed executor after releasing the registry close claim.

Removal owners retain their issuing coordinator and observer from acquisition,
so abandoning either purpose returns its module authority to that issuer. The
private claim assembly cannot combine a claim with another runtime's dependencies;
neither acquisition nor a later `drain` can change its purpose. Removal
starts by consuming that owner into the teardown transaction. The transaction
must close module callbacks, prove host detachment, and quiesce the add-in before
sealing generation services. The seal transition first detaches
handle topics while the handle slot is still published, preserving separate
topic-stop and handle-seal failures. Fixtures reject skipping these stages,
reusing the initial owner, and reassembling an unrelated owner and drained stage.
Interrupted teardown still returns its module authority before releasing the
cleanup claim.
A lifecycle match retains `AvailableClose` or `ClaimedClose` rather than rebuilding
the broad resource enum. Only available resources can claim a removal owner;
already drained or retained authority cannot start a new close. Cleanup-ready
authority alone exposes `finish`, keeping retention outside its receiver type.
An undrained module becomes a terminal retained owner without a drain or retry
operation. A handle-registry seal carries its own registry reference, so callers
cannot apply it to another store or pair a service with an unrelated seal.

Server termination fixtures also require cancellation before finish, reject
reusing cancellation or completion authority, and prevent passing a cancellation
result from another termination. Coordinator fixtures check that only its affine
owner can publish one terminal result and that shared state remains private.
Publish termination carries its exact core through a consumed wait and finish;
fixtures reject early finish, a substituted core, repeated wait/finish, and access
to the raw finish method. COM ServerStart commits only after the consuming
callback-publication operation; failures before and after publication retain
their distinct retry policies.

Typed handle fixtures also reject payload retagging and subtype conversions
that would change the exact `TypeId`. Input fixtures check static materializer
selection and consuming prepared values once. Teardown fixtures require recovery
authority while permitting borrowed stages, and cache fixtures enforce its
thread-safety bounds.
Single-flight cache completion consumes its leader guard and publishes before
unregistering. The guard has no separate unregister operation; abandonment still
detaches before waking followers to retry.

Endpoint memoization fixtures require the endpoint and retained cache pointer
to have the same key, value, and marker types, including function subtyping.
Unload-policy fixtures allow both policies for an opted-in add-in, require the
physical-unload contract for the physical policy, and reject policy retagging
or unchecked implementations of the sealed policy trait.

Source subscription owners can transfer across threads but cannot be shared
concurrently or consumed twice, even when the concrete subscription is not
`Sync`. Registration into a closed subscription enum requires an audited
`IntoRtdSubscription<E>` conversion, generated by `rtd_subscriptions!` for direct
enum construction; a safe `From` implementation alone is rejected.
Static source catalogs return handles in their declared tuple order and reject
incremental source registration or changing a handle's concrete source type.

RTD senders retain their FIFO or Latest policy in their type and cannot be
retagged. Validated pending values and batches may still be retried on a sender
with another policy, without converting again or changing their input type.
Worker payload variants own the producer, running thread, or completed result
required by each state. Channel attachment owns its subscription permit until
a consuming transfer moves it to the multiplexed subscription. Windows worker
joining retains its thread identity so a concurrent self-join remains rejected.

Registration mutation evidence retains its concrete operation type. Its callback
ID, decoder, output type and decode-failure policy come from that same operation;
fixtures reject substituting another operation even when both return `()`, or
applying mutation policy to untyped callback evidence.

Return contexts retain their input mode and concrete RTD observer catalog.
Plain contexts cannot publish handles, formula contexts require their publisher,
and scalar/array payload types remain concrete through the value ABI boundary.
Channel senders also retain the actual FIFO/latest storage policy, preventing a
sender from being constructed with the other policy's channel allocation.

COM resource disposal requires a drained capability that owns a reference to its
exact server. Only a worker-joined capability can release ACTIVE_SERVER; deferred
workers can dispose backend/callback resources without that authority. Fixtures
reject disposal through an undrained server/barrier, active-reference release
before join, substitution of another server, and construction of subscription
attachments from unrelated runtime/server handles.

Async execution leases are issued through process-lifetime async admission.
The admission retains its exact publication owner; callers supply neither a
second runtime nor a generation pointer, and short-lived ordinary call guards
cannot launch owned execution. Closing rejects fresh leases while existing
leases still block generation reclamation.

Reclaim owns diagnostics shutdown and RTD waiting instead of accepting detached
certificates. Module logical quiescence requires the complete non-module proof
bundle and exact module epoch; exports alone cannot publish it. Canonical
retirement and completion consume private, purpose-specific authorizations and
revalidate their coordinator, attempt, generation and epoch under the mutation
lock. Failed resource certification returns the progressed module authority.

COM gate phase/counters are read-only outside the gate module. Terminal worker
join seals future starts, including an idle worker and a rolled-back reservation;
start reservations are thread-affine and reject waiting on themselves.

Opening acquisition retains its issuer and recovery responsibility immediately
after the canonical start mutation, across return-admission reopening and module
opening. There is no detached start descriptor or public raw-pair constructor.
Rollback conversion retains its issuer and unfinished Drop recovery; neither
failure recovery nor rollback execution accepts a separate runtime. Interrupted
rollback returns its host journal and keeps untrusted lifecycle state quarantined.

Async launch uses the registry and return protocol retained by its admission
rather than accepting another runtime alongside the guard. ServerStart likewise
reserves and publishes through one retained target; publication cannot select a
different callback store, and the start atomic is read-only outside its module.

Deferred termination transfers one affine ticket from its reservation into a
server-owned worker payload. The ticket retains its barrier and attempt, and is
consumed once to obtain the completion owner. A rolled-back ticket cannot finish
a later attempt by the same thread. The startup reservation keeps the gate lock
until spawn succeeds, and an uncommitted Drop reopens admission. Same-thread wait
before commit is rejected, while another worker can wait for the commit. These
restrictions do not limit repeated terminal drain observations or resource reapers.

Deferred worker startup is one consuming transaction. Its private fields keep the
gate reservation, worker-start reservation, completion ticket and COM reference
together. Only `spawn(self)` can use the fixed worker entrypoint, register its
handle and commit the gate. Sibling callers cannot prepare a detached ticket or
commit a termination without its worker. Failed or abandoned startup restores
the worker before reopening the gate.

Open initialization obtains host intent, generation and calculation-event
registration/residency from its transaction's issuer. The internal entrypoint
has no independent runtime parameter. Production-only feature combinations must
also compile with warnings denied; all-features tests alone do not detect dead
APIs whose only callers are tests or internal instrumentation.
