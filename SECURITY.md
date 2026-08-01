# Security Policy

## Supported versions

xlfn is currently a pre-release project. Until the first stable release, security fixes are applied to the current `main` branch only.

| Version | Supported |
|---|---|
| `main` | Yes |
| Older commits and unpublished snapshots | No |

Downstream distributors are responsible for qualifying and maintaining the exact commit, Windows version, Excel version and bitness, native dependencies, and package digest they deploy.

## Reporting a vulnerability

Do not report suspected vulnerabilities in a public GitHub issue, discussion, or pull request.

Use GitHub's private vulnerability reporting flow:

1. Open the repository's **Security** tab.
2. Select **Advisories**.
3. Select **Report a vulnerability**.

Include, where available:

- the affected commit or version;
- Excel and Windows versions and bitness;
- the affected component or generated artifact;
- the security impact and required attacker capabilities;
- minimal reproduction steps or a proof of concept;
- known mitigations or workarounds.

Do not include production credentials, proprietary workbooks, customer data, signing keys, or other unrelated secrets. Provide a reduced reproducer instead.

If private vulnerability reporting is temporarily unavailable, open a public issue containing no vulnerability details and ask the maintainer to establish a private reporting channel.

## Response and disclosure

Reports will be reviewed on a best-effort basis. There is currently no guaranteed response-time SLA or bug-bounty program.

Please allow time for validation, remediation, regression testing, and coordinated disclosure before publishing technical details. Credit will be given when requested and appropriate.

## Scope

Security-relevant reports may include:

- memory-safety or ABI-boundary defects;
- panic or exception paths that cross the Excel ABI;
- use-after-free, double-free, or lifetime violations involving Excel return values, handles, async work, RTD, or shutdown;
- handle-token forgery or cross-workbook authorization bypasses;
- package-validation or architecture-check bypasses;
- unsafe DLL search-path behavior introduced by xlfn;
- defects that cause the runtime to report successful quiescence while executable work or resources remain active.

Third-party native libraries, application-specific UDF logic, workbook formulas, and deployment infrastructure are normally outside the project's scope. A report remains relevant when xlfn incorrectly validates, loads, isolates, or manages such a component.

## Deployment trust model

An XLL and every bundled native DLL are executable code. Users and distributors should:

- obtain artifacts from a controlled source;
- verify release provenance and package contents;
- sign artifacts where organizational policy requires it;
- protect installation directories from untrusted modification;
- audit third-party DLLs and their transitive imports;
- qualify the exact Excel architecture and deployment environment.

Build manifests and hashes detect changes relative to recorded inputs; they do not by themselves establish that an artifact is trustworthy at runtime.

The Lean 4 model under [`formal/`](formal) proves properties of an abstract shutdown protocol. It does not currently prove that every Rust implementation path refines that model and must not be treated as a complete security proof of the implementation.

For additional design details, see the [security guide](guide/src/security.md), [lifecycle guide](guide/src/lifecycle.md), and [testing guide](guide/src/testing.md).
