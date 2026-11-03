//! Stable snapshots and commit verification with an already-owned artifact.
//! Run with `cargo run -p xlfn-package --release --example snapshot_storage`.
use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

fn measure<T>(mut run: impl FnMut() -> T) -> Vec<u128> {
    let mut elapsed = Vec::new();
    for round in 0..6 {
        let started = Instant::now();
        let output = black_box(run());
        let nanos = started.elapsed().as_nanos();
        if round != 0 {
            elapsed.push(nanos);
        }
        drop(output);
    }
    elapsed
}

fn main() {
    let size = std::env::args()
        .nth(1)
        .map(|value| value.parse::<usize>().expect("size in MiB"))
        .unwrap_or(64)
        * 1024
        * 1024;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("payload.bin");
    std::fs::write(&path, vec![0x53; size]).unwrap();

    let snapshots = measure(|| xlfn_package::snapshot_file("benchmark", &path).unwrap());
    let expected = xlfn_package::snapshot_file("benchmark", &path).unwrap();
    let shared = BTreeMap::from([(PathBuf::from("payload.bin"), expected)]);
    let prepare = || {
        xlfn_package::PreparedDirectoryCommit::prepare_with_shared_artifacts(
            directory.path(),
            &["payload.bin"],
            &shared,
        )
        .unwrap()
    };
    let preparations = measure(prepare);
    let prepared = prepare();
    let verifications = measure(|| prepared.verify_source_contents().unwrap());
    println!(
        "{}",
        serde_json::json!({
            "bytes": size,
            "snapshot_file_ns": snapshots,
            "prepare_shared_ns": preparations,
            "verify_source_ns": verifications,
        })
    );
}
