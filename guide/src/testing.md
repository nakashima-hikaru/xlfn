# Test your add-in

Start with fast Rust tests for application behavior, then check the Windows
artifact. Run [the Excel checklist](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/EXCEL_TESTING.md) before distribution.
This chapter is for add-in authors; changes to xlfn itself use
[CONTRIBUTING.md](https://github.com/nakashima-hikaru/xlfn/blob/main/CONTRIBUTING.md).

## Run Rust tests during development

From the add-in project directory, run:

```console
cargo test --locked
```

Use the same Cargo features that your add-in distributes. If the project has
feature-specific behavior, test each supported combination separately rather
than assuming `--all-features` represents a production configuration. Repeat
performance-sensitive checks with the release profile.

Keep application calculations independent of Excel callbacks where possible.
They can then be tested on a normal Rust test host:

```rust
fn scaled_total(values: &[f64], scale: f64) -> f64 {
    values.iter().sum::<f64>() * scale
}

#[cfg(test)]
mod tests {
    use super::scaled_total;

    #[test]
    fn scales_the_complete_input() {
        assert_eq!(scaled_total(&[1.0, 2.0, 3.0], 2.0), 12.0);
    }

    #[test]
    fn empty_input_has_zero_total() {
        assert_eq!(scaled_total(&[], 2.0), 0.0);
    }
}
```

Call the tested function from a small `#[excel_function]` wrapper. Test the
application's actual edge cases: empty inputs, domain bounds, overflow,
configuration changes, and failures from external services. A pure Rust test
does not exercise Excel's argument conversion, registration, or callback ABI;
include those behaviors in the Excel checklist.

## Test state and external services

Build application state with test implementations of network or resource
clients, then exercise success, timeout, cancellation, overload, and cleanup.
Keep the doubles at your application's boundary; tests should not need private
xlfn runtime types.

For stateful features, test these application behaviors before opening Excel:

| Facility | Useful Rust tests |
| --- | --- |
| Handles | Object methods, explicit revision dependencies, safe and idempotent resource disposal |
| Async | Owned input processing, cooperative cancellation, bounded blocking adapters |
| RTD | Topic validation, value conversion, producer exit when the sender closes |
| Caches | Key identity, application weight policy, invalidation when external data changes |

Use deterministic signals or channels to coordinate concurrency tests. Avoid
using a sleep as proof that a worker has started or stopped. Verify that
application-owned workers are joined and that shutdown leaves no code running
against state that has been disposed.

## Check the Windows artifact

On the supported Windows build host, validate each architecture you distribute:

```powershell
cargo xlfn check --target x86_64-pc-windows-msvc --locked
cargo xlfn check --target i686-pc-windows-msvc --locked
cargo xlfn package --all --locked
```

Add the same feature and profile options used for your release. Omit an
architecture only when your support policy excludes it. See
[Build, validate, and load](build-validation.md) and the
[`cargo xlfn` reference](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/cli-reference.md) for prerequisites and options.

Inspect the final package for:

- required lifecycle, COM, async, and UDF exports, including x86 decoration;
- the expected PE machine type;
- a complete packaged import closure;
- unique case-insensitive bundle basenames;
- staged bytes matching the manifest records.

Run tests against the dependency versions that the add-in will ship, including
registry versions when validating an upgrade. Artifact checks do not start
Excel. Continue with [Test in Excel before release](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/EXCEL_TESTING.md) to test
worksheet behavior, lifecycle, and the actual installation environment.

## Record what passed

Keep Rust results, artifact checks, and Excel execution evidence separate.
Record the source commit, toolchain, target, feature selection, and final
package digest. Publish only the Excel environments actually tested; the
[evidence template](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/EXCEL_TESTING.md#choose-environments-and-record-evidence)
helps make results reproducible.
