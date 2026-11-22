# xlfn-engine

Host-independent execution control for xlfn. Add-ins use the `xlfn` facade.

The engine owns open/removal identity allocation, the quarantine claim state
machine, portable outcomes and error contracts, and ordered stop progress.
Numeric identities identify observations; they do not by themselves confer a
right to publish, retire, or reclaim a generation.

| Boundary | Owns |
| --- | --- |
| Engine | Lifecycle sequencing, claim arbitration, host mutation outcomes, and stop progress |
| Kernel | Atomic gates, notification tails, and allocation retirement |
| Services | Cache, handle payload ownership, and subscription state and accounting |
| Host | Native waits, physical resource ownership, and the FP12 process ledger |
| Facade | Public add-in surface, canonical host-typed payloads, and composition of the owners |

The facade drives payload-dependent transitions and holds the canonical lifecycle
lock. The engine arbitrates quarantine under that same lock. A matching claim
confers the responsibility to finish, defer, or retain the native payload; a
stale claim may not operate on a new module epoch. Logical close precedes
release of the STA binding, which precedes successful return. Physical module
release requires separate host evidence.

In a crate migration, never widen the constructors of publication, drain, or
quiescence witnesses just to move a caller.
