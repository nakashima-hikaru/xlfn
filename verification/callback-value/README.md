# Callback value read and cleanup capabilities

`CallbackResult` separates readable values from terminal-result cleanup-only
values. A private `CallbackOwner` retains the raw XLOPER12, session, module
permit, and dynamic release state. Both wrappers own it; only
`ExcelCallbackValue` exposes the read accessors. Explicit release consumes the
wrapper. `XlValueRef::from_raw` still validates potentially malformed Excel
data, so read conversion remains fallible.

Nonterminal error statuses can still produce readable memory, but operation
success and memory readability are different facts: `HostInvocation` retains
its status-based decoder suppression and existing error priority.

Before calling `xlFree`, the owner changes its state to Indeterminate. A failed
or panicking cleanup cannot be retried by Drop. Terminal results retain cleanup
authority; a later nested Abort or Uncalced does not revoke an already issued
owner. The module permit is retained throughout the host cleanup call, including
unwinding. Forgetting an owner cannot manufacture a successful cleanup receipt:
it also forgets the permit, so closing the gate remains blocked.

`raw_pointer()` is intentionally not a lifetime-carrying reference. Callers
must keep the owner live and stationary throughout the synchronous host call and
must not retain the pointer. The existing nested caller/sheet-name/sheet-ID
paths meet that condition; the wrapper cannot guarantee arbitrary future raw
pointer usage.

The cleanup-only/no-access and release-then-read compile-fail fixtures ensure
access restrictions do not merely fail at runtime. Tests also cover failed
nonterminal classification, one-attempt cleanup, terminal status propagation,
panic through the scripted HostInvocation decoder, panic during fake Rust-ABI
cleanup, nested callback termination, module close,
and deliberately forgotten values. GhostCell and new generative brands are not
needed here. No speedup is claimed: the goal is eliminating an invalid API
state while leaving cleanup's dynamic state machine intact.

The fake Rust-ABI cleanup panic tests unwinding of the owner, not a promise
that an actual Excel extern ABI may safely unwind.
