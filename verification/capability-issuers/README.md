# Issuer-bound capabilities and validated values

This change removes APIs that permitted unrelated owner/capability pairs. It
does not replace synchronization, reclamation, stale generation or capacity
checks with a one-time type conversion.

- An async runtime calculation snapshot owns the runtime reference and the
  registry calculation pin from that runtime. Preflight is still advisory;
  the final reservation still performs executor reservation, calculation
  admission and capacity checks and can fail after an earlier preflight.
- Subscription server operations derive the runtime from the server handle.
  The handle contains non-owning raw pointers: its construction and use still
  require the existing stable-owner and external operation/COM lifetime
  contract. The runtime/server operation gates and deferred-operation guard
  remain necessary for teardown; the type alone does not prove that contract.
  Subscription IDs are not newly branded and retain catalog/generation checks.
- RTD buffer indices are the two-valued `BufferIndex`. Epoch parity yields one
  of the two slots, while absence remains `Option::None`. This is independent
  of subscription commit/admission lifecycle.
- Publication reservation receives either a raw new object or a call-scoped
  alias, not a pending object for an arbitrary registry. It makes and publishes
  a pending binding through its own runtime's store. Existing alias conversion
  still checks the source arena matches the destination, returning StaleHandle
  otherwise; rollback and topic ownership remain on the reservation's runtime.
- Registration preflight snapshots a validated signature after descriptor
  validation. Encoding that immutable value does not revalidate. Public raw
  signature construction and validation still reject invalid values. Validation
  order remains descriptor-wide before metadata preparation. A temporary SmallVec
  can allocate above 16 definitions; registration is not a pricing hot path.

These changes intentionally leave `CallbackResult`, mutable cross-process
state checks, and dynamic array-size validation unchanged. Negative typestate
fixtures prohibit mixing owners or forging the validated signature; focused
concurrency, Miri and standard feature tests complement those type checks.
