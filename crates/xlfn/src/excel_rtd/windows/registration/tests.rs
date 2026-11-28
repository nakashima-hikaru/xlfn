use super::*;
use crate::win32::{CoCreateGuid, RPC_S_CALLPENDING};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

static_assertions::assert_not_impl_any!(CrossProcessRegistrationGuard: Send, Sync);

#[test]
fn global_registration_mutex_excludes_other_processes_until_released() {
    const NAME: &str = "XLFN_TEST_REGISTRATION_MUTEX_NAME";
    const MODE: &str = "XLFN_TEST_REGISTRATION_MUTEX_MODE";
    const TEST: &str = concat!(
        "excel_rtd::windows::registration::tests::",
        "global_registration_mutex_excludes_other_processes_until_released"
    );

    if let Ok(name) = std::env::var(NAME) {
        let security = security::MutexSecurity::current_user().unwrap();
        assert!(
            name.strip_prefix(security.name())
                .is_some_and(|suffix| suffix.starts_with("_process_test_"))
        );
        match std::env::var(MODE).unwrap().as_str() {
            "blocked" => {
                let started = Instant::now();
                assert_mutex_timeout(CrossProcessRegistrationGuard::acquire_named_with_timeout(
                    &name, 50,
                ));
                assert!(started.elapsed() < Duration::from_secs(5));
                // Closing a timed-out handle must not release the parent's
                // ownership. This second open must still find it held.
                assert_mutex_timeout(CrossProcessRegistrationGuard::acquire_named_with_timeout(
                    &name, 0,
                ));
            }
            "released" => {
                let _guard = CrossProcessRegistrationGuard::acquire_named(&name).unwrap();
            }
            mode => panic!("unexpected registration mutex child mode: {mode}"),
        }
        return;
    }

    let security = security::MutexSecurity::current_user().unwrap();
    let name = format!(
        "{}_process_test_{}_{}",
        security.name(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let first = CrossProcessRegistrationGuard::acquire_named(&name).unwrap();
    let run_child = |mode| {
        wait_for_child(
            Command::new(std::env::current_exe().unwrap())
                .env(NAME, &name)
                .env(MODE, mode)
                .args(["--exact", TEST])
                .spawn()
                .unwrap(),
        );
    };
    run_child("blocked");
    drop(first);
    run_child("released");
}

fn assert_mutex_timeout(result: XllResult<CrossProcessRegistrationGuard>) {
    assert!(matches!((result).as_ref().map_err(crate::error::construct::view),
        Err(crate::error::construct::ErrorView::WindowsApi {
            function: "CoWaitForMultipleHandles(RTD registration mutex)",
            code: RPC_S_CALLPENDING,
            ..
        })
    ));
}

#[test]
fn registration_mutex_acquires_abandoned_ownership_and_releases_it_once() {
    let security = security::MutexSecurity::current_user().unwrap();
    let name = format!(
        "{}_abandoned_test_{}_{}",
        security.name(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let attributes = security.attributes();
    let wide = wide_nul(&name);
    // SAFETY: the unique name and security descriptor are retained. This
    // unowned reference keeps the object alive after its first owner exits.
    let raw = unsafe { CreateMutexW(&attributes, 0, wide.as_ptr()) };
    assert!(!raw.is_null());
    let _retained = MutexHandle(raw);
    let abandoned_name = name.clone();
    std::thread::spawn(move || {
        let security = security::MutexSecurity::current_user().unwrap();
        let attributes = security.attributes();
        let wide = wide_nul(&abandoned_name);
        // SAFETY: these descriptor/name inputs outlive creation. Existing
        // mutexes ignore initial ownership, so acquire it explicitly below.
        let raw = unsafe { CreateMutexW(&attributes, 0, wide.as_ptr()) };
        assert!(!raw.is_null());
        let handle = MutexHandle(raw);
        // SAFETY: this worker owns a valid handle to the unowned mutex.
        let status = unsafe { crate::win32::WaitForSingleObject(handle.0, 1_000) };
        assert_eq!(status, crate::win32::WAIT_OBJECT_0 as u32);
        // Close this reference without ReleaseMutex. The outer reference
        // retains the kernel object, which becomes abandoned at thread exit.
    })
    .join()
    .unwrap();

    // SAFETY: set a stale nonzero error before CoWait to exercise its mutex
    // last-error workaround without interfering with any live failed call.
    unsafe { SetLastError(ERROR_FILE_NOT_FOUND as u32) };
    let acquired = CrossProcessRegistrationGuard::acquire_named(&name).unwrap();
    drop(acquired);
    std::thread::spawn(move || {
        // Another thread must acquire after Drop; same-thread recursive
        // acquisition would hide an unreleased or multiply acquired mutex.
        let _guard = CrossProcessRegistrationGuard::acquire_named(&name).unwrap();
    })
    .join()
    .unwrap();
}

fn wait_for_child(child: Child) {
    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = OwnedChild(child);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(
                status.success(),
                "registration mutex child failed: {status}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "registration mutex child exceeded its deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn schema_three_scavenging_preserves_fully_linked_schema_two_registrations() {
    // Match the serialization used by the other Windows RTD tests without
    // changing any module, ingress, or server state in this registry test.
    let _runtime = crate::runtime::tests::TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _maintenance = REGISTRATION_MAINTENANCE.lock();
    let _cross_process = CrossProcessRegistrationGuard::acquire().unwrap();
    assert_eq!(RTD_REGISTRATION_SCHEMA, "3");
    let module = format!(
        r"C:\xlfn-tests\registration-schema-transition-{}.xll",
        std::process::id()
    );
    let legacy_one = RegistrationFixture::new("1", &module);
    let legacy_two = RegistrationFixture::new("2", &module);
    let current = RegistrationFixture::new(RTD_REGISTRATION_SCHEMA, &module);

    scavenge_owned_registrations(&module, None).unwrap();

    for (fixture, schema) in [(&legacy_one, "1"), (&legacy_two, "2")] {
        assert_eq!(
            read_registry_string(&fixture.prog_key, "XlFnRegistrationSchema").unwrap(),
            Some(schema.to_owned())
        );
        assert_eq!(
            read_registry_string(&format!("{}\\CLSID", fixture.prog_key), "").unwrap(),
            class_id_from_prog_id(&fixture.prog_id)
        );
        assert_eq!(
            read_registry_string(&format!("{}\\ProgID", fixture.class_key), "").unwrap(),
            Some(fixture.prog_id.clone())
        );
        assert_eq!(
            read_registry_string(&format!("{}\\InProcServer32", fixture.class_key), "").unwrap(),
            Some(module.clone())
        );
    }
    assert!(!registry_key_exists(&current.prog_key).unwrap());
    assert!(!registry_key_exists(&current.class_key).unwrap());
}

struct RegistrationFixture {
    prog_id: String,
    prog_key: String,
    class_key: String,
}

impl RegistrationFixture {
    fn new(schema: &str, module: &str) -> Self {
        let mut id = GUID::default();
        // SAFETY: id points to writable GUID storage.
        assert_eq!(unsafe { CoCreateGuid(&mut id) }, S_OK);
        let class = guid_braced(id);
        let prog_id = format!("{RTD_PROG_ID_PREFIX}{}", guid_compact(id));
        let fixture = Self {
            prog_key: format!("{CLASSES_KEY}\\{prog_id}"),
            class_key: format!("{CLSID_KEY}\\{class}"),
            prog_id,
        };
        for (name, value) in [
            ("XlFnOwner", RTD_REGISTRATION_OWNER),
            ("XlFnRegistrationSchema", schema),
            ("XlFnOwnerModule", module),
            ("XlFnClassId", class.as_str()),
        ] {
            set_registry_value(&fixture.prog_key, Some(name), value).unwrap();
        }
        set_registry_value(&format!("{}\\CLSID", fixture.prog_key), None, &class).unwrap();
        set_registry_value(
            &format!("{}\\ProgID", fixture.class_key),
            None,
            &fixture.prog_id,
        )
        .unwrap();
        set_registry_value(
            &format!("{}\\InProcServer32", fixture.class_key),
            None,
            module,
        )
        .unwrap();
        fixture
    }
}

impl Drop for RegistrationFixture {
    fn drop(&mut self) {
        for path in [&self.class_key, &self.prog_key] {
            let _ = delete_registry_tree(path);
        }
    }
}
