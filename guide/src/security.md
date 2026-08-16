# Security model

xlfn reduces unsafe surface area; it does not turn Excel, a workbook, an external component, or a service into a security boundary. This chapter defines what must be trusted and what the framework validates.

## Trust boundaries

### Trusted code and package

The add-in binary and every in-process sidecar execute inside Excel with the user's privileges and must be treated as trusted executable code. Loading an in-process component may run initialization before application-level protocol or ABI checks. Out-of-process and remote adapters have different isolation properties but still require authentication, authorization, and bounded resource use.

Protect the final installation directory with appropriate ACLs and code signing. The `build-manifest.json` hashes support audit and reproducibility; they are not a runtime pre-execution integrity mechanism. Runtime loading, authentication, and trust decisions belong to the application adapter and deployment policy; xlfn does not perform them.

### Untrusted workbook input

Cell values, strings, arrays, references, handle-token text, and RTD topic arguments can be malformed or adversarial. The generated boundary validates types, lengths, finite numbers, shapes, pointer structure, and memory limits before ordinary Rust code receives them.

Application code must still validate domain limits. A structurally valid array can request an expensive model calculation, and a valid string can be an unsafe filename, URL, query, or command if passed on without policy.

Never derive a sidecar path, executable path, shell command, SQL statement, credential scope, or authorization decision directly from workbook text.

### Excel callbacks

Raw references and Excel callback operations are capability-limited to macro-sheet/main-thread contexts. Their results are still host-provided data and must be converted before being retained.

## Panic and memory safety

Framework ABI boundaries catch Rust panics so unwinding does not cross Excel or COM ABIs. Unsafe pointer handling and return ownership are centralized and validated.

This containment has limits:

- a panic can leave an external or application transaction partially complete;
- an incorrect in-process ABI declaration can corrupt memory before Rust can catch anything;
- a foreign exception crossing an ABI boundary is undefined behavior unless contained by the application adapter;
- process aborts, stack corruption, and access violations are not Rust panics;
- a destructor that blocks indefinitely can prevent safe unload.

Set `panic = "unwind"` for release profiles that depend on containment. Do not replace domain errors with panics.

## Handle tokens

Handle tokens are authenticated, session-scoped capabilities. The runtime
verifies a keyed MAC, session/generation data, and slot generation first, then
checks the requested Rust type against the canonical record. Type identity is
not duplicated in the wire token.

They prevent accidental or textual fabrication of a valid live handle. They do not provide:

- workbook-level authorization;
- confidentiality;
- cross-process persistence;
- user identity;
- protection after an attacker already executes code in the Excel process.

Do not expose a token as a durable object ID or accept it over an external service boundary.

## External components and sidecar files

An application adapter may use `OpenContext::module_directory()` to locate installed sidecar files. xlfn does not load those files. `cargo xlfn` checks the independently declared bundle architecture and packaged DLL import closure. `strict-paths` defaults to true; configured bundle paths reject symlink, junction, and reparse-point traversal observed during validation unless a manifest explicitly opts into the relaxed policy. This path-based check is not a defense against concurrent replacement of a checked component, so protect the manifest tree from mutation during packaging when that threat is in scope.

Still apply these controls:

- install in a non-user-replaceable directory appropriate to your threat model;
- sign all executable components;
- avoid ambient PATH or current-directory dependency resolution;
- package non-system transitive dependencies explicitly;
- keep `external-imports` narrowly reviewed;
- validate sidecar and adapter-dependency provenance and hashes before building;
- test on a clean environment without developer-only DLLs.

## RTD COM registration

RTD uses temporary per-user COM registration. Ownership markers prevent broad cleanup: only entries matching the same owner, schema, module path, and CLSID are scavenged after an abnormal exit.

Do not grant the add-in broad registry write privileges. Validate the exact keys in deployment tests, and ensure that uninstall logic removes only its own entries.

RTD source data is application data. Authenticate and authorize external feeds in the source adapter; the RTD transport itself does not do so.

## Diagnostics and privacy

Diagnostics can contain function names, argument labels, paths, external status codes, and exception context. They should not contain workbook cells, customer data, credentials, access tokens, connection strings, or full external buffers unless an explicit protected support mode permits it.

The file sink writes under the user's local application-data area and rotates bounded files. Apply the organization's retention, support-bundle, and access policy. Monitor dropped diagnostic events so an error storm is not mistaken for silence.

## Denial of service

The framework bounds several resources, including array sizes, handle count, diagnostic delivery, async execution, and RTD structures. Application-owned adapter queues are not framework-managed and must be bounded separately. RTD has explicit limits for topic parts and bytes (per topic and in aggregate), pending preparations, active streams, queued updates, and live source identities; the standard values are documented in the RTD chapter. Application code must also bound:

- algorithmic complexity and model iterations;
- cache weight and key cardinality;
- external request concurrency;
- external context/object counts;
- RTD publication rate;
- retry and backoff loops;
- string-to-path or string-to-query expansion;
- shutdown duration.

A thread-safe attribute can let Excel invoke many calls concurrently. Pair it with measured capacity and admission control rather than relying on Excel to protect a backend.

## Supply-chain and release controls

For a production release:

1. use a locked dependency graph and review dependency changes;
2. build from a controlled clean environment;
3. verify downloaded SDKs, sidecars, and adapter dependencies by digest and publisher;
4. retain source commit, toolchain, target, and feature evidence;
5. run static analysis, tests, ABI probes, and package inspection;
6. sign the final bytes and verify the signatures;
7. scan and install the final package in a clean environment;
8. archive the package and evidence needed for incident response.

Security claims should name the tested artifact and environment. "Written in Rust" is not a substitute for an audited adapter and deployment boundary.
