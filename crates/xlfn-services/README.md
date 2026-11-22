# xlfn-services

Host-independent service backends for xlfn. Add-ins use the `xlfn` facade.

- The cache feature owns resident storage, single-flight evaluation, scoped
  borrowed lookup, and deferred destruction. The facade supplies the diagnostic
  reporter and retains the public cache types.
- RTD owns the reservation, connection and revocation state machine, candidate
  identity index, and topic quotas. The facade composes it with its catalog lock,
  Excel's transport keys, and host callbacks. Old tickets cannot restore an
  entry after revocation.
- Formula handle payloads have a unique, fixed-address owner. Binding counts
  and execution pins determine when it can be released. The host-typed arena
  owns the records and reports release progress to the shutdown coordinator.

A service may use the engine and kernel. It must not call back through the
facade or depend on the host adapter.
