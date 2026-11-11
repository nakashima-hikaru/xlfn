# Security review follow-up

This change addresses the reproducible defects and proportionate hardening from
the review of `1027e4f`. The review's severity labels are identifiers, not a claim
that all thirteen items are independently exploitable vulnerabilities.

| Review | Change |
| --- | --- |
| H-01 | Reject null/misaligned return pointers and enforce the return magic invariant in release builds. The ABI still requires a live, owned, exactly-once return pointer. An address registry cannot distinguish a stale pointer after address reuse. |
| H-02 | Bind the mapping descriptor to the process integrity SID and no-read/no-write-up policy; compare its label as well as owner/DACL. Check committed, read/write mapped memory before touching atomic state. |
| H-03 | Type-check synchronous and both asynchronous callbacks outside the unsafe call expression; deny implicit unsafe operations in generated wrappers. Raw parameter/local names use mixed-site hygiene. |
| H-04 | Inherit private directory ACLs into children, validate ACLs through the retained directory handle, and reject ambiguous Windows destination components before lock creation. |
| H-05 | Require successful main-branch push/manual builds using GitHub run metadata, bind verification to the API's commit SHA, validate CLI arguments, and reject extra artifact files and links. |
| H-06 | Audit all features and all target dependencies in the root and eight independent workspaces; reject yanked releases. Extend dependency updates to those workspaces, uv and NuGet. Document unavoidable transitive version splits. |
| M-01 | Keep topic bytes charged in every phase; only removal/termination releases that charge. Pending count remains independent. |
| M-02 | Collect COM arguments through raw pointers without exclusive slices; initialize outputs before shutdown admission, guard null AddRef, and read GUIDs unaligned. |
| M-03 | Randomize externally controlled topic/lifetime-key hashing, bound sequence deserialization before excess elements are decoded, and enforce a per-call scratch payload budget with fallible arena allocation. |
| M-04 | Require sorted, unique PE export names, validate section file bounds, bound symbol counts/text and snapshots. Bound configuration includes by count/physical identity and file size. Resolve bundle containment before opening files; reject Windows reparse sources and nonblocking-open Unix sources. |
| M-05 | Remove persisted checkout credentials, separate guide deployment and benchmark publication into jobs that execute no repository build code, pin installer tools and the .NET SDK, and lock NuGet restoration. |
| L-01 | Redact enum input/topic text in default diagnostics, bound/sanitize boundary messages, reject UNC/reparse log paths, and create private Unix log files/directories. Custom sinks still receive structured errors and control their own disclosure policy. |
| L-02 | Contain tracing panics after resource transitions, continue registration scavenging after individual errors, use volatile registration keys, recursively filter cfg_attr, and reject oversized export IDs during macro validation. |

## Boundaries retained

FFI/COM callers must supply valid live storage. These changes do not make arbitrary
native pointers safe, authenticate equal-authority processes, or remove every
filesystem race against a same-user process that can replace ancestor directories.
Windows private staging ACL checks now describe the exact opened directory.

Scratch accounting bounds requested payload across an invocation (64 MiB on
32-bit, 256 MiB on 64-bit); allocator chunk overhead is additional. Generic serde
element types can allocate internally: the collection visitor limits element
count, not arbitrary allocations performed inside `T::deserialize`.

The trusted CI download helper intentionally rejects PR artifacts and non-main
branches. CI can still build and test those artifacts without trusting them for
local Excel loading. Relaxed bundle paths continue to allow contained Unix
symlinks; Windows reparse points are rejected even with `strict-paths = false`.

Regression coverage includes retained active topic budgets, unsafe default
expressions and macro hygiene, nested cfg_attr filtering, pre-deserialization
limits, aliased includes, malformed PE names, extra artifact files/links, log
links, and Windows reserved mappings/integrity labels/shutdown outputs.
