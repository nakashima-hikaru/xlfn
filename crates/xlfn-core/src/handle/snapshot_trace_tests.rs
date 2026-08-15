use super::*;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

struct TestObject;

impl ExcelHandleObject for TestObject {}

fn create_test_object() -> XllResult<Arc<TestObject>> {
    Ok(Arc::new(TestObject))
}

fn write_snapshot_trace(runtime: &HandleRuntime, name: &str) {
    let Some(directory) = std::env::var_os("XLFN_HANDLE_SNAPSHOT_REFINEMENT_TRACE_DIR") else {
        return;
    };
    let directory = std::path::Path::new(&directory);
    std::fs::create_dir_all(directory).expect("create Snapshot trace directory");
    std::fs::write(
        directory.join(name),
        runtime.snapshot_refinement_trace_json("returned_success"),
    )
    .expect("write Snapshot refinement trace");
}

fn fast_lookup_success_trace() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("snapshot-fast-success");
    runtime
        .prepare_observed(key, create_test_object, |_, _| Ok(()))
        .expect("initial publication succeeds");
    let rtd_key = key.format_rtd_key();
    runtime
        .connect(1, 10, &rtd_key)
        .expect("Excel connection commits");

    let token = {
        let topic = runtime
            .published
            .load(&key)
            .get(&key)
            .expect("topic is published")
            .clone();
        topic.token.clone()
    };

    let handle = runtime
        .lookup_handle::<TestObject>(&token)
        .expect("fast lookup succeeds");
    assert_eq!(runtime.active_leases(), 1);

    drop(handle);
    assert_eq!(runtime.active_leases(), 0);

    runtime.close().expect("runtime close succeeds");
    write_snapshot_trace(&runtime, "rust-snapshot-fast-success.json");
}

fn handle_clone_surviving_close_trace() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("snapshot-clone-close");
    runtime
        .prepare_observed(key, create_test_object, |_, _| Ok(()))
        .expect("initial publication succeeds");
    let rtd_key = key.format_rtd_key();
    runtime
        .connect(1, 20, &rtd_key)
        .expect("Excel connection commits");

    let token = {
        let topic = runtime
            .published
            .load(&key)
            .get(&key)
            .expect("topic is published")
            .clone();
        topic.token.clone()
    };

    let handle1 = runtime
        .lookup_handle::<TestObject>(&token)
        .expect("lookup succeeds");
    let handle2 = handle1.clone();
    drop(handle1); // Non-final drop: stutter

    let (close_started_tx, close_started_rx) = mpsc::channel();
    *runtime.leases.before_idle_wait_hook.lock() = Some(Arc::new(move || {
        let _ = close_started_tx.send(());
    }));

    let close_runtime = Arc::clone(&runtime);
    let close_thread = thread::spawn(move || close_runtime.close());

    close_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("close reached idle wait while handle clone was held");

    // Lineage remains active during close wait
    assert_eq!(runtime.active_leases(), 1);

    // Final drop: emits CompleteFastLookup and allows close to complete
    drop(handle2);

    close_thread
        .join()
        .expect("close thread panicked")
        .expect("close succeeded after handle drop");

    assert_eq!(runtime.active_leases(), 0);
    write_snapshot_trace(&runtime, "rust-snapshot-clone-close.json");
}

fn first_live_race_close_abandon_trace() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("snapshot-abandon-race");
    runtime
        .prepare_observed(key, create_test_object, |_, _| Ok(()))
        .expect("initial publication succeeds");
    let rtd_key = key.format_rtd_key();
    runtime
        .connect(1, 30, &rtd_key)
        .expect("Excel connection commits");

    let token = {
        let topic = runtime
            .published
            .load(&key)
            .get(&key)
            .expect("topic is published")
            .clone();
        topic.token.clone()
    };

    let close_runtime = Arc::clone(&runtime);
    *runtime.registry.before_fast_lease_acquire_hook.lock() = Some(Arc::new(move || {
        close_runtime
            .close()
            .expect("close succeeds before acquire");
    }));

    let result = runtime.lookup_handle::<TestObject>(&token);
    assert!(matches!(result, Err(XllError::Closing)));

    write_snapshot_trace(&runtime, "rust-snapshot-abandon-race.json");
}

fn first_live_race_remove_reject_tentative_trace() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("snapshot-reject-tentative");
    runtime
        .prepare_observed(key, create_test_object, |_, _| Ok(()))
        .expect("initial publication succeeds");
    let rtd_key = key.format_rtd_key();
    runtime
        .connect(1, 40, &rtd_key)
        .expect("Excel connection commits");

    let token = {
        let topic = runtime
            .published
            .load(&key)
            .get(&key)
            .expect("topic is published")
            .clone();
        topic.token.clone()
    };

    let disconnect_runtime = Arc::clone(&runtime);
    *runtime.registry.before_fast_lease_acquire_hook.lock() = Some(Arc::new(move || {
        disconnect_runtime.disconnect(1, 40);
    }));

    let result = runtime.lookup_handle::<TestObject>(&token);
    assert!(matches!(result, Err(XllError::StaleHandle)));

    runtime.close().expect("close succeeds");
    write_snapshot_trace(&runtime, "rust-snapshot-reject-tentative.json");
}

fn second_live_race_remove_fallback_trace() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("snapshot-fallback-race");
    runtime
        .prepare_observed(key, create_test_object, |_, _| Ok(()))
        .expect("initial publication succeeds");
    let rtd_key = key.format_rtd_key();
    runtime
        .connect(1, 50, &rtd_key)
        .expect("Excel connection commits");

    let token = {
        let topic = runtime
            .published
            .load(&key)
            .get(&key)
            .expect("topic is published")
            .clone();
        topic.token.clone()
    };

    let disconnect_runtime = Arc::clone(&runtime);
    let disconnect_token = token.clone();
    *runtime.registry.before_fast_upgrade_hook.lock() = Some(Arc::new(move || {
        // Disconnect and remove canonical value so upgrade fails
        disconnect_runtime.disconnect(1, 50);
        disconnect_runtime
            .registry
            .remove_and_drop(&disconnect_token, "test disconnect");
    }));

    let result = runtime.lookup_handle::<TestObject>(&token);
    assert!(matches!(result, Err(XllError::StaleHandle)));

    runtime.close().expect("close succeeds");
    write_snapshot_trace(&runtime, "rust-snapshot-fallback-race.json");
}

fn slot_reuse_aba_trace() {
    let runtime = HandleRuntime::new(8);
    let key1 = test_topic_key("snapshot-aba-1");
    let key2 = test_topic_key("snapshot-aba-2");

    runtime
        .prepare_observed(key1, create_test_object, |_, _| Ok(()))
        .expect("initial publication 1 succeeds");
    let rtd_key1 = key1.format_rtd_key();
    runtime
        .connect(1, 60, &rtd_key1)
        .expect("connection 1 commits");

    let token1 = {
        let topic = runtime
            .published
            .load(&key1)
            .get(&key1)
            .expect("topic 1 is published")
            .clone();
        topic.token.clone()
    };

    // Remove topic 1 -> slot becomes free and reusable
    runtime.disconnect(1, 60);
    runtime.registry.remove_and_drop(&token1, "aba remove 1");

    // Insert topic 2 -> reuses slot with incremented generation
    runtime
        .prepare_observed(key2, create_test_object, |_, _| Ok(()))
        .expect("publication 2 succeeds");
    let rtd_key2 = key2.format_rtd_key();
    runtime
        .connect(1, 61, &rtd_key2)
        .expect("connection 2 commits");

    let token2 = {
        let topic = runtime
            .published
            .load(&key2)
            .get(&key2)
            .expect("topic 2 is published")
            .clone();
        topic.token.clone()
    };

    // Lookup token1 fails (stale generation)
    let stale_lookup = runtime.lookup_handle::<TestObject>(&token1);
    assert!(matches!(stale_lookup, Err(XllError::StaleHandle)));

    // Lookup token2 succeeds
    let handle2 = runtime
        .lookup_handle::<TestObject>(&token2)
        .expect("lookup 2 succeeds");
    drop(handle2);

    runtime.close().expect("close succeeds");
    write_snapshot_trace(&runtime, "rust-snapshot-slot-reuse-aba.json");
}

#[test]
fn rust_handle_snapshot_refinement_traces_are_production_path_replays() {
    fast_lookup_success_trace();
    handle_clone_surviving_close_trace();
    first_live_race_close_abandon_trace();
    first_live_race_remove_reject_tentative_trace();
    second_live_race_remove_fallback_trace();
    slot_reuse_aba_trace();
}
