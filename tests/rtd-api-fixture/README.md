# RTD capability consumer fixture

This standalone crate renames the dependency to `framework`. Its consumer
features deliberately differ from the dependency's names:

- `objects` enables `xlfn/handles`;
- `streaming` enables `xlfn/rtd`;
- neither consumer feature is named `rtd`.

The normal library declares `#[excel_addin]` on its `impl Addin` and supplies
no RTD associated types. It must compile with every combination of these flags.
The four extra source cases verify explicit RTD configuration, public RTD
exports, `OpenContext::rtd`, and erased source registration respectively.
The erased case retains two sources with capturing closures and different
value/subscription types in non-generic state handles, with only the common
boxed owner selected in the add-in. They compile only when `streaming`
is enabled. The disabled configurations must fail with the intended framework
feature error, privacy/import errors, or missing capability method, rather than
an unrelated Addin implementation error.

Run all twenty checks from the repository root:

```sh
python3 -B tests/rtd-api-fixture/check.py
```

The checker uses temporary source projects, locked offline dependency resolution,
a separate target directory and `CARGO_INCREMENTAL=0`. It does not edit the
fixture's source while checking failure cases. Override `--target-dir` or
`--report-dir` to choose other output directories.

Generated reports and logs are written under `target/rtd-api-fixture/reports/`
and are not committed. These checks compile external consumers; they do not
execute Excel or Windows COM.
