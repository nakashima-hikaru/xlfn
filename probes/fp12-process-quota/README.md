# FP12 process quota physical reload probe

This standalone DLL and Python runner test whether cooperating xlfn images use
the same process ledger after actual `FreeLibrary` / `LoadLibraryW` cycles. The
runner also alternates an independently copied DLL with the original path.
Every unload must make `GetModuleHandleW` report that image absent.

The probe deliberately forgets `NumericArrayBuilder` values. Their unpublished
FP12 allocations remain in the Windows process heap, together with their shared
quota reservations. It never registers an add-in, enters a UDF, pins its module,
or exposes an FP12 result to Excel. No boxed Rust owner or executable callback is
retained. This isolates process accounting and physical image lifetime from the
separate Excel consumption/reclamation contract.

Run from the repository root on native Windows, using a Python interpreter with
the same architecture as the DLL:

```powershell
cargo build --manifest-path probes/fp12-process-quota/Cargo.toml --release --target x86_64-pc-windows-msvc --offline
python probes/fp12-process-quota/run.py probes/fp12-process-quota/target/x86_64-pc-windows-msvc/release/fp12_process_quota_probe.dll --exhaust-quota --report fp12-process-quota-native.json
```

For 32-bit Windows, replace the target with `i686-pc-windows-msvc` and run 32-bit
Python. The runner recognizes undecorated and x86 stdcall-decorated exports.

Without `--exhaust-quota`, the default eight allocation/reload cycles retain
about 64 MiB of process heap memory. With that option, the worker retains almost
the provisional 512 MiB cap, then verifies rejection of the next allocation and
continued metrics availability after another physical reload. The runner always
starts a fresh subprocess; process exit releases all intentionally retained
memory and the one canonical mapping view/handle.

A passing JSON result establishes:

- the first metrics attachment retained exactly one mapping handle;
- repeated attachments, allocations, and physical reloads add no further handles;
- prior allocations and peak values survive both original-path and copied-image
  reloads, with consistent architecture-dependent private header charges;
- unpublished allocations remain distinct from published buffer counters;
- when requested, quota rejection leaves the ledger unchanged and the cap is
  never exceeded.

`--same-path-only` omits the independently copied image. `--reloads` changes the
number of initial successful cycles, and `--cells` changes the column height
(the default is 1,048,576 doubles). Large reload counts can exhaust the cap during
the initial phase and deliberately fail the run.

On other operating systems the runner emits `status: skipped`. Portable builds,
Python syntax checks, and cross-target checks do not establish native Windows
qualification. No native Windows result is included here. This probe does not
test Excel result consumption, published FP12 buffer reclamation, or performance.
