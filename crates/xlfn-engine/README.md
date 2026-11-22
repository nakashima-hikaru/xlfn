# xlfn-engine

Host-independent lifecycle contracts for xlfn. Add-ins use the `xlfn` facade.

The crate owns the identity vocabulary, the classification of host mutations,
and the ordered stop record. Numerical identities are observations, not
capabilities. The coordinator owns and validates affine cleanup and publication
tickets; the engine's stop record cannot manufacture them.

| Boundary | Owns |
| --- | --- |
| Engine | Identity, host mutation outcomes, stop goal and completion order |
| Kernel | Atomic gates, notification tails, and allocation retirement |
| Services | Host-independent admission and accounting for generation resources |
| Host | Platform operations and actual native references |
| Facade | Public API, add-in bridge, and composition of the private owners |

A stop publishes logical closure first, releases the STA binding second, and
authorizes a successful return third. A quarantine may still record a native
action that already completed, but cannot commit success. Open attempt,
removal attempt, runtime generation, and RTD generations must not be conflated.
Physical module release requires separate host evidence.

The facade presently composes the lifecycle coordinator and service implementations
against these types. Moving a module across crates must not widen the
constructors of publisher/drain/quiescence witnesses or make a private Arc,
pointer, or OS handle into a public authority.
