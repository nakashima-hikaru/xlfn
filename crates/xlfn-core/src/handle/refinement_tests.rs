use super::*;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

struct TraceValue;

impl ExcelHandleObject for TraceValue {}

fn create_value() -> XllResult<TraceValue> {
    Ok(TraceValue)
}

fn write_trace(runtime: &HandleRuntime, name: &str) {
    let Some(directory) = std::env::var_os("XLFN_HANDLE_REFINEMENT_TRACE_DIR") else {
        return;
    };
    let directory = std::path::Path::new(&directory);
    std::fs::create_dir_all(directory).expect("create H4 trace directory");
    std::fs::write(directory.join(name), runtime.refinement_trace_json())
        .expect("write H4 handle refinement trace");
}

fn cold_success_trace() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("h4-cold-success");
    runtime
        .prepare_observed(key, create_value, |_, _| Ok(()))
        .expect("cold publication succeeds");
    runtime.close().expect("cold success closes");
    write_trace(&runtime, "rust-handle-cold-success.json");
}

fn cold_failure_trace() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("h4-cold-failure");
    let result = runtime.prepare_observed(key, create_value, |_, _| Err(XllError::Panic));
    assert!(matches!(result, Err(XllError::Panic)));
    runtime.close().expect("cold failure closes");
    write_trace(&runtime, "rust-handle-cold-observe-failure.json");
}

fn warm_disconnect_trace() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("h4-warm-disconnect");
    runtime
        .prepare_observed(key, create_value, |_, _| Ok(()))
        .expect("initial publication succeeds");
    let rtd_key = key.format_rtd_key();
    runtime
        .connect(1, 41, &rtd_key)
        .expect("Excel connection commits");

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_runtime = Arc::clone(&runtime);
    let worker = thread::spawn(move || {
        worker_runtime.prepare_observed(key, create_value, |_, _| {
            entered_tx.send(()).expect("warm reader entered");
            release_rx.recv().expect("warm reader release");
            Ok(())
        })
    });
    entered_rx.recv().expect("warm reader did not enter");
    runtime.disconnect(1, 41);
    release_tx.send(()).expect("release warm reader");
    assert!(matches!(
        worker.join().expect("warm reader panicked"),
        Err(XllError::StaleHandle)
    ));
    runtime.close().expect("disconnect trace closes");
    write_trace(&runtime, "rust-handle-warm-disconnect.json");
}

fn warm_generation_termination_trace() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("h4-warm-termination");
    runtime
        .prepare_observed(key, create_value, |_, _| Ok(()))
        .expect("initial publication succeeds");
    let rtd_key = key.format_rtd_key();
    runtime
        .claim_server(&rtd_key, 1)
        .expect("server claims topic");

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_runtime = Arc::clone(&runtime);
    let worker = thread::spawn(move || {
        worker_runtime.prepare_observed(key, create_value, |_, _| {
            entered_tx.send(()).expect("warm reader entered");
            release_rx.recv().expect("warm reader release");
            Ok(())
        })
    });
    entered_rx.recv().expect("warm reader did not enter");
    runtime.terminate_topics(1);
    release_tx.send(()).expect("release warm reader");
    assert!(matches!(
        worker.join().expect("warm reader panicked"),
        Err(XllError::StaleHandle)
    ));
    runtime.close().expect("termination trace closes");
    write_trace(&runtime, "rust-handle-warm-generation-termination.json");
}

fn same_key_aba_trace() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("h4-same-key-aba");
    runtime
        .prepare_observed(key, create_value, |_, _| Ok(()))
        .expect("initial publication succeeds");
    let rtd_key = key.format_rtd_key();
    runtime
        .connect(1, 77, &rtd_key)
        .expect("Excel connection commits");

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_runtime = Arc::clone(&runtime);
    let worker = thread::spawn(move || {
        worker_runtime.prepare_observed(key, create_value, |_, _| {
            entered_tx.send(()).expect("old warm reader entered");
            release_rx.recv().expect("old warm reader release");
            Ok(())
        })
    });
    entered_rx.recv().expect("old warm reader did not enter");
    runtime.disconnect(1, 77);

    let (_, created) = runtime
        .prepare_observed(key, create_value, |_, _| Ok(()))
        .expect("replacement publication succeeds");
    assert!(created, "same-key replacement must be a cold publication");

    release_tx.send(()).expect("release old warm reader");
    assert!(matches!(
        worker.join().expect("old warm reader panicked"),
        Err(XllError::StaleHandle)
    ));
    runtime.close().expect("ABA trace closes");
    write_trace(&runtime, "rust-handle-same-key-aba.json");
}

fn warm_close_trace() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("h4-warm-close");
    runtime
        .prepare_observed(key, create_value, |_, _| Ok(()))
        .expect("initial publication succeeds");

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (observed_tx, observed_rx) = mpsc::channel();
    let worker_runtime = Arc::clone(&runtime);
    let worker = thread::spawn(move || {
        worker_runtime.prepare_observed(key, create_value, |_, _| {
            entered_tx.send(()).expect("warm reader entered");
            release_rx.recv().expect("warm reader release");
            observed_tx.send(()).expect("warm reader observed");
            Ok(())
        })
    });
    entered_rx.recv().expect("warm reader did not enter");

    let (seal_entered_tx, seal_entered_rx) = mpsc::channel();
    let (seal_release_tx, seal_release_rx) = mpsc::channel();
    runtime
        .refinement
        .set_before_seal_hook(seal_entered_tx, seal_release_rx);
    let close_runtime = Arc::clone(&runtime);
    let close_thread = thread::spawn(move || close_runtime.close());
    seal_entered_rx
        .recv()
        .expect("close reached the Closing/SealForClose boundary");
    release_tx.send(()).expect("release warm reader");
    observed_rx
        .recv()
        .expect("warm reader returned from observation");
    seal_release_tx
        .send(())
        .expect("release close seal test hook");
    assert!(matches!(
        worker.join().expect("warm reader panicked"),
        Err(XllError::Closing)
    ));
    close_thread
        .join()
        .expect("close thread panicked")
        .expect("warm close succeeds");
    write_trace(&runtime, "rust-handle-warm-close.json");
}

#[test]
fn rust_handle_refinement_traces_are_production_path_replays() {
    cold_success_trace();
    cold_failure_trace();
    warm_disconnect_trace();
    warm_generation_termination_trace();
    same_key_aba_trace();
    warm_close_trace();
}

#[test]
fn refinement_trace_uses_input_fingerprint_wire_name() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("h4-wire-schema");
    runtime
        .prepare_observed(key, create_value, |_, _| Ok(()))
        .expect("wire-schema publication succeeds");

    let trace = runtime.refinement_trace_json();
    assert!(trace.contains("\"inputFingerprint\""));
    assert!(!trace.contains("\"argumentDigest\""));
    assert!(trace.contains("\"schema_version\": 3"));
}
