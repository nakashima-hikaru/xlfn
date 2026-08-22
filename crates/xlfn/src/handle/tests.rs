use super::*;

fn generation(raw: u64) -> crate::generation::RuntimeGeneration {
    crate::generation::RuntimeGeneration::new(raw).expect("test generation is non-zero")
}

fn server_generation(raw: u64) -> crate::generation::ServerGeneration {
    crate::generation::ServerGeneration::new(raw).expect("test server generation is non-zero")
}
use crate::input_identity::InputFingerprint;
use crate::input_identity::InputFingerprintBuilder;

fn format_formula_revision_key(
    caller: FormulaCaller,
    udf_id: &'static str,
    input_bytes: &[u8; 32],
) -> String {
    FormulaRevisionKey::new(caller, udf_id, InputFingerprint::from_bytes(*input_bytes))
        .format_rtd_key()
}

fn insert_production<T>(registry: &HandleRegistry, value: Arc<T>) -> XllResult<String>
where
    T: Send + Sync + 'static,
{
    let mut value = Some(
        Arc::try_unwrap(value)
            .ok()
            .expect("test value is uniquely owned"),
    );
    registry.insert_pending(&mut value)
}

fn with_handle<T, R>(
    runtime: &HandleRuntime,
    token: &str,
    operation: impl for<'call> FnOnce(Handle<'call, T>) -> R,
) -> XllResult<R>
where
    T: ExcelHandleObject,
{
    crate::value::with_excel_call_scope(|scope| runtime.lookup(scope, token).map(operation))
}

fn input_identity<'call, T: ExcelHandleObject>(value: &Handle<'call, T>) -> InputFingerprint {
    let mut builder = InputFingerprintBuilder::new(1);
    builder
        .with_argument(0, "handle", |encoder| {
            encoder.u64(value.object.id.0.0);
            Ok(())
        })
        .unwrap();
    builder.finish().unwrap()
}

fn reference_handle_identity(object_id: u64) -> InputFingerprint {
    let mut builder = InputFingerprintBuilder::new(1);
    builder
        .with_argument(0, "handle", |encoder| {
            encoder.u64(object_id);
            Ok(())
        })
        .unwrap();
    builder.finish().unwrap()
}

fn semantic_handle_key<T: ExcelHandleObject>(handle: &Handle<'_, T>) -> HandleTopicKey {
    HandleTopicKey::Formula(FormulaRevisionKey::new(
        FormulaCaller {
            sheet_id: 0,
            row: 20,
            column: 4,
        },
        "TEST.SEMANTIC.HANDLE",
        input_identity(handle),
    ))
}

#[derive(serde::Deserialize)]
struct SerializationGoldenFile {
    schema_version: u32,
    vectors: Vec<SerializationGoldenVector>,
}

#[derive(serde::Deserialize)]
struct SerializationGoldenVector {
    name: String,
    sheet_id: u64,
    row: i32,
    column: i32,
    udf_id: String,
    digest_hex: String,
    rtd_key: String,
}

fn decode_digest_hex(hex: &str) -> [u8; 32] {
    fn nibble(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => panic!("golden digest contains a non-lowercase-hex byte"),
        }
    }

    assert_eq!(hex.len(), 64, "golden digest must contain 32 bytes");
    let mut digest = [0_u8; 32];
    let (chunks, _) = hex.as_bytes().as_chunks::<2>();
    for (index, pair) in chunks.iter().enumerate() {
        digest[index] = nibble(pair[0]) << 4 | nibble(pair[1]);
    }
    digest
}

#[test]
fn rtd_key_golden_vectors_match_wire_contract() {
    let golden: SerializationGoldenFile = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../formal/fixtures/topics/serialization-golden.json"
    )))
    .expect("serialization golden vectors must be valid JSON");
    assert_eq!(golden.schema_version, 1);

    for vector in golden.vectors {
        let Some(sheet_id) = usize::try_from(vector.sheet_id).ok() else {
            // The 64-bit boundary vector is intentionally not representable
            // when this test is compiled for a 32-bit target.
            continue;
        };
        let udf_id: &'static str = Box::leak(vector.udf_id.into_boxed_str());
        let digest = decode_digest_hex(&vector.digest_hex);
        let actual = format_formula_revision_key(
            FormulaCaller {
                sheet_id,
                row: vector.row,
                column: vector.column,
            },
            udf_id,
            &digest,
        );

        assert_eq!(
            actual.as_bytes(),
            vector.rtd_key.as_bytes(),
            "Rust RTD formatter disagrees with golden vector {}",
            vector.name
        );
    }
}

#[test]
fn formula_revision_key_uses_the_stable_sheet_identifier() {
    let digest = [0xab_u8; 32];
    let caller = FormulaCaller {
        sheet_id: 17,
        row: 4,
        column: 8,
    };
    let first = format_formula_revision_key(caller, "TEST.CREATE", &digest);
    let recalculated = format_formula_revision_key(caller, "TEST.CREATE", &digest);
    let other_sheet = format_formula_revision_key(
        FormulaCaller {
            sheet_id: 18,
            ..caller
        },
        "TEST.CREATE",
        &digest,
    );

    assert_eq!(first, recalculated);
    assert_ne!(first, other_sheet);
}

#[test]
fn formula_revision_key_changes_with_every_component() {
    let digest = [0x12_u8; 32];
    let caller = FormulaCaller {
        sheet_id: 17,
        row: 4,
        column: 8,
    };
    let base = format_formula_revision_key(caller, "TEST.CREATE", &digest);

    assert_ne!(
        base,
        format_formula_revision_key(
            FormulaCaller {
                sheet_id: 18,
                ..caller
            },
            "TEST.CREATE",
            &digest,
        )
    );
    assert_ne!(
        base,
        format_formula_revision_key(FormulaCaller { row: 5, ..caller }, "TEST.CREATE", &digest,)
    );
    assert_ne!(
        base,
        format_formula_revision_key(
            FormulaCaller {
                column: 9,
                ..caller
            },
            "TEST.CREATE",
            &digest,
        )
    );
    assert_ne!(
        base,
        format_formula_revision_key(caller, "TEST.OTHER", &digest)
    );
    assert_ne!(
        base,
        format_formula_revision_key(caller, "TEST.CREATE", &[0x13_u8; 32])
    );

    assert!(base.ends_with("1212121212121212121212121212121212121212121212121212121212121212"));
}

#[test]
fn published_topic_keeps_identity_and_rtd_reverse_maps_consistent() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("reverse-map");
    let expected_rtd_key = key.format_rtd_key();
    let observed = Arc::new(Mutex::new(None::<String>));
    let observed_for_callback = Arc::clone(&observed);

    let (token, created) = runtime
        .prepare_observed(
            key,
            || Ok(DataRecord(7)),
            move |rtd_key, observed_token| {
                assert!(!observed_token.is_empty());
                *observed_for_callback.lock() = Some(rtd_key.to_owned());
                Ok(())
            },
        )
        .unwrap();
    assert!(created);

    let rtd_key = observed
        .lock()
        .clone()
        .expect("observation key was recorded");
    assert_eq!(rtd_key, expected_rtd_key);
    let topics = runtime.topics.read();
    let identity = topics
        .by_rtd_key
        .get(rtd_key.as_str())
        .copied()
        .expect("published RTD key must resolve to its identity");
    let topic = topics
        .by_key
        .get(&identity)
        .expect("reverse map identity must resolve to a topic");
    assert_eq!(topic.publication.rtd_key.as_ref(), rtd_key.as_str());
    assert_eq!(topic.publication.token, token);
    assert!(topics.by_excel_id.is_empty());
    drop(topics);

    let published = runtime.topics.published().load(&key);
    let publication = published
        .get(&key)
        .expect("successful observation must commit its published snapshot");
    assert_eq!(
        publication.state.load(Ordering::Acquire),
        PublishedTopicState::Live as u8
    );

    runtime.connect(server_generation(1), 41, &rtd_key).unwrap();
    {
        let topics = runtime.topics.read();
        assert_eq!(topics.by_excel_id.len(), 1);
        assert_eq!(topics.by_excel_id.values().next(), Some(&identity));
    }

    runtime.disconnect(server_generation(1), 41);
    let topics = runtime.topics.read();
    assert!(topics.by_key.is_empty());
    assert!(topics.by_rtd_key.is_empty());
    assert!(topics.by_excel_id.is_empty());
}

#[test]
fn cold_publication_stays_out_of_fast_snapshot_until_observation_succeeds() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("publication-commit-after-observation");

    runtime
        .prepare_observed(
            key,
            || Ok(DataRecord(1)),
            |_, _| {
                let published = runtime.topics.published().load(&key);
                assert!(published.get(&key).is_none());
                Ok(())
            },
        )
        .unwrap();

    assert!(runtime.topics.published().load(&key).get(&key).is_some());
}

#[test]
fn publication_rejects_rtd_key_collision_without_overwriting_existing_topic() {
    let runtime = HandleRuntime::new(8);
    let first_key = test_topic_key("collision-first");
    let second_key = test_topic_key("collision-second");
    let first_rtd_key = first_key.format_rtd_key();
    let second_rtd_key = second_key.format_rtd_key();
    assert_ne!(first_rtd_key, second_rtd_key);

    let (first_token, created) = runtime.prepare(first_key, || Ok(DataRecord(1))).unwrap();
    assert!(created);

    // Force the state that a formatter collision would present at the
    // publication boundary: the existing topic owns the incoming RTD key.
    {
        let mut topics = runtime.topics.write();
        topics.by_rtd_key.remove(first_rtd_key.as_str());
        topics
            .by_rtd_key
            .insert(Arc::from(second_rtd_key.as_str()), first_key);
    }

    let result = runtime.prepare(second_key, || Ok(DataRecord(2)));
    assert!(matches!(
        result,
        Err(XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::HANDLE_TOPIC_COLLISION
        })
    ));

    {
        let topics = runtime.topics.read();
        assert_eq!(topics.by_key.len(), 1);
        assert_eq!(
            topics
                .by_key
                .get(&first_key)
                .map(|topic| &topic.publication.token),
            Some(&first_token)
        );
        assert!(!topics.by_key.contains_key(&second_key));
        assert_eq!(
            topics.by_rtd_key.get(second_rtd_key.as_str()),
            Some(&first_key)
        );
    }

    // Restore the valid first-topic mapping so the test can release its
    // registry root through the normal rollback path.
    {
        let mut topics = runtime.topics.write();
        topics.by_rtd_key.remove(second_rtd_key.as_str());
        topics
            .by_rtd_key
            .insert(Arc::from(first_rtd_key.as_str()), first_key);
    }
    runtime.rollback(&first_rtd_key);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn ref_caller_uses_embedded_sheet_id_without_sheet_callbacks() {
    let _callback_guard = crate::test_callback::lock();
    crate::test_callback::install();
    crate::test_callback::reset();
    crate::test_callback::set_formula_caller(crate::test_callback::FormulaCallerKind::Ref);

    let callbacks = crate::host_callback::HostCallbackSession::new();
    let caller = resolve_formula_caller(&callbacks).unwrap();

    assert_eq!(
        caller,
        FormulaCaller {
            sheet_id: 17,
            row: 11,
            column: 3,
        }
    );
    assert_eq!(crate::test_callback::calls_for(xlfn_sys::XLF_CALLER), 1);
    assert_eq!(crate::test_callback::calls_for(xlfn_sys::XL_SHEET_NM), 0);
    assert_eq!(crate::test_callback::calls_for(xlfn_sys::XL_SHEET_ID), 0);
    assert_eq!(crate::test_callback::free_calls(), 1);
}

#[test]
fn sref_caller_keeps_sheet_lookup_fallback() {
    let _callback_guard = crate::test_callback::lock();
    crate::test_callback::install();
    crate::test_callback::reset();
    crate::test_callback::set_formula_caller(crate::test_callback::FormulaCallerKind::SRef);

    let callbacks = crate::host_callback::HostCallbackSession::new();
    let caller = resolve_formula_caller(&callbacks).unwrap();

    assert_eq!(
        caller,
        FormulaCaller {
            sheet_id: 19,
            row: 11,
            column: 3,
        }
    );
    assert_eq!(crate::test_callback::calls_for(xlfn_sys::XLF_CALLER), 1);
    assert_eq!(crate::test_callback::calls_for(xlfn_sys::XL_SHEET_NM), 1);
    assert_eq!(crate::test_callback::calls_for(xlfn_sys::XL_SHEET_ID), 1);
    assert_eq!(crate::test_callback::free_calls(), 3);
}

#[test]
fn generation_prevents_aba_and_lookup_keeps_value_alive() {
    #[derive(Clone)]
    struct TestObj(&'static str);
    impl ExcelHandleObject for TestObj {}

    let registry = HandleRegistry::new(4);
    let token = insert_production(&registry, Arc::new(TestObj("first"))).unwrap();

    crate::value::with_excel_call_scope(|scope| {
        let borrowed = registry.lookup_handle::<TestObj>(scope, &token).unwrap();
        assert_eq!(borrowed.0, "first");

        registry.remove::<TestObj>(&token).unwrap();
        assert!(matches!(
            registry.lookup::<TestObj>(&token),
            Err(XllError::StaleHandle)
        ));

        let replacement = insert_production(&registry, Arc::new(TestObj("replacement"))).unwrap();
        assert_ne!(token, replacement);
        assert_eq!(borrowed.0, "first");

        let replacement_handle = registry
            .lookup_handle::<TestObj>(scope, &replacement)
            .unwrap();
        assert_eq!(replacement_handle.0, "replacement");
    });
}

#[test]
fn one_call_scope_carries_one_object_store_capability() {
    struct ScopeObject(u32);
    impl ExcelHandleObject for ScopeObject {}

    let first = HandleRegistry::new(2);
    let second = HandleRegistry::new(2);
    let first_token = insert_production(&first, Arc::new(ScopeObject(1))).unwrap();
    let second_token = insert_production(&second, Arc::new(ScopeObject(2))).unwrap();

    crate::value::with_excel_call_scope(|scope| {
        let first_handle = first
            .lookup_handle::<ScopeObject>(scope, &first_token)
            .expect("the first runtime establishes the call object store");
        assert_eq!(first_handle.0, 1);

        assert!(matches!(
            second.lookup_handle::<ScopeObject>(scope, &second_token),
            Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::HANDLE_CONTEXT
            })
        ));
    });
}

#[test]
fn published_binding_snapshot_does_not_own_object_after_retirement() {
    struct Counted(Arc<AtomicUsize>);
    impl ExcelHandleObject for Counted {}
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    let drops = Arc::new(AtomicUsize::new(0));
    let registry = HandleRegistry::new(2);
    let token = insert_production(&registry, Arc::new(Counted(Arc::clone(&drops)))).unwrap();
    let parsed = registry
        .codec
        .parse(
            std::ptr::from_ref(&registry).addr(),
            HandleToken::new(&token),
        )
        .unwrap();
    let snapshot = registry.bindings.published().load(parsed.id.slot);
    let publication = snapshot
        .get(parsed.id.slot)
        .expect("inserted handle must be published");
    assert_eq!(publication.state(), BindingState::Live);

    registry.remove::<Counted>(&token).unwrap();
    assert_eq!(publication.state(), BindingState::Retired);
    assert_eq!(drops.load(Ordering::Relaxed), 1);

    drop(snapshot);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn reused_slot_keeps_old_borrow_separate_from_new_generation() {
    #[derive(Clone)]
    struct TestObj(&'static str);
    impl ExcelHandleObject for TestObj {}

    let registry = HandleRegistry::new(2);
    let token1 = insert_production(&registry, Arc::new(TestObj("first"))).unwrap();
    let parsed1 = registry
        .codec
        .parse(
            std::ptr::from_ref(&registry).addr(),
            HandleToken::new(&token1),
        )
        .unwrap();

    crate::value::with_excel_call_scope(|scope| {
        let old = registry.lookup_handle::<TestObj>(scope, &token1).unwrap();
        registry.remove::<TestObj>(&token1).unwrap();

        let token2 = insert_production(&registry, Arc::new(TestObj("second"))).unwrap();
        let parsed2 = registry
            .codec
            .parse(
                std::ptr::from_ref(&registry).addr(),
                HandleToken::new(&token2),
            )
            .unwrap();
        assert_eq!(parsed1.id.slot, parsed2.id.slot);
        assert_ne!(parsed1.id.generation, parsed2.id.generation);
        assert_eq!(old.0, "first");

        assert!(matches!(
            registry.lookup_handle::<TestObj>(scope, &token1),
            Err(XllError::StaleHandle)
        ));
        assert_eq!(
            registry.lookup_handle::<TestObj>(scope, &token2).unwrap().0,
            "second"
        );
    });
}

#[test]
fn close_rejects_new_borrows_but_retires_after_existing_call_release() {
    #[derive(Clone)]
    struct TestObj(&'static str);
    impl ExcelHandleObject for TestObj {}

    let registry = HandleRegistry::new(2);
    let token = insert_production(&registry, Arc::new(TestObj("live"))).unwrap();

    crate::value::with_excel_call_scope(|scope| {
        let borrowed = registry.lookup_handle::<TestObj>(scope, &token).unwrap();
        registry.seal().map(|_| ()).unwrap();
        assert_eq!(borrowed.0, "live");
        assert!(matches!(
            registry.lookup_handle::<TestObj>(scope, &token),
            Err(XllError::Closing)
        ));
    });
}

#[test]
fn exhausted_generation_retires_the_slot_permanently() {
    let registry = HandleRegistry::new(2);
    let first = insert_production(&registry, Arc::new(1_u32)).unwrap();
    registry.remove::<u32>(&first).unwrap();
    registry.bindings.write_state().slots[0].next_generation =
        crate::generation::BindingGeneration::new(u64::MAX).unwrap();
    let final_token = insert_production(&registry, Arc::new(1_u32)).unwrap();
    registry.remove::<u32>(&final_token).unwrap();
    assert!(registry.bindings.read_state().free.is_empty());

    let replacement = insert_production(&registry, Arc::new(2_u32)).unwrap();
    assert_eq!(
        registry
            .codec
            .parse(
                std::ptr::from_ref(&registry).addr(),
                HandleToken::new(&replacement),
            )
            .unwrap()
            .id
            .slot,
        1
    );
    assert!(matches!(
        registry.lookup::<u32>(&final_token),
        Err(XllError::StaleHandle)
    ));
}

#[test]
fn corruption_and_cross_session_tokens_are_rejected() {
    let first = HandleRegistry::new(2);
    let second = HandleRegistry::new(2);
    let token = insert_production(&first, Arc::new(1_u32)).unwrap();
    let fields = token.split(':').collect::<Vec<_>>();
    assert_eq!(fields[1], "3");
    assert_eq!(fields[5].len(), 32);
    let mut corrupted = token.clone();
    let last = corrupted.pop().unwrap();
    corrupted.push(if last == '0' { '1' } else { '0' });
    assert!(first.lookup::<u32>(&corrupted).is_err());
    let forged = format!(
        "xllh:3:{}:{}:{}:{}",
        fields[2],
        fields[3],
        fields[4],
        "0".repeat(32)
    );
    assert!(first.lookup::<u32>(&forged).is_err());
    assert!(second.lookup::<u32>(&token).is_err());
}

#[test]
fn csprng_failure_is_a_stable_initialization_error_not_a_panic() {
    let error = HandleRegistry::try_new_with(2, |_| Err("injected CSPRNG failure"), false)
        .err()
        .expect("injected entropy failure is returned");
    assert!(matches!(
        error,
        XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::HANDLE_ENTROPY
        }
    ));
}

#[test]
fn close_invalidates_tokens_and_rejects_new_bindings() {
    let registry = HandleRegistry::new(2);
    let token = insert_production(&registry, Arc::new(42_u32)).unwrap();
    let value = registry.lookup::<u32>(&token).unwrap();
    registry.seal().map(|_| ()).unwrap();
    assert!(registry.lookup::<u32>(&token).is_err());
    assert_eq!(value, 42);
    assert!(matches!(
        insert_production(&registry, Arc::new(7_u32)),
        Err(XllError::Closing)
    ));
}

#[cfg(not(all(target_os = "windows", target_arch = "x86")))]
#[test]
#[ignore = "run in the dedicated Shuttle test step"]
fn shuttle_insert_racing_close_never_leaves_a_live_handle() {
    shuttle::check_random(
        || {
            let registry = shuttle::sync::Arc::new(HandleRegistry::new(2));
            let inserting = shuttle::sync::Arc::clone(&registry);
            let worker = shuttle::thread::spawn(move || {
                shuttle::thread::yield_now();
                insert_production(&inserting, Arc::new(42_u32))
            });

            shuttle::thread::yield_now();
            registry.seal().map(|_| ()).unwrap();
            let result = worker.join().expect("insertion thread panicked");

            assert_eq!(registry.len(), 0);
            match result {
                Ok(token) => assert!(matches!(
                    registry.lookup::<u32>(&token),
                    Err(XllError::Closing)
                )),
                Err(error) => assert!(matches!(error, XllError::Closing)),
            }
        },
        100,
    );
}

#[test]
fn wrong_remove_type_does_not_consume_handle() {
    let registry = HandleRegistry::new(2);
    let token = insert_production(&registry, Arc::new(42_u32)).unwrap();
    assert!(matches!(
        registry.remove::<String>(&token),
        Err(XllError::InvalidHandle)
    ));
    assert_eq!(registry.lookup::<u32>(&token).unwrap(), 42);
}

#[test]
fn close_drops_values_outside_registry_lock() {
    struct ReenterOnDrop {
        registry: Arc<HandleRegistry>,
    }
    impl Drop for ReenterOnDrop {
        fn drop(&mut self) {
            assert!(matches!(
                insert_production(&self.registry, Arc::new(1_u32)),
                Err(XllError::Closing)
            ));
        }
    }

    let registry = Arc::new(HandleRegistry::new(2));
    insert_production(
        &registry,
        Arc::new(ReenterOnDrop {
            registry: Arc::clone(&registry),
        }),
    )
    .unwrap();
    registry.seal().map(|_| ()).unwrap();
}

#[test]
fn close_contains_panicking_destructors_and_continues_dropping() {
    struct PanicOnDrop;
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("injected handle destructor panic");
        }
    }

    struct CountOnDrop(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for CountOnDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registry = HandleRegistry::new(2);
    insert_production(&registry, Arc::new(PanicOnDrop)).unwrap();
    insert_production(&registry, Arc::new(CountOnDrop(Arc::clone(&drops)))).unwrap();

    assert!(matches!(registry.seal().map(|_| ()), Err(XllError::Panic)));
    assert_eq!(registry.len(), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[derive(Debug)]
struct DataRecord(u32);

impl ExcelHandleObject for DataRecord {}

struct SimpleResource;

impl ExcelHandleObject for SimpleResource {}

fn token_value(token: &str) -> (Vec<u16>, xlfn_sys::XLOPER12) {
    let mut encoded = Vec::with_capacity(token.encode_utf16().count() + 1);
    encoded.push(token.encode_utf16().count() as u16);
    encoded.extend(token.encode_utf16());
    let raw = xlfn_sys::XLOPER12 {
        value: xlfn_sys::XLOPER12Value {
            string: encoded.as_mut_ptr(),
        },
        xltype: xlfn_sys::XLTYPE_STR,
    };
    (encoded, raw)
}

#[test]
fn repeated_formula_revision_runs_factory_exactly_once() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("same");
    let rtd_key = key.format_rtd_key();
    let calls = AtomicUsize::new(0);

    let (first, created) = runtime
        .prepare(key, || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(DataRecord(1))
        })
        .unwrap();
    assert!(created);

    let (second, created) = runtime
        .prepare(key, || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(DataRecord(2))
        })
        .unwrap();
    assert!(!created);
    assert_eq!(first, second);
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &first, |value| value.0).unwrap(),
        1
    );

    runtime.connect(server_generation(1), 41, &rtd_key).unwrap();
    runtime.disconnect(server_generation(1), 41);
    assert_eq!(runtime.len(), 0);
    assert!(matches!(
        with_handle::<DataRecord, _>(&runtime, &first, |_| ()),
        Err(XllError::StaleHandle)
    ));
}

#[test]
fn explicit_handle_argument_conversion_resolves_a_typed_token() {
    let runtime: &'static crate::runtime::Runtime<()> =
        Box::leak(Box::new(crate::runtime::Runtime::new()));
    runtime.arm_test_generation();
    let handles = runtime.handles().unwrap();
    let (token, _) = handles
        .prepare(test_topic_key("argument"), || Ok(DataRecord(19)))
        .unwrap();
    let (_encoded, mut raw) = token_value(&token);

    crate::value::with_excel_call_scope(|scope| {
        // SAFETY: `raw` and its counted UTF-16 storage remain live for conversion.
        let resolved: Handle<'_, DataRecord> = unsafe {
            crate::value::argument_from_raw_with_context(scope, runtime, "dataset", &mut raw)
        }
        .unwrap();
        assert_eq!(resolved.0, 19);
    });
}

#[test]
fn explicit_handle_lease_argument_conversion_leases_the_payload() {
    let runtime: &'static crate::runtime::Runtime<()> =
        Box::leak(Box::new(crate::runtime::Runtime::new()));
    runtime.arm_test_generation();
    let handles = runtime.handles().unwrap();
    let (token, _) = handles
        .prepare(test_topic_key("async-argument"), || Ok(DataRecord(29)))
        .unwrap();
    let (_encoded, mut raw) = token_value(&token);

    let resolved: HandleLease<DataRecord> = crate::value::with_excel_call_scope(|scope| {
        // SAFETY: `raw` and its counted UTF-16 storage remain live for conversion.
        unsafe { crate::value::argument_from_raw_with_context(scope, runtime, "dataset", &mut raw) }
            .unwrap()
    });
    handles
        .registry
        .remove_and_drop(&token, "test remove async argument");
    assert_eq!(resolved.0, 29);
    drop(resolved);
}

#[test]
fn generic_handle_conversion_rejects_wrong_stale_foreign_and_tampered_tokens() {
    let runtime: &'static crate::runtime::Runtime<()> =
        Box::leak(Box::new(crate::runtime::Runtime::new()));
    runtime.arm_test_generation();
    let handles = runtime.handles().unwrap();
    let key = test_topic_key("argument-errors");
    let rtd_key = key.format_rtd_key();
    let (token, _) = handles.prepare(key, || Ok(DataRecord(23))).unwrap();
    handles.connect(server_generation(1), 91, &rtd_key).unwrap();

    let (_wrong_encoded, mut wrong_raw) = token_value(&token);
    // SAFETY: `wrong_raw` and its counted UTF-16 storage remain live for conversion.
    crate::value::with_excel_call_scope(|scope| {
        // SAFETY: `wrong_raw` remains live for the duration of this conversion.
        let wrong = unsafe {
            crate::value::argument_from_raw_with_context::<_, Handle<'_, SimpleResource>>(
                scope,
                runtime,
                "curve",
                &mut wrong_raw,
            )
        };
        assert!(matches!(wrong, Err(XllError::InvalidHandle)));
    });

    let foreign_runtime: &'static crate::runtime::Runtime<()> =
        Box::leak(Box::new(crate::runtime::Runtime::new()));
    foreign_runtime.arm_test_generation();
    let (_foreign_encoded, mut foreign_raw) = token_value(&token);
    // SAFETY: `foreign_raw` and its counted UTF-16 storage remain live for conversion.
    crate::value::with_excel_call_scope(|scope| {
        // SAFETY: `foreign_raw` remains live for the duration of this conversion.
        let foreign = unsafe {
            crate::value::argument_from_raw_with_context::<_, Handle<'_, DataRecord>>(
                scope,
                foreign_runtime,
                "dataset",
                &mut foreign_raw,
            )
        };
        assert!(matches!(foreign, Err(XllError::InvalidHandle)));
    });

    let mut tampered = token.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == '0' { '1' } else { '0' });
    let (_tampered_encoded, mut tampered_raw) = token_value(&tampered);
    // SAFETY: `tampered_raw` and its counted UTF-16 storage remain live for conversion.
    crate::value::with_excel_call_scope(|scope| {
        // SAFETY: `tampered_raw` remains live for the duration of this conversion.
        let tampered = unsafe {
            crate::value::argument_from_raw_with_context::<_, Handle<'_, DataRecord>>(
                scope,
                runtime,
                "dataset",
                &mut tampered_raw,
            )
        };
        assert!(matches!(tampered, Err(XllError::InvalidHandle)));
    });

    handles.disconnect(server_generation(1), 91);
    let (_stale_encoded, mut stale_raw) = token_value(&token);
    // SAFETY: `stale_raw` and its counted UTF-16 storage remain live for conversion.
    crate::value::with_excel_call_scope(|scope| {
        // SAFETY: `stale_raw` remains live for the duration of this conversion.
        let stale = unsafe {
            crate::value::argument_from_raw_with_context::<_, Handle<'_, DataRecord>>(
                scope,
                runtime,
                "dataset",
                &mut stale_raw,
            )
        };
        assert!(matches!(stale, Err(XllError::StaleHandle)));
    });
}

#[test]
fn optional_handle_conversion_preserves_blank_and_missing_policy() {
    let runtime: &'static crate::runtime::Runtime<()> =
        Box::leak(Box::new(crate::runtime::Runtime::new()));
    let mut blank = xlfn_sys::XLOPER12::nil();
    let mut missing = xlfn_sys::XLOPER12::missing();
    // SAFETY: `blank` remains live for the duration of conversion.
    crate::value::with_excel_call_scope(|scope| {
        // SAFETY: `blank` remains live for the duration of this conversion.
        let blank_value = unsafe {
            crate::value::argument_from_raw_with_context::<_, Option<Handle<'_, DataRecord>>>(
                scope, runtime, "dataset", &mut blank,
            )
        }
        .unwrap();
        assert!(blank_value.is_none());
    });
    // SAFETY: `missing` remains live for the duration of conversion.
    crate::value::with_excel_call_scope(|scope| {
        // SAFETY: `missing` remains live for the duration of this conversion.
        let missing_value = unsafe {
            crate::value::argument_from_raw_with_context::<_, Option<Handle<'_, DataRecord>>>(
                scope,
                runtime,
                "dataset",
                &mut missing,
            )
        }
        .unwrap();
        assert!(missing_value.is_none());
    });

    // SAFETY: `blank` remains live for the duration of conversion.
    crate::value::with_excel_call_scope(|scope| {
        // SAFETY: `blank` remains live for the duration of this conversion.
        let direct_blank = unsafe {
            crate::value::argument_from_raw_with_context::<_, Handle<'_, DataRecord>>(
                scope, runtime, "dataset", &mut blank,
            )
        };
        assert!(direct_blank.is_err());
    });
}

#[test]
fn existing_handle_publication_creates_an_independent_formula_owner() {
    let runtime = HandleRuntime::new(8);
    let source_key = test_topic_key("source");
    let source_rtd_key = source_key.format_rtd_key();
    let (source_token, _) = runtime.prepare(source_key, || Ok(DataRecord(31))).unwrap();
    runtime
        .connect(server_generation(1), 1, &source_rtd_key)
        .unwrap();

    let alias_key = test_topic_key("alias");
    let alias_rtd_key = alias_key.format_rtd_key();
    let (alias_token, object_id) = crate::value::with_excel_call_scope(|scope| {
        let resolved: Handle<'_, DataRecord> = runtime.lookup(scope, &source_token).unwrap();
        let object = resolved.alias().into_locator();
        let alias = runtime
            .prepare_observed_alias::<DataRecord, _>(alias_key, object, |_, _| Ok(()))
            .unwrap();
        (alias.0, object.id)
    });
    runtime
        .connect(server_generation(1), 2, &alias_rtd_key)
        .unwrap();
    assert_ne!(source_token, alias_token);
    let source_binding = runtime
        .registry
        .codec
        .parse(
            std::ptr::from_ref(&runtime.registry).addr(),
            HandleToken::new(&source_token),
        )
        .unwrap()
        .id;
    let alias_binding = runtime
        .registry
        .codec
        .parse(
            std::ptr::from_ref(&runtime.registry).addr(),
            HandleToken::new(&alias_token),
        )
        .unwrap()
        .id;
    let state = runtime.registry.bindings.read_state();
    let alias_object_id = state.slots[alias_binding.slot as usize]
        .record
        .as_ref()
        .unwrap()
        .object
        .id;
    assert_ne!(source_binding, alias_binding);
    assert_eq!(alias_object_id, object_id);
    drop(state);

    runtime.disconnect(server_generation(1), 1);
    assert!(matches!(
        with_handle::<DataRecord, _>(&runtime, &source_token, |_| ()),
        Err(XllError::StaleHandle)
    ));
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &alias_token, |value| value.0).unwrap(),
        31
    );

    runtime.disconnect(server_generation(1), 2);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn aliased_binding_survives_source_retirement_and_drops_once() {
    struct DropTracked {
        value: u32,
        drops: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ExcelHandleObject for DropTracked {}

    impl Drop for DropTracked {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime = HandleRuntime::new(8);
    let source_key = test_topic_key("alias-binding-source");
    let source_rtd_key = source_key.format_rtd_key();
    let (source_token, _) = runtime
        .prepare(source_key, || {
            Ok(DropTracked {
                value: 73,
                drops: Arc::clone(&drops),
            })
        })
        .unwrap();
    runtime
        .connect(server_generation(1), 3, &source_rtd_key)
        .unwrap();

    let alias_key = test_topic_key("alias-binding-target");
    let alias_rtd_key = alias_key.format_rtd_key();
    let alias_token = crate::value::with_excel_call_scope(|scope| {
        let source: Handle<'_, DropTracked> = runtime.lookup(scope, &source_token).unwrap();
        let object = source.alias().into_locator();
        runtime
            .prepare_observed_alias::<DropTracked, _>(alias_key, object, |_, _| Ok(()))
            .unwrap()
            .0
    });

    runtime.disconnect(server_generation(1), 3);
    assert!(matches!(
        with_handle::<DropTracked, _>(&runtime, &source_token, |_| ()),
        Err(XllError::StaleHandle)
    ));
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    runtime
        .connect(server_generation(1), 4, &alias_rtd_key)
        .unwrap();

    assert_eq!(
        with_handle::<DropTracked, _>(&runtime, &alias_token, |handle| (*handle).value).unwrap(),
        73
    );
    runtime.disconnect(server_generation(1), 4);
    assert_eq!(runtime.len(), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn alias_publication_resurrects_a_retired_object_with_a_new_storage_key() {
    struct DropTracked {
        value: u32,
        drops: Arc<AtomicUsize>,
    }

    impl ExcelHandleObject for DropTracked {}

    impl Drop for DropTracked {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    let drops = Arc::new(AtomicUsize::new(0));
    let runtime = HandleRuntime::new(8);
    let source_key = test_topic_key("resurrection-source");
    let (source_token, _) = runtime
        .prepare_observed(
            source_key,
            || {
                Ok(DropTracked {
                    value: 107,
                    drops: Arc::clone(&drops),
                })
            },
            |_, _| Ok(()),
        )
        .unwrap();

    let alias_key = test_topic_key("resurrection-alias");
    let alias_token = crate::value::with_excel_call_scope(|scope| {
        let source: Handle<'_, DropTracked> = runtime.lookup(scope, &source_token).unwrap();
        let object = source.alias().into_locator();

        // The active call epoch keeps the detached payload available for the
        // alias publication even though its last live binding is gone.
        runtime
            .registry
            .remove_and_drop(&source_token, "retire source before alias publication");

        let (alias_token, _) = runtime
            .prepare_observed_alias::<DropTracked, _>(alias_key, object, |_, _| Ok(()))
            .unwrap();

        let alias_id = runtime
            .registry
            .codec
            .parse(
                std::ptr::from_ref(&runtime.registry).addr(),
                HandleToken::new(&alias_token),
            )
            .unwrap()
            .id;
        let state = runtime.registry.bindings.read_state();
        let alias_record = state.slots[alias_id.slot as usize]
            .record
            .as_ref()
            .expect("resurrected alias must have a canonical record");
        assert_eq!(alias_record.object.id, object.id);
        assert_ne!(alias_record.object.key, object.key_hint);
        drop(state);

        (alias_token, object.id)
    })
    .0;

    assert_eq!(
        with_handle::<DropTracked, _>(&runtime, &alias_token, |handle| (*handle).value).unwrap(),
        107
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    runtime
        .registry
        .remove_and_drop(&alias_token, "remove resurrected alias");
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    runtime.seal().map(|_| ()).unwrap();
}

#[test]
fn aliases_of_one_object_have_one_semantic_input_identity() {
    let runtime = HandleRuntime::new(8);
    let source_key = test_topic_key("identity-source");
    let source_rtd_key = source_key.format_rtd_key();
    let (source_token, _) = runtime.prepare(source_key, || Ok(DataRecord(91))).unwrap();
    runtime
        .connect(server_generation(1), 5, &source_rtd_key)
        .unwrap();

    let object = crate::value::with_excel_call_scope(|scope| {
        let source: Handle<'_, DataRecord> = runtime.lookup(scope, &source_token).unwrap();
        source.alias().into_locator()
    });

    let alias_key = test_topic_key("identity-alias");
    let alias_rtd_key = alias_key.format_rtd_key();
    let (alias_token, _) = runtime
        .prepare_observed_alias::<DataRecord, _>(alias_key, object, |_, _| Ok(()))
        .unwrap();
    runtime
        .connect(server_generation(1), 6, &alias_rtd_key)
        .unwrap();

    let other_key = test_topic_key("identity-other");
    let other_rtd_key = other_key.format_rtd_key();
    let (other_token, _) = runtime.prepare(other_key, || Ok(DataRecord(91))).unwrap();
    runtime
        .connect(server_generation(1), 7, &other_rtd_key)
        .unwrap();

    crate::value::with_excel_call_scope(|scope| {
        let source: Handle<'_, DataRecord> = runtime.lookup(scope, &source_token).unwrap();
        let alias: Handle<'_, DataRecord> = runtime.lookup(scope, &alias_token).unwrap();
        let other: Handle<'_, DataRecord> = runtime.lookup(scope, &other_token).unwrap();
        assert_eq!(source.object.id, alias.object.id);
        assert_eq!(input_identity(&source), input_identity(&alias));
        assert_eq!(
            input_identity(&source),
            reference_handle_identity(source.object.id.0.0)
        );
        assert_ne!(source.object.id, other.object.id);
        assert_ne!(input_identity(&source), input_identity(&other));
    });

    runtime.disconnect(server_generation(1), 5);
    runtime.disconnect(server_generation(1), 6);
    runtime.disconnect(server_generation(1), 7);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn semantic_handle_identity_controls_formula_memoization() {
    let runtime = HandleRuntime::new(16);

    let source_key = test_topic_key("semantic-memo-source");
    let source_token = runtime
        .prepare(source_key, || Ok(DataRecord(91)))
        .unwrap()
        .0;
    let source_rtd_key = source_key.format_rtd_key();
    runtime
        .connect(server_generation(1), 50, &source_rtd_key)
        .unwrap();

    let object = crate::value::with_excel_call_scope(|scope| {
        let source: Handle<'_, DataRecord> = runtime.lookup(scope, &source_token).unwrap();
        source.alias().into_locator()
    });

    let alias_key = test_topic_key("semantic-memo-alias");
    let alias_token = runtime
        .prepare_observed_alias::<DataRecord, _>(alias_key, object, |_, _| Ok(()))
        .unwrap()
        .0;
    let alias_rtd_key = alias_key.format_rtd_key();
    runtime
        .connect(server_generation(1), 51, &alias_rtd_key)
        .unwrap();

    let other_key = test_topic_key("semantic-memo-other");
    let other_token = runtime.prepare(other_key, || Ok(DataRecord(91))).unwrap().0;
    let other_rtd_key = other_key.format_rtd_key();
    runtime
        .connect(server_generation(1), 52, &other_rtd_key)
        .unwrap();

    let (source_revision, alias_revision, other_revision) =
        crate::value::with_excel_call_scope(|scope| {
            let source: Handle<'_, DataRecord> = runtime.lookup(scope, &source_token).unwrap();
            let alias: Handle<'_, DataRecord> = runtime.lookup(scope, &alias_token).unwrap();
            let other: Handle<'_, DataRecord> = runtime.lookup(scope, &other_token).unwrap();
            assert_eq!(source.object.id, alias.object.id);
            (
                semantic_handle_key(&source),
                semantic_handle_key(&alias),
                semantic_handle_key(&other),
            )
        });

    assert_eq!(source_revision, alias_revision);
    assert_ne!(source_revision, other_revision);

    let factory_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first_calls = Arc::clone(&factory_calls);
    let (token_a, created_a) = runtime
        .prepare_observed(
            source_revision,
            move || {
                first_calls.fetch_add(1, Ordering::SeqCst);
                Ok(DataRecord(700))
            },
            |_, _| Ok(()),
        )
        .unwrap();
    assert!(created_a);

    let second_calls = Arc::clone(&factory_calls);
    let (token_b, created_b) = runtime
        .prepare_observed(
            alias_revision,
            move || {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok(DataRecord(701))
            },
            |_, _| Ok(()),
        )
        .unwrap();
    assert!(!created_b);
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
    assert_eq!(token_a, token_b);
    let object_a =
        with_handle::<DataRecord, _>(&runtime, &token_a, |handle| handle.object.id).unwrap();
    let object_b =
        with_handle::<DataRecord, _>(&runtime, &token_b, |handle| handle.object.id).unwrap();
    assert_eq!(object_a, object_b);

    let third_calls = Arc::clone(&factory_calls);
    let (token_c, created_c) = runtime
        .prepare_observed(
            other_revision,
            move || {
                third_calls.fetch_add(1, Ordering::SeqCst);
                Ok(DataRecord(702))
            },
            |_, _| Ok(()),
        )
        .unwrap();
    assert!(created_c);
    assert_eq!(factory_calls.load(Ordering::SeqCst), 2);
    assert_ne!(token_a, token_c);
    let object_c =
        with_handle::<DataRecord, _>(&runtime, &token_c, |handle| handle.object.id).unwrap();
    assert_ne!(object_a, object_c);

    for (topic_id, rtd_key) in [
        (53, source_revision.format_rtd_key()),
        (55, other_revision.format_rtd_key()),
    ] {
        runtime
            .connect(server_generation(1), topic_id, &rtd_key)
            .unwrap();
    }

    for topic_id in [50, 51, 52, 53, 55] {
        runtime.disconnect(server_generation(1), topic_id);
    }
    assert_eq!(runtime.len(), 0);
}

#[test]
fn failed_rtd_connection_rolls_back_pending_object() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("pending");
    let rtd_key = key.format_rtd_key();
    runtime.prepare(key, || Ok(DataRecord(1))).unwrap();
    runtime.rollback(&rtd_key);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn server_generation_prevents_stale_rtd_ownership_after_claim_and_rollback() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("server-generation");
    let rtd_key = key.format_rtd_key();
    runtime.prepare(key, || Ok(DataRecord(1))).unwrap();

    runtime
        .claim_server(&rtd_key, server_generation(1))
        .unwrap();
    assert!(matches!(
        runtime.claim_server(&rtd_key, server_generation(2)),
        Err(XllError::InvalidHandle)
    ));
    assert!(matches!(
        runtime.connect(server_generation(2), 7, &rtd_key),
        Err(XllError::InvalidHandle)
    ));

    let provisional = runtime
        .connect_transaction(server_generation(1), 7, &rtd_key)
        .unwrap();
    drop(provisional);
    assert!(matches!(
        runtime.connect(server_generation(2), 7, &rtd_key),
        Err(XllError::InvalidHandle)
    ));

    runtime.connect(server_generation(1), 8, &rtd_key).unwrap();
    runtime.disconnect(server_generation(1), 8);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn uncalculated_rtd_connection_rolls_back_an_already_connected_topic() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("uncalculated");
    let rtd_key = key.format_rtd_key();
    runtime.prepare(key, || Ok(DataRecord(1))).unwrap();
    runtime.connect(server_generation(1), 9, &rtd_key).unwrap();
    runtime.rollback(&rtd_key);
    assert_eq!(runtime.len(), 0);
    runtime.disconnect(server_generation(1), 9);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn uncommitted_connect_transaction_rolls_back_only_the_excel_connection() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("transactional");
    let rtd_key = key.format_rtd_key();
    let (token, _) = runtime.prepare(key, || Ok(DataRecord(1))).unwrap();

    let connection = runtime
        .connect_transaction(server_generation(1), 10, &rtd_key)
        .unwrap();
    assert_eq!(connection.token(), token);
    drop(connection);

    assert_eq!(runtime.len(), 1);
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &token, |value| value.0).unwrap(),
        1
    );

    let retry = runtime
        .connect_transaction(server_generation(1), 10, &rtd_key)
        .unwrap();
    assert_eq!(retry.token(), token);
    retry.commit().unwrap();
    runtime.disconnect(server_generation(1), 10);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn concurrent_handle_connect_rejects_an_uncommitted_assignment() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("concurrent-transaction");
    let rtd_key = key.format_rtd_key();
    runtime.prepare(key, || Ok(DataRecord(3))).unwrap();

    let connection = runtime
        .connect_transaction(server_generation(1), 12, &rtd_key)
        .unwrap();
    assert!(matches!(
        runtime.connect_transaction(server_generation(1), 12, &rtd_key),
        Err(XllError::Overloaded)
    ));
    connection.commit().unwrap();

    let repeated = runtime
        .connect_transaction(server_generation(1), 12, &rtd_key)
        .unwrap();
    repeated.commit().unwrap();
    runtime.disconnect(server_generation(1), 12);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn failed_repeated_connect_transaction_preserves_existing_connection() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("existing-transaction");
    let rtd_key = key.format_rtd_key();
    let (token, _) = runtime.prepare(key, || Ok(DataRecord(2))).unwrap();
    runtime.connect(server_generation(1), 11, &rtd_key).unwrap();

    let connection = runtime
        .connect_transaction(server_generation(1), 11, &rtd_key)
        .unwrap();
    assert_eq!(connection.token(), token);
    drop(connection);

    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &token, |value| value.0).unwrap(),
        2
    );
    runtime.disconnect(server_generation(1), 11);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn excel_topic_id_cannot_be_connected_to_two_formula_topics() {
    let runtime = HandleRuntime::new(8);
    let first_key = test_topic_key("first");
    let first_rtd_key = first_key.format_rtd_key();
    let second_key = test_topic_key("second");
    let second_rtd_key = second_key.format_rtd_key();
    runtime.prepare(first_key, || Ok(DataRecord(1))).unwrap();
    runtime.prepare(second_key, || Ok(DataRecord(2))).unwrap();
    runtime
        .connect(server_generation(1), 9, &first_rtd_key)
        .unwrap();
    assert!(matches!(
        runtime.connect(server_generation(1), 9, &second_rtd_key),
        Err(XllError::InvalidHandle)
    ));
    runtime.disconnect(server_generation(1), 9);
    assert_eq!(runtime.len(), 1);
}

#[test]
fn handle_lease_keeps_payload_alive_after_binding_retirement() {
    let drops = Arc::new(AtomicUsize::new(0));
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("handle-lease-retirement");
    let (token, _) = runtime
        .prepare(key, || Ok(CountedDataRecord(Arc::clone(&drops))))
        .unwrap();

    let pinned: HandleLease<CountedDataRecord> = crate::value::with_excel_call_scope(|scope| {
        runtime
            .lookup::<CountedDataRecord>(scope, &token)
            .unwrap()
            .pin()
            .unwrap()
    });
    runtime
        .registry
        .remove_and_drop(&token, "test remove while pinned");

    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(pinned.0.load(Ordering::SeqCst), 0);
    drop(pinned);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn handle_lease_survives_terminal_runtime_close() {
    let drops = Arc::new(AtomicUsize::new(0));
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("handle-lease-close");
    let (token, _) = runtime
        .prepare(key, || Ok(CountedDataRecord(Arc::clone(&drops))))
        .unwrap();

    let pinned: HandleLease<CountedDataRecord> = crate::value::with_excel_call_scope(|scope| {
        runtime
            .lookup::<CountedDataRecord>(scope, &token)
            .unwrap()
            .pin()
            .unwrap()
    });
    let sealed = runtime.seal().unwrap();

    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(pinned.0.load(Ordering::SeqCst), 0);
    assert!(matches!(
        runtime.registry.finish_quiescence(&sealed),
        Err(XllError::Internal { diagnostic_id })
            if diagnostic_id == crate::error::DiagnosticId::HANDLE_PINS
    ));
    drop(pinned);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    runtime.registry.finish_quiescence(&sealed).unwrap();
}

#[test]
fn pin_promotion_resurrects_a_retired_payload_without_a_binding() {
    let drops = Arc::new(AtomicUsize::new(0));
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("handle-lease-resurrection");
    let (token, _) = runtime
        .prepare(key, || Ok(CountedDataRecord(Arc::clone(&drops))))
        .unwrap();

    let pinned: HandleLease<CountedDataRecord> = crate::value::with_excel_call_scope(|scope| {
        let handle = runtime.lookup::<CountedDataRecord>(scope, &token).unwrap();
        runtime
            .registry
            .remove_and_drop(&token, "test retire before pin promotion");
        handle.pin().unwrap()
    });

    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(pinned.0.load(Ordering::SeqCst), 0);
    drop(pinned);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

struct CountedDataRecord(Arc<std::sync::atomic::AtomicUsize>);

impl ExcelHandleObject for CountedDataRecord {}

impl Drop for CountedDataRecord {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn different_formula_keys_create_distinct_handles() {
    let runtime = HandleRuntime::new(8);
    let first_key = test_topic_key("sheet:A1:rate=1");
    let second_key = test_topic_key("sheet:A2:rate=1");
    let changed_key = test_topic_key("sheet:A1:rate=2");
    let (first, _) = runtime.prepare(first_key, || Ok(DataRecord(1))).unwrap();
    let (second, _) = runtime.prepare(second_key, || Ok(DataRecord(1))).unwrap();
    let (changed, _) = runtime.prepare(changed_key, || Ok(DataRecord(2))).unwrap();
    assert_ne!(first, second);
    assert_ne!(first, changed);
}

#[test]
fn disconnect_waits_for_an_in_flight_consumer_and_drops_once() {
    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("sheet:A1");
    let rtd_key = key.format_rtd_key();
    let (token, _) = runtime
        .prepare(key, || Ok(CountedDataRecord(Arc::clone(&drops))))
        .unwrap();
    runtime.connect(server_generation(1), 7, &rtd_key).unwrap();
    crate::value::with_excel_call_scope(|scope| {
        let consumer: Handle<'_, CountedDataRecord> = runtime.lookup(scope, &token).unwrap();
        runtime.disconnect(server_generation(1), 7);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert!(consumer.0.load(Ordering::Relaxed) == 0);
    });
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    runtime.disconnect(server_generation(1), 7);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn terminate_and_close_release_every_remaining_topic_once() {
    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime = HandleRuntime::new(8);
    for label in ["one", "two"] {
        let key = test_topic_key(label);
        let rtd_key = key.format_rtd_key();
        runtime
            .prepare(key, || Ok(CountedDataRecord(Arc::clone(&drops))))
            .unwrap();
        runtime
            .claim_server(&rtd_key, server_generation(1))
            .unwrap();
    }
    runtime.terminate_topics(server_generation(1));
    assert_eq!(drops.load(Ordering::Relaxed), 2);
    runtime.seal().map(|_| ()).unwrap();
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
fn panicking_factory_does_not_publish_a_topic() {
    let runtime = HandleRuntime::new(8);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = runtime
            .prepare::<DataRecord, _>(test_topic_key("panic"), || panic!("injected factory panic"));
    }));
    assert!(panic.is_err());
    assert_eq!(runtime.len(), 0);
}

#[test]
fn same_thread_factory_reentry_returns_an_error_without_waiting() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("factory-reentry");
    let (token, created) = runtime
        .prepare(key, || {
            let nested = runtime.prepare(key, || Ok(DataRecord(2)));
            assert!(matches!(nested, Err(XllError::ReentrantCall)));
            Ok(DataRecord(1))
        })
        .unwrap();
    assert!(created);
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &token, |value| value.0).unwrap(),
        1
    );
}

#[test]
fn different_key_factory_reentry_returns_an_error_without_waiting() {
    let runtime = HandleRuntime::new(8);
    let outer_key = test_topic_key("outer-factory");
    let inner_key = test_topic_key("inner-factory");
    let (token, created) = runtime
        .prepare(outer_key, || {
            let nested = runtime.prepare(inner_key, || Ok(DataRecord(2)));
            assert!(matches!(nested, Err(XllError::ReentrantCall)));
            Ok(DataRecord(1))
        })
        .unwrap();
    assert!(created);
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &token, |value| value.0).unwrap(),
        1
    );
    assert_eq!(runtime.len(), 1);
}

#[test]
fn same_thread_observer_reentry_returns_an_error_without_waiting() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("observer-reentry");
    let (token, created) = runtime
        .prepare_observed(
            key,
            || Ok(DataRecord(1)),
            |_, _| {
                let nested = runtime.prepare(key, || Ok(DataRecord(2)));
                assert!(matches!(nested, Err(XllError::ReentrantCall)));
                Ok(())
            },
        )
        .unwrap();
    assert!(created);
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &token, |value| value.0).unwrap(),
        1
    );
}

#[test]
fn different_key_observer_reentry_returns_an_error_without_waiting() {
    let runtime = HandleRuntime::new(8);
    let outer_key = test_topic_key("outer-observer");
    let inner_key = test_topic_key("inner-observer");
    let (token, created) = runtime
        .prepare_observed(
            outer_key,
            || Ok(DataRecord(1)),
            |_, _| {
                let nested = runtime.prepare(inner_key, || Ok(DataRecord(2)));
                assert!(matches!(nested, Err(XllError::ReentrantCall)));
                Ok(())
            },
        )
        .unwrap();
    assert!(created);
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &token, |value| value.0).unwrap(),
        1
    );
    assert_eq!(runtime.len(), 1);
}

#[test]
fn failed_observation_does_not_publish_a_topic_and_allows_retry() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("observed");
    let first = runtime.prepare_observed(
        key,
        || Ok(DataRecord(1)),
        |_, _| {
            Err(XllError::ExcelApi {
                function: crate::error::ExcelApiFunction::Rtd,
                failure: crate::error::ExcelApiFailure::Status(
                    crate::return_value::ExcelCallbackStatus::Failed(xlfn_sys::XLRET_FAILED),
                ),
            })
        },
    );
    assert!(matches!(first, Err(XllError::ExcelApi { .. })));
    assert_eq!(runtime.len(), 0);

    let (token, created) = runtime
        .prepare_observed(key, || Ok(DataRecord(2)), |_, _| Ok(()))
        .unwrap();
    assert!(created);
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &token, |value| value.0).unwrap(),
        2
    );
}

#[test]
fn cache_hit_observe_failure_does_not_invalidate_object() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("observed-memoized");
    let (token, created) = runtime
        .prepare_observed(key, || Ok(DataRecord(1)), |_, _| Ok(()))
        .unwrap();
    assert!(created);

    let calls = AtomicUsize::new(0);
    let result = runtime.prepare_observed(
        key,
        || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(DataRecord(2))
        },
        |_, _| {
            Err(XllError::ExcelApi {
                function: crate::error::ExcelApiFunction::Rtd,
                failure: crate::error::ExcelApiFailure::Status(
                    crate::return_value::ExcelCallbackStatus::Failed(xlfn_sys::XLRET_FAILED),
                ),
            })
        },
    );
    assert!(matches!(result, Err(XllError::ExcelApi { .. })));

    // factory was never invoked because cache hit skips it
    assert_eq!(calls.load(Ordering::Relaxed), 0);

    // original object is preserved
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &token, |value| value.0).unwrap(),
        1
    );
    assert_eq!(runtime.len(), 1);
}

#[test]
fn cache_hit_observe_failure_preserves_existing_topic() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("observe-retry");
    let (token, created) = runtime
        .prepare_observed(key, || Ok(DataRecord(10)), |_, _| Ok(()))
        .unwrap();
    assert!(created);

    // Observation failure on warm hit
    let result = runtime.prepare_observed(
        key,
        || Ok(DataRecord(20)),
        |_, _| {
            Err(XllError::ExcelApi {
                function: crate::error::ExcelApiFunction::Rtd,
                failure: crate::error::ExcelApiFailure::Status(
                    crate::return_value::ExcelCallbackStatus::Failed(xlfn_sys::XLRET_FAILED),
                ),
            })
        },
    );
    assert!(matches!(result, Err(XllError::ExcelApi { .. })));

    // Retry with successful observation still reuses the same object
    let (retry_token, created) = runtime
        .prepare_observed(key, || Ok(DataRecord(30)), |_, _| Ok(()))
        .unwrap();
    assert!(!created);
    assert_eq!(retry_token, token);
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &token, |value| value.0).unwrap(),
        10
    );
}

#[test]
fn observation_cannot_commit_a_topic_removed_reentrantly() {
    let runtime = HandleRuntime::new(8);
    let result = runtime.prepare_observed(
        test_topic_key("removed-during-observation"),
        || Ok(DataRecord(1)),
        |key, _| {
            runtime.rollback(key);
            Ok(())
        },
    );
    assert!(matches!(result, Err(XllError::StaleHandle)));
    assert_eq!(runtime.len(), 0);
}

#[test]
fn published_warm_observation_rejects_topic_removed_reentrantly() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("published-removed-during-observation");
    let (token, created) = runtime
        .prepare_observed(key, || Ok(DataRecord(1)), |_, _| Ok(()))
        .unwrap();
    assert!(created);

    let result = runtime.prepare_observed::<DataRecord, _>(
        key,
        || -> XllResult<DataRecord> { panic!("warm factory must not run") },
        |rtd_key, observed_token| {
            assert_eq!(observed_token, token);
            runtime.rollback(rtd_key);
            Ok(())
        },
    );

    assert!(matches!(result, Err(XllError::StaleHandle)));
    assert_eq!(runtime.len(), 0);
}

#[test]
fn warm_observation_rejects_generation_terminated_topic() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("warm-generation-terminated");
    let rtd_key = key.format_rtd_key();
    let (token, created) = runtime
        .prepare_observed(key, || Ok(DataRecord(1)), |_, _| Ok(()))
        .unwrap();
    assert!(created);

    runtime
        .claim_server(&rtd_key, server_generation(1))
        .unwrap();

    let observed_runtime = Arc::clone(&runtime);
    let result = runtime.prepare_observed::<DataRecord, _>(
        key,
        || -> XllResult<DataRecord> { panic!("warm factory must not run") },
        move |observed_rtd_key, observed_token| {
            assert_eq!(observed_rtd_key, rtd_key);
            assert_eq!(observed_token, token);
            observed_runtime.terminate_topics(server_generation(1));
            Ok(())
        },
    );

    assert!(matches!(result, Err(XllError::StaleHandle)));
    assert_eq!(runtime.len(), 0);
}

#[test]
fn warm_observation_does_not_follow_recreated_same_key() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("warm-same-key-aba");
    let rtd_key = key.format_rtd_key();
    let (old_token, created) = runtime
        .prepare_observed(key, || Ok(DataRecord(1)), |_, _| Ok(()))
        .unwrap();
    assert!(created);

    let (observation_started_tx, observation_started_rx) = std::sync::mpsc::sync_channel(0);
    let (replacement_ready_tx, replacement_ready_rx) = std::sync::mpsc::sync_channel(0);
    let replacement_runtime = Arc::clone(&runtime);
    let replacement_rtd_key = rtd_key.clone();
    let replacement_old_token = old_token.clone();
    let replacement = std::thread::spawn(move || {
        observation_started_rx.recv().unwrap();
        replacement_runtime.rollback(&replacement_rtd_key);
        let (new_token, created) = replacement_runtime
            .prepare(key, || Ok(DataRecord(2)))
            .unwrap();
        assert!(created);
        assert_ne!(new_token, replacement_old_token);
        replacement_ready_tx.send(new_token).unwrap();
    });

    let observed_runtime = Arc::clone(&runtime);
    let result = runtime.prepare_observed::<DataRecord, _>(
        key,
        || -> XllResult<DataRecord> { panic!("warm factory must not run") },
        move |observed_rtd_key, observed_token| {
            assert_eq!(observed_rtd_key, rtd_key);
            assert_eq!(observed_token, old_token);
            observation_started_tx.send(()).unwrap();
            let replacement_token = replacement_ready_rx.recv().unwrap();
            assert_ne!(replacement_token, observed_token);
            assert!(
                with_handle::<DataRecord, _>(&observed_runtime, &replacement_token, |_| (),)
                    .is_ok()
            );
            Ok(())
        },
    );

    replacement.join().unwrap();
    assert!(matches!(result, Err(XllError::StaleHandle)));
    assert_eq!(runtime.len(), 1);
}

#[test]
fn disconnect_can_remove_pending_formula_root_during_excel_connection() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let observed_runtime = Arc::clone(&runtime);
    let key = test_topic_key("disconnect-during-excel-connection");

    let result = runtime.prepare_observed(
        key,
        || Ok(DataRecord(1)),
        move |rtd_key, token| {
            let connection = observed_runtime
                .connect_transaction(server_generation(1), 17, rtd_key)
                .expect("ConnectData must be able to claim the visible topic");
            assert_eq!(connection.token(), token);

            // DisconnectData may enter while ConnectData still owns an
            // uncommitted connection transaction. The server operation gate
            // permits the two COM operations to overlap.
            let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
            let disconnect_runtime = Arc::clone(&observed_runtime);
            let disconnect = std::thread::spawn(move || {
                release_rx.recv().unwrap();
                disconnect_runtime.disconnect(server_generation(1), 17);
            });
            release_tx.send(()).unwrap();
            disconnect.join().unwrap();

            // DisconnectData removes the visible topic and registry root
            // without inspecting the connection commit bit.
            assert!(matches!(
                with_handle::<DataRecord, _>(&observed_runtime, token, |_| ()),
                Err(XllError::StaleHandle)
            ));

            // The connection guard observes that the topic was already
            // detached and therefore has no rollback work left to perform.
            drop(connection);
            Ok(())
        },
    );

    assert!(matches!(result, Err(XllError::StaleHandle)));
    assert_eq!(runtime.len(), 0);
}

#[test]
fn disconnect_rejects_provisional_excel_commit_without_resurrection() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let observed_runtime = Arc::clone(&runtime);
    let key = test_topic_key("disconnect-before-excel-commit");

    let result = runtime.prepare_observed(
        key,
        || Ok(DataRecord(1)),
        move |rtd_key, token| {
            let connection = observed_runtime
                .connect_transaction(server_generation(1), 17, rtd_key)
                .expect("ConnectData must be able to claim the visible topic");
            assert_eq!(connection.token(), token);

            // DisconnectData may detach the topic before ConnectData commits
            // its provisional Excel connection.
            observed_runtime.disconnect(server_generation(1), 17);

            // The commit must fail at the detached ownership boundary. Its
            // drop path must not recreate the topic or registry root.
            assert!(matches!(connection.commit(), Err(XllError::StaleHandle)));
            assert!(matches!(
                with_handle::<DataRecord, _>(&observed_runtime, token, |_| ()),
                Err(XllError::StaleHandle)
            ));
            Ok(())
        },
    );

    assert!(matches!(result, Err(XllError::StaleHandle)));
    assert_eq!(runtime.len(), 0);
}

#[test]
fn concurrent_waiter_retries_after_observation_failure() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let runtime = Arc::new(HandleRuntime::new(8));
    let (observing_tx, observing_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let first_runtime = Arc::clone(&runtime);
    let key = test_topic_key("concurrent-observe");
    let first = std::thread::spawn(move || {
        first_runtime.prepare_observed(
            key,
            || Ok(DataRecord(1)),
            |_, _| {
                observing_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Err(XllError::ExcelApi {
                    function: crate::error::ExcelApiFunction::Rtd,
                    failure: crate::error::ExcelApiFailure::Status(
                        crate::return_value::ExcelCallbackStatus::Failed(xlfn_sys::XLRET_FAILED),
                    ),
                })
            },
        )
    });
    observing_rx.recv().unwrap();

    let (waiting_tx, waiting_rx) = mpsc::channel();
    let second_runtime = Arc::clone(&runtime);
    let second = std::thread::spawn(move || {
        waiting_tx.send(()).unwrap();
        second_runtime.prepare_observed(key, || Ok(DataRecord(2)), |_, _| Ok(()))
    });
    waiting_rx.recv().unwrap();

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let waiter_is_blocked = {
            let topics = runtime.topics.read();
            topics
                .initializing
                .get(&test_topic_key("concurrent-observe"))
                .is_some_and(|initialization| Arc::strong_count(initialization) >= 2)
        };
        if waiter_is_blocked {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "second prepare did not wait for observation"
        );
        std::thread::yield_now();
    }

    finish_tx.send(()).unwrap();
    assert!(matches!(
        first.join().unwrap(),
        Err(XllError::ExcelApi { .. })
    ));
    let (token, created) = second.join().unwrap().unwrap();
    assert!(created);
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &token, |value| value.0).unwrap(),
        2
    );
}

#[test]
fn concurrent_prepare_with_same_key_runs_factory_once() {
    use std::sync::Barrier;
    use std::sync::mpsc;

    let runtime = Arc::new(HandleRuntime::new(8));
    let factory_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let (in_factory_tx, in_factory_rx) = mpsc::channel();
    let barrier = Arc::new(Barrier::new(2));
    let key = test_topic_key("concurrent_key");

    let runtime1 = Arc::clone(&runtime);
    let factory_calls1 = Arc::clone(&factory_calls);
    let barrier1 = Arc::clone(&barrier);

    let t1 = std::thread::spawn(move || {
        runtime1
            .prepare(key, || {
                factory_calls1.fetch_add(1, Ordering::SeqCst);
                in_factory_tx.send(()).unwrap();
                barrier1.wait();
                Ok(DataRecord(100))
            })
            .unwrap()
    });

    in_factory_rx.recv().unwrap();

    let runtime2 = Arc::clone(&runtime);
    let factory_calls2 = Arc::clone(&factory_calls);
    let t2 = std::thread::spawn(move || {
        runtime2
            .prepare(key, || {
                factory_calls2.fetch_add(1, Ordering::SeqCst);
                Ok(DataRecord(200))
            })
            .unwrap()
    });

    barrier.wait();

    let res1 = t1.join().unwrap();
    let res2 = t2.join().unwrap();

    // Under memoization, thread 1 creates the topic. Thread 2 waits for
    // thread 1 to finish, then finds the existing topic and reuses it.
    // The factory is invoked exactly once.
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
    assert_eq!(res1.0, res2.0);
    assert!(!res2.1);
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &res1.0, |value| value.0).unwrap(),
        100
    );
    assert_eq!(runtime.len(), 1);
}

#[test]
fn handle_dependency_chain_propagates_identity_change() {
    let runtime = HandleRuntime::new(16);

    // Upstream: different semantic input fingerprint → different revision key
    // → different token.
    let upstream_a_key = test_topic_key("sheet:A1:CURVE.CREATE:digest_a");
    let (upstream_a, created) = runtime
        .prepare(upstream_a_key, || Ok(DataRecord(10)))
        .unwrap();
    assert!(created);

    // Downstream uses the converted upstream Handle as part of its key,
    // simulating MODEL.CREATE(Handle<'_, Curve>, params). The ObjectId is the
    // semantic identity, so a different upstream object yields a different
    // downstream revision key even when the source values are equal.
    let downstream_key_a =
        with_handle::<DataRecord, _>(&runtime, &upstream_a, |handle| semantic_handle_key(&handle))
            .unwrap();
    let (downstream_a, created) = runtime
        .prepare(downstream_key_a, || Ok(DataRecord(100)))
        .unwrap();
    assert!(created);

    // Upstream changes (different arguments → different key)
    let upstream_b_key = test_topic_key("sheet:A1:CURVE.CREATE:digest_b");
    let (upstream_b, created) = runtime
        .prepare(upstream_b_key, || Ok(DataRecord(20)))
        .unwrap();
    assert!(created);
    assert_ne!(upstream_a, upstream_b);

    // Downstream key also changes because the upstream ObjectId changed.
    let downstream_key_b =
        with_handle::<DataRecord, _>(&runtime, &upstream_b, |handle| semantic_handle_key(&handle))
            .unwrap();
    let (downstream_b, created) = runtime
        .prepare(downstream_key_b, || Ok(DataRecord(200)))
        .unwrap();
    assert!(created);
    assert_ne!(downstream_a, downstream_b);

    // Both downstream objects are distinct
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &downstream_a, |value| value.0).unwrap(),
        100
    );
    assert_eq!(
        with_handle::<DataRecord, _>(&runtime, &downstream_b, |value| value.0).unwrap(),
        200
    );
}

#[test]
fn close_wakes_waiter_and_prevents_creator_from_publishing() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("closing");
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (factory_started_tx, factory_started_rx) = mpsc::channel();
    let (release_factory_tx, release_factory_rx) = mpsc::channel();

    let creator_runtime = Arc::clone(&runtime);
    let creator_observed = Arc::clone(&observed);
    let creator = std::thread::spawn(move || {
        creator_runtime.prepare_observed(
            key,
            || {
                factory_started_tx.send(()).unwrap();
                release_factory_rx.recv().unwrap();
                Ok(DataRecord(1))
            },
            |_, _| {
                creator_observed.store(true, Ordering::Release);
                Ok(())
            },
        )
    });
    factory_started_rx.recv().unwrap();

    let waiter_runtime = Arc::clone(&runtime);
    let (waiter_done_tx, waiter_done_rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let result = waiter_runtime.prepare(key, || Ok(DataRecord(2)));
        waiter_done_tx.send(result).unwrap();
    });

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let blocked = runtime
            .topics
            .read()
            .initializing
            .get(&key)
            .is_some_and(|initialization| Arc::strong_count(initialization) >= 4);
        if blocked {
            break;
        }
        assert!(Instant::now() < deadline, "waiter did not block");
        std::thread::yield_now();
    }

    let close_runtime = Arc::clone(&runtime);
    let closer = std::thread::spawn(move || close_runtime.seal().map(|_| ()));
    let deadline = Instant::now() + Duration::from_secs(1);
    while !runtime.topics.read().closed {
        assert!(
            Instant::now() < deadline,
            "close did not mark runtime closed"
        );
        std::thread::yield_now();
    }
    assert!(matches!(
        waiter_done_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(XllError::Closing)
    ));
    release_factory_tx.send(()).unwrap();
    assert!(matches!(creator.join().unwrap(), Err(XllError::Closing)));
    closer.join().unwrap().unwrap();
    waiter.join().unwrap();
    assert!(!observed.load(Ordering::Acquire));
    assert_eq!(runtime.len(), 0);
}

#[test]
fn warm_hit_does_not_enter_single_flight_initialization() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("warm-fast");

    let (token, created) = runtime
        .prepare_observed(key, || Ok(DataRecord(1)), |_, _| Ok(()))
        .unwrap();

    assert!(created);

    let calls = AtomicUsize::new(0);

    let (second, created) = runtime
        .prepare_observed(
            key,
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(DataRecord(2))
            },
            |key, _| {
                let topics = runtime.topics.read();
                let identity = topics
                    .by_rtd_key
                    .get(key)
                    .copied()
                    .expect("warm observation must use a published RTD key");

                assert!(
                    !topics.initializing.contains_key(&identity),
                    "warm hit must bypass per-key single-flight state",
                );

                Ok(())
            },
        )
        .unwrap();

    assert!(!created);
    assert_eq!(token, second);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn close_waits_for_in_flight_warm_observation_before_closing_registry() {
    use std::sync::mpsc;
    use std::time::Duration;

    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("warm-close");

    runtime
        .prepare_observed(key, || Ok(DataRecord(1)), |_, _| Ok(()))
        .unwrap();

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let warm_runtime = Arc::clone(&runtime);
    let warm = std::thread::spawn(move || {
        warm_runtime.prepare_observed::<DataRecord, _>(
            key,
            || panic!("warm factory must not run"),
            |_, _| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            },
        )
    });

    entered_rx.recv().unwrap();

    let closing_runtime = Arc::clone(&runtime);
    let (closed_tx, closed_rx) = mpsc::channel();

    let closer = std::thread::spawn(move || {
        closed_tx.send(closing_runtime.seal().map(|_| ())).unwrap();
    });

    while !runtime.topics.read().closed {
        std::thread::yield_now();
    }

    //
    // close has started, but registry must remain alive while observe executes.
    //
    assert_eq!(runtime.registry.phase(), HandleRegistryPhase::Closing);

    assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());

    release_tx.send(()).unwrap();

    assert!(matches!(warm.join().unwrap(), Err(XllError::Closing)));

    closed_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();

    closer.join().unwrap();

    assert_eq!(runtime.registry.phase(), HandleRegistryPhase::Closed);
}

#[test]
fn handle_type_mismatch_returns_invalid_handle() {
    #[derive(Debug, PartialEq, Clone)]
    struct TypeA(u32);
    impl ExcelHandleObject for TypeA {}

    #[derive(Debug, PartialEq, Clone)]
    struct TypeB(u32);
    impl ExcelHandleObject for TypeB {}

    let runtime = HandleRuntime::new(16);
    let key = test_topic_key("type_mismatch");
    let (token, _) = runtime
        .prepare_observed(key, || Ok(TypeA(42)), |_, _| Ok(()))
        .unwrap();

    // Looking up TypeA as TypeB must fail with InvalidHandle
    crate::value::with_excel_call_scope(|scope| {
        let result = runtime.lookup::<TypeB>(scope, &token);
        assert!(matches!(result, Err(XllError::InvalidHandle)));
    });

    // Looking up TypeA as TypeA must succeed
    crate::value::with_excel_call_scope(|scope| {
        let handle = runtime.lookup::<TypeA>(scope, &token).unwrap();
        assert_eq!(*handle, TypeA(42));
    });
}

#[test]
fn alias_preserves_pointer_and_object_identity() {
    #[derive(Debug, PartialEq)]
    struct TrackedObj(u64);
    impl ExcelHandleObject for TrackedObj {}

    let runtime = HandleRuntime::new(16);
    let key1 = test_topic_key("alias_identity_1");
    let (token1, _) = runtime
        .prepare_observed(key1, || Ok(TrackedObj(12345)), |_, _| Ok(()))
        .unwrap();

    let (token2, object_id1, ptr1) = crate::value::with_excel_call_scope(|scope| {
        let handle1 = runtime.lookup::<TrackedObj>(scope, &token1).unwrap();
        let object = handle1.object;
        let ptr = handle1.value.address();
        let alias = handle1.alias();
        let key2 = test_topic_key("alias_identity_2");
        let (token2, _) = runtime
            .prepare_observed_alias::<TrackedObj, _>(key2, alias.object, |_, _| Ok(()))
            .unwrap();
        (token2, object.id, ptr)
    });

    assert_ne!(token1, token2);

    crate::value::with_excel_call_scope(|scope| {
        let handle2 = runtime.lookup::<TrackedObj>(scope, &token2).unwrap();
        assert_eq!(handle2.object.id, object_id1);
        assert_eq!(handle2.value.address(), ptr1);
        assert_eq!(*handle2, TrackedObj(12345));
    });
}

#[test]
fn removing_original_binding_keeps_aliased_object_alive() {
    let drops = Arc::new(AtomicUsize::new(0));

    struct DropCounter {
        _value: u64,
        drops: Arc<AtomicUsize>,
    }
    impl ExcelHandleObject for DropCounter {}
    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    let runtime = HandleRuntime::new(16);
    let key1 = test_topic_key("retire_alias_1");
    let (token1, _) = runtime
        .prepare_observed(
            key1,
            || {
                Ok(DropCounter {
                    _value: 999,
                    drops: Arc::clone(&drops),
                })
            },
            |_, _| Ok(()),
        )
        .unwrap();

    let key2 = test_topic_key("retire_alias_2");
    let token2 = crate::value::with_excel_call_scope(|scope| {
        let handle1 = runtime.lookup::<DropCounter>(scope, &token1).unwrap();
        let alias = handle1.alias();
        let (token2, _) = runtime
            .prepare_observed_alias::<DropCounter, _>(key2, alias.object, |_, _| Ok(()))
            .unwrap();
        token2
    });

    // Remove token1 binding
    runtime.registry.remove_and_drop(&token1, "test remove 1");
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    // Reading token2 must still work and access the same value
    crate::value::with_excel_call_scope(|scope| {
        let handle2 = runtime.lookup::<DropCounter>(scope, &token2).unwrap();
        assert_eq!(handle2._value, 999);
    });

    // Remove token2 binding -> last reference dropped
    runtime.registry.remove_and_drop(&token2, "test remove 2");
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn call_borrow_keeps_value_alive_across_binding_retirement() {
    let drops = Arc::new(AtomicUsize::new(0));

    struct DropCounter {
        val: u32,
        drops: Arc<AtomicUsize>,
    }
    impl ExcelHandleObject for DropCounter {}
    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    let runtime = HandleRuntime::new(16);
    let key = test_topic_key("call_borrow_alive");
    let (token, _) = runtime
        .prepare_observed(
            key,
            || {
                Ok(DropCounter {
                    val: 777,
                    drops: Arc::clone(&drops),
                })
            },
            |_, _| Ok(()),
        )
        .unwrap();

    crate::value::with_excel_call_scope(|scope| {
        let handle = runtime.lookup::<DropCounter>(scope, &token).unwrap();
        assert_eq!(handle.val, 777);

        // Retire the binding while the call guard is still in scope.
        runtime.registry.remove_and_drop(&token, "test remove");

        // The epoch guard, not the publication snapshot, keeps the object alive.
        assert_eq!(drops.load(Ordering::SeqCst), 0);

        // Dereference remains safe
        assert_eq!(handle.val, 777);
    });

    // After CallScope ends and handle is dropped, the object is dropped
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn registry_close_drops_each_handle_exactly_once() {
    let drops = Arc::new(AtomicUsize::new(0));

    struct TrackedItem {
        drops: Arc<AtomicUsize>,
    }
    impl ExcelHandleObject for TrackedItem {}
    impl Drop for TrackedItem {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    let runtime = HandleRuntime::new(16);
    for i in 0..5 {
        let key = test_topic_key(&format!("close_drop_{i}"));
        let d = Arc::clone(&drops);
        let _ = runtime
            .prepare_observed(key, || Ok(TrackedItem { drops: d }), |_, _| Ok(()))
            .unwrap();
    }

    assert_eq!(drops.load(Ordering::SeqCst), 0);
    runtime.seal().map(|_| ()).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 5);
}

#[test]
fn drop_panic_is_recorded_in_handle_cleanup_state() {
    struct PanickingDrop;
    impl ExcelHandleObject for PanickingDrop {}
    impl Drop for PanickingDrop {
        fn drop(&mut self) {
            panic!("intended destructor panic in test");
        }
    }

    let runtime = HandleRuntime::new(16);
    let key = test_topic_key("drop_panic_test");
    let (token, _) = runtime
        .prepare_observed(key, || Ok(PanickingDrop), |_, _| Ok(()))
        .unwrap();

    // Removing and dropping the object should catch the panic and record it
    runtime
        .registry
        .remove_and_drop(&token, "test panic remove");

    assert!(matches!(
        runtime.registry.cleanup_result(),
        Err(XllError::Panic)
    ));
}

#[test]
fn zero_sized_type_handle_lifecycle() {
    #[derive(Debug, PartialEq, Eq)]
    struct ZeroSized;
    impl ExcelHandleObject for ZeroSized {}

    let runtime = HandleRuntime::new(16);
    let key1 = test_topic_key("zst_test_1");
    let (token1, _) = runtime
        .prepare_observed(key1, || Ok(ZeroSized), |_, _| Ok(()))
        .unwrap();

    let (token2, object_id) = crate::value::with_excel_call_scope(|scope| {
        let handle1 = runtime.lookup::<ZeroSized>(scope, &token1).unwrap();
        assert_eq!(*handle1, ZeroSized);
        let alias = handle1.alias();
        let key2 = test_topic_key("zst_test_2");
        let (token2, _) = runtime
            .prepare_observed_alias::<ZeroSized, _>(key2, alias.object, |_, _| Ok(()))
            .unwrap();
        (token2, alias.object.id)
    });

    crate::value::with_excel_call_scope(|scope| {
        let handle2 = runtime.lookup::<ZeroSized>(scope, &token2).unwrap();
        assert_eq!(*handle2, ZeroSized);
        assert_eq!(handle2.object.id, object_id);
    });

    runtime.registry.remove_and_drop(&token1, "remove zst 1");
    crate::value::with_excel_call_scope(|scope| {
        let handle2 = runtime.lookup::<ZeroSized>(scope, &token2).unwrap();
        assert_eq!(*handle2, ZeroSized);
    });

    runtime.registry.remove_and_drop(&token2, "remove zst 2");
    runtime.seal().map(|_| ()).unwrap();
}

#[test]
fn alias_capability_does_not_extend_object_lifetime() {
    let drops = Arc::new(AtomicUsize::new(0));

    struct TrackedValue {
        _id: u32,
        drops: Arc<AtomicUsize>,
    }
    impl ExcelHandleObject for TrackedValue {}
    impl Drop for TrackedValue {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    let runtime = HandleRuntime::new(16);
    let key = test_topic_key("alias_alone_alive");
    let (token, _) = runtime
        .prepare_observed(
            key,
            || {
                Ok(TrackedValue {
                    _id: 42,
                    drops: Arc::clone(&drops),
                })
            },
            |_, _| Ok(()),
        )
        .unwrap();

    crate::value::with_excel_call_scope(|scope| {
        let handle = runtime.lookup::<TrackedValue>(scope, &token).unwrap();
        let alias = handle.alias();

        // Remove the original binding from registry
        runtime.registry.remove_and_drop(&token, "remove original");

        // The call epoch keeps the retired object readable until the scope
        // ends, but the borrowed alias is not an ownership extension.
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(alias.object.id.0.0, 1);
    });
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn resolver_does_not_initialize_slot_when_unused() {
    let slot = HandleRuntimeSlot::new();
    assert!(slot.is_none());

    let resolver = HandleRuntimeResolver::new(&slot);
    assert!(slot.is_none());
    drop(resolver);
    assert!(slot.is_none());
}

#[test]
fn resolver_keeps_one_runtime_read_guard_across_arguments_and_return_context() {
    let _callback_guard = crate::test_callback::lock();
    crate::test_callback::install();
    crate::test_callback::reset();

    struct TestObj(u32);
    impl ExcelHandleObject for TestObj {}

    let slot: &'static HandleRuntimeSlot = Box::leak(Box::new(HandleRuntimeSlot::new()));
    assert!(slot.is_none());

    slot.arm(generation(1), crate::HandleConfig::default())
        .unwrap();
    let handle_rt = slot.get_owned().unwrap();
    let key = test_topic_key("resolver_test");
    let (token, _) = handle_rt
        .prepare_observed(key, || Ok(TestObj(123)), |_, _| Ok(()))
        .unwrap();

    crate::value::with_excel_call_scope(|scope| {
        let resolver = HandleRuntimeResolver::new(slot);
        let mut call_ctx = crate::value::CallContext::from_access(scope, Some(resolver));

        // First handle resolution initializes resolver OnceCell
        let h1: Handle<'_, TestObj> = call_ctx.resolve_handle(&token).unwrap();
        assert_eq!(h1.0, 123);

        // Second handle resolution reuses OnceCell
        let h2: Handle<'_, TestObj> = call_ctx.resolve_handle(&token).unwrap();
        assert_eq!(h2.0, 123);

        // Take resolver and move to ReturnContext
        let moved_access = call_ctx.take_handle_access();
        assert!(call_ctx.take_handle_access().is_none());

        let res_ref = &moved_access.as_ref().unwrap().runtime;
        assert!(std::ptr::eq(res_ref.get().unwrap(), &*handle_rt));
        assert!(std::ptr::eq(&**res_ref.get_arc().unwrap(), &*handle_rt));

        let mut return_ctx = ReturnContext::for_frame(
            moved_access.expect("test context has handle access"),
            "test_udf",
            Some([0; 32]),
        );
        let err = return_ctx
            .publish_new_handle(|| Ok(TestObj(456)))
            .unwrap_err();
        assert!(matches!(err, XllError::ExcelApi { .. }));
    });
}

#[test]
fn concurrent_first_use_initializes_exactly_once() {
    let slot: &'static HandleRuntimeSlot = Box::leak(Box::new(HandleRuntimeSlot::new()));
    assert!(slot.is_none());
    slot.arm(generation(1), crate::HandleConfig::default())
        .unwrap();

    let barrier = Arc::new(std::sync::Barrier::new(16));
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let read = slot.read().unwrap();
                (&*read as *const HandleRuntime).addr()
            })
        })
        .collect();

    let mut ptrs = Vec::new();
    for handle in handles {
        ptrs.push(handle.join().unwrap());
    }
    assert_ne!(ptrs[0], 0);
    for ptr in &ptrs {
        assert_eq!(*ptr, ptrs[0]);
    }
    assert!(!slot.is_none());
}

#[test]
fn close_resets_to_closed_for_reopen() {
    let slot = HandleRuntimeSlot::new();
    assert!(slot.is_none());

    slot.arm(generation(1), crate::HandleConfig::default())
        .unwrap();
    let rt1 = slot.get_owned().unwrap();
    assert!(!slot.is_none());

    slot.seal(Some(generation(1))).map(|_| ()).unwrap();
    assert!(slot.is_none());

    slot.arm(generation(2), crate::HandleConfig::default())
        .unwrap();
    let rt2 = slot.get_owned().unwrap();
    assert!(!slot.is_none());

    assert!(!Arc::ptr_eq(&rt1, &rt2));
}

#[test]
fn handle_slot_requires_matching_generation_for_seal() {
    let slot = HandleRuntimeSlot::new();
    assert!(matches!(slot.read(), Err(XllError::Closing)));

    slot.arm(generation(7), crate::HandleConfig::default())
        .unwrap();
    assert!(matches!(
        slot.seal(Some(generation(6))),
        Err(XllError::Closing)
    ));
    assert!(slot.get_owned().is_ok());
    assert!(matches!(slot.disarm(generation(6)), Err(XllError::Closing)));

    slot.seal(Some(generation(7))).unwrap();
    assert!(slot.is_none());
}

#[test]
fn handle_config_rejects_an_unbounded_dense_publication_table() {
    let slot = HandleRuntimeSlot::new();
    let invalid_limit =
        crate::HandleBindingLimit::try_from(crate::HandleConfig::MAX_SUPPORTED_BINDINGS + 1);

    assert!(invalid_limit.is_err());
    assert!(slot.is_none());
}
