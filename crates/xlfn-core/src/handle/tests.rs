use super::*;

fn format_formula_topic_key(
    caller: FormulaCaller,
    udf_id: &'static str,
    argument_digest: &[u8; 32],
) -> String {
    FormulaTopicKey::new(caller, udf_id, argument_digest).format_rtd_key()
}

fn insert_production<T>(registry: &HandleRegistry, value: Arc<T>) -> XllResult<String>
where
    T: Any + Send + Sync + 'static,
{
    let mut value = Some(value);
    registry.insert_pending(&mut value)
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
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
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
        let actual = format_formula_topic_key(
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
fn formula_topic_key_uses_the_stable_sheet_identifier() {
    let digest = [0xab_u8; 32];
    let caller = FormulaCaller {
        sheet_id: 17,
        row: 4,
        column: 8,
    };
    let first = format_formula_topic_key(caller, "TEST.CREATE", &digest);
    let recalculated = format_formula_topic_key(caller, "TEST.CREATE", &digest);
    let other_sheet = format_formula_topic_key(
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
fn formula_topic_key_changes_with_every_identity_component() {
    let digest = [0x12_u8; 32];
    let caller = FormulaCaller {
        sheet_id: 17,
        row: 4,
        column: 8,
    };
    let base = format_formula_topic_key(caller, "TEST.CREATE", &digest);

    assert_ne!(
        base,
        format_formula_topic_key(
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
        format_formula_topic_key(FormulaCaller { row: 5, ..caller }, "TEST.CREATE", &digest,)
    );
    assert_ne!(
        base,
        format_formula_topic_key(
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
        format_formula_topic_key(caller, "TEST.OTHER", &digest)
    );
    assert_ne!(
        base,
        format_formula_topic_key(caller, "TEST.CREATE", &[0x13_u8; 32])
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
            || Ok(Arc::new(DataRecord(7))),
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
    assert_eq!(topic.rtd_key.as_ref(), rtd_key.as_str());
    assert_eq!(topic.token, token);
    assert!(topics.by_excel_id.is_empty());
    drop(topics);

    let published = runtime.published.load(&key);
    let publication = published
        .get(&key)
        .expect("successful observation must commit its published snapshot");
    assert_eq!(
        publication.state.load(Ordering::Acquire),
        PublishedTopicState::Live as u8
    );

    runtime.connect(1, 41, &rtd_key).unwrap();
    {
        let topics = runtime.topics.read();
        assert_eq!(topics.by_excel_id.len(), 1);
        assert_eq!(topics.by_excel_id.values().next(), Some(&identity));
    }

    runtime.disconnect(1, 41);
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
            || Ok(Arc::new(DataRecord(1))),
            |_, _| {
                let published = runtime.published.load(&key);
                assert!(published.get(&key).is_none());
                Ok(())
            },
        )
        .unwrap();

    assert!(runtime.published.load(&key).get(&key).is_some());
}

#[test]
fn publication_rejects_rtd_key_collision_without_overwriting_existing_topic() {
    let runtime = HandleRuntime::new(8);
    let first_key = test_topic_key("collision-first");
    let second_key = test_topic_key("collision-second");
    let first_rtd_key = first_key.format_rtd_key();
    let second_rtd_key = second_key.format_rtd_key();
    assert_ne!(first_rtd_key, second_rtd_key);

    let (first_token, created) = runtime
        .prepare(first_key, || Ok(Arc::new(DataRecord(1))))
        .unwrap();
    assert!(created);

    // Force the state that a formatter collision would present at the
    // publication boundary: the existing topic owns the incoming RTD key.
    {
        let mut topics = runtime.topics.write();
        let topic = topics
            .by_key
            .get_mut(&first_key)
            .expect("the first topic must be published");
        topic.rtd_key = Arc::from(second_rtd_key.as_str());
        topics.by_rtd_key.remove(first_rtd_key.as_str());
        topics
            .by_rtd_key
            .insert(Arc::from(second_rtd_key.as_str()), first_key);
    }

    let result = runtime.prepare(second_key, || Ok(Arc::new(DataRecord(2))));
    assert!(matches!(
        result,
        Err(XllError::Internal {
            diagnostic_id: HANDLE_TOPIC_RTD_KEY_COLLISION_DIAGNOSTIC_ID
        })
    ));

    {
        let topics = runtime.topics.read();
        assert_eq!(topics.by_key.len(), 1);
        assert_eq!(
            topics.by_key.get(&first_key).map(|topic| &topic.token),
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
        let topic = topics.by_key.get_mut(&first_key).unwrap();
        topic.rtd_key = Arc::from(first_rtd_key.as_str());
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
    let registry = HandleRegistry::new(4);
    let first = Arc::new(String::from("first"));
    let token = insert_production(&registry, Arc::clone(&first)).unwrap();
    let borrowed = registry.lookup::<String>(&token).unwrap();
    assert_eq!(&*borrowed, "first");

    let removed = registry.remove::<String>(&token).unwrap();
    assert_eq!(&*removed, "first");
    assert!(matches!(
        registry.lookup::<String>(&token),
        Err(XllError::StaleHandle)
    ));

    let replacement = insert_production(&registry, Arc::new(String::from("replacement"))).unwrap();
    assert_ne!(token, replacement);
    assert_eq!(&*borrowed, "first");
}

#[test]
fn published_handle_index_is_weak_and_generation_scoped() {
    let registry = HandleRegistry::new(4);
    let first = Arc::new(String::from("first"));
    let first_weak = Arc::downgrade(&first);
    let token = insert_production(&registry, Arc::clone(&first)).unwrap();
    let parsed = registry.parse_token(&token).unwrap();
    let first_snapshot = registry.published.load(parsed.slot);
    let first_publication = first_snapshot
        .get(&parsed.slot)
        .expect("inserted handle must be published");

    assert_eq!(first_publication.generation, parsed.generation);
    assert_eq!(first_publication.state(), PublishedHandleState::Live);
    drop(first);
    assert!(first_weak.upgrade().is_some());

    let removed = registry.remove::<String>(&token).unwrap();
    assert_eq!(first_publication.state(), PublishedHandleState::Stale);
    drop(removed);
    assert!(first_weak.upgrade().is_none());
    assert!(first_publication.upgrade().is_none());

    let replacement = insert_production(&registry, Arc::new(String::from("replacement"))).unwrap();
    let replacement_parsed = registry.parse_token(&replacement).unwrap();
    let replacement_snapshot = registry.published.load(replacement_parsed.slot);
    let replacement_publication = replacement_snapshot
        .get(&replacement_parsed.slot)
        .expect("reused handle must be republished");

    assert_eq!(replacement_parsed.slot, parsed.slot);
    assert_ne!(replacement_parsed.generation, parsed.generation);
    assert!(!Arc::ptr_eq(&first_publication, &replacement_publication));
    assert_eq!(replacement_publication.state(), PublishedHandleState::Live);
}

#[test]
fn published_handle_index_does_not_extend_values_through_close() {
    let registry = HandleRegistry::new(2);
    let value = Arc::new(42_u32);
    let value_weak = Arc::downgrade(&value);
    let token = insert_production(&registry, Arc::clone(&value)).unwrap();
    let parsed = registry.parse_token(&token).unwrap();
    let publication_snapshot = registry.published.load(parsed.slot);
    let publication = publication_snapshot
        .get(&parsed.slot)
        .expect("inserted handle must be published");

    drop(value);
    registry.close().unwrap();

    assert_eq!(publication.state(), PublishedHandleState::Closing);
    assert!(
        registry
            .published
            .load(parsed.slot)
            .get(&parsed.slot)
            .is_none()
    );
    assert!(value_weak.upgrade().is_none());
    assert!(publication.upgrade().is_none());
}

#[test]
fn published_handle_reused_slot_retains_stale_publication_arc() {
    struct TestObj(&'static str);
    impl ExcelHandleObject for TestObj {}

    let registry = HandleRegistry::new(4);
    let leases = Arc::new(HandleLeaseState::new());

    let token1 = insert_production(&registry, Arc::new(TestObj("first"))).unwrap();
    let parsed1 = registry.parse_token(&token1).unwrap();
    let retained_old_snapshot = registry.published.load(parsed1.slot);
    let retained_old_publication = retained_old_snapshot
        .get(&parsed1.slot)
        .expect("first handle must be published");

    // Remove first handle: slot is now vacant with next generation, retained_old_publication is Stale.
    let removed = registry.remove::<TestObj>(&token1).unwrap();
    assert_eq!(removed.0, "first");
    assert_eq!(
        retained_old_publication.state(),
        PublishedHandleState::Stale
    );
    drop(removed);

    // Insert replacement in the same slot (generation + 1).
    let token2 = insert_production(&registry, Arc::new(TestObj("second"))).unwrap();
    let parsed2 = registry.parse_token(&token2).unwrap();
    assert_eq!(parsed1.slot, parsed2.slot);
    assert_ne!(parsed1.generation, parsed2.generation);

    let replacement_snapshot = registry.published.load(parsed2.slot);
    let replacement_publication = replacement_snapshot
        .get(&parsed2.slot)
        .expect("reused handle must be published");
    assert_eq!(replacement_publication.state(), PublishedHandleState::Live);
    assert_eq!(replacement_publication.generation, parsed2.generation);

    // The retained old publication remains Stale and its Weak is dead; it did NOT update to the replacement.
    assert_eq!(
        retained_old_publication.state(),
        PublishedHandleState::Stale
    );
    assert_eq!(retained_old_publication.generation, parsed1.generation);
    assert!(!Arc::ptr_eq(
        &retained_old_publication,
        &replacement_publication
    ));
    assert!(retained_old_publication.upgrade().is_none());

    // Fast lookup on stale token1 is rejected as StaleHandle by generation check
    let stale_lookup = registry.lookup_handle::<TestObj>(&token1, &leases);
    assert!(matches!(stale_lookup, Err(XllError::StaleHandle)));

    // Fast lookup on new token2 succeeds
    let fresh_handle = registry.lookup_handle::<TestObj>(&token2, &leases).unwrap();
    assert_eq!(fresh_handle.0, "second");
    assert_eq!(leases.active(), 1);
    drop(fresh_handle);
    assert_eq!(leases.active(), 0);
}

#[test]
fn published_handle_remove_during_lease_acquire_linearizes_as_stale() {
    use std::sync::mpsc;

    struct TestObj(&'static str);
    impl ExcelHandleObject for TestObj {}

    let registry = Arc::new(HandleRegistry::new(4));
    let leases = Arc::new(HandleLeaseState::new());

    let token = insert_production(&registry, Arc::new(TestObj("target"))).unwrap();
    let parsed = registry.parse_token(&token).unwrap();
    let publication_snapshot = registry.published.load(parsed.slot);
    let publication = publication_snapshot
        .get(&parsed.slot)
        .expect("handle must be published");
    assert_eq!(publication.state(), PublishedHandleState::Live);

    let (lease_acquired_tx, lease_acquired_rx) = mpsc::sync_channel(0);
    let release_reader = Arc::new(AtomicBool::new(false));
    let release_reader_hook = Arc::clone(&release_reader);
    *leases.after_acquire_hook.lock() = Some(Arc::new(move || {
        lease_acquired_tx
            .send(())
            .expect("reader must reach the lease-acquired gate");
        while !release_reader_hook.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }));

    let reader_registry = Arc::clone(&registry);
    let reader_leases = Arc::clone(&leases);
    let reader_token = token.clone();
    let reader = std::thread::spawn(move || {
        reader_registry.lookup_handle::<TestObj>(&reader_token, &reader_leases)
    });

    // The reader has passed its first Live check and acquired the lease, but
    // has not performed the second Live check yet.
    lease_acquired_rx
        .recv()
        .expect("reader must acquire its tentative lease");
    assert_eq!(leases.active(), 1);

    // Remove wins before the reader's second state check.
    let removed = registry.remove::<TestObj>(&token).unwrap();
    assert_eq!(removed.0, "target");
    assert_eq!(publication.state(), PublishedHandleState::Stale);

    release_reader.store(true, Ordering::Release);
    let lookup = reader.join().unwrap();
    assert!(matches!(lookup, Err(XllError::StaleHandle)));
    assert_eq!(leases.active(), 0);
}

#[test]
fn published_handle_remove_before_weak_upgrade_falls_back_to_stale() {
    use std::sync::mpsc;

    struct TestObj;
    impl ExcelHandleObject for TestObj {}

    let registry = Arc::new(HandleRegistry::new(4));
    let leases = Arc::new(HandleLeaseState::new());

    let token = insert_production(&registry, Arc::new(TestObj)).unwrap();
    let parsed = registry.parse_token(&token).unwrap();

    let publication_snapshot = registry.published.load(parsed.slot);
    let publication = publication_snapshot
        .get(&parsed.slot)
        .expect("handle must be published");
    let (validated_tx, validated_rx) = mpsc::sync_channel(0);
    let release_reader = Arc::new(AtomicBool::new(false));
    let release_reader_hook = Arc::clone(&release_reader);
    *registry.before_fast_upgrade_hook.lock() = Some(Arc::new(move || {
        validated_tx
            .send(())
            .expect("reader must reach the weak-upgrade gate");
        while !release_reader_hook.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }));

    let reader_registry = Arc::clone(&registry);
    let reader_leases = Arc::clone(&leases);
    let reader_token = token.clone();
    let reader = std::thread::spawn(move || {
        reader_registry.lookup_handle::<TestObj>(&reader_token, &reader_leases)
    });

    // The reader has passed the second Live check and holds a validated lease,
    // but has not upgraded the weak snapshot yet.
    validated_rx
        .recv()
        .expect("reader must reach the weak-upgrade gate");
    assert_eq!(leases.active(), 1);

    let removed = registry.remove::<TestObj>(&token).unwrap();
    drop(removed);
    assert!(publication.upgrade().is_none());
    assert_eq!(publication.state(), PublishedHandleState::Stale);

    release_reader.store(true, Ordering::Release);
    let fallback = reader.join().unwrap();
    assert!(matches!(fallback, Err(XllError::StaleHandle)));
    assert_eq!(leases.active(), 0);
}

#[test]
fn published_handle_close_after_first_live_rejects_admission_and_returns_closing() {
    use std::sync::mpsc;

    struct TestObj;
    impl ExcelHandleObject for TestObj {}

    let registry = Arc::new(HandleRegistry::new(4));
    let leases = Arc::new(HandleLeaseState::new());

    let token = insert_production(&registry, Arc::new(TestObj)).unwrap();
    let (first_live_tx, first_live_rx) = mpsc::sync_channel(0);
    let release_reader = Arc::new(AtomicBool::new(false));
    let release_reader_hook = Arc::clone(&release_reader);

    *registry.before_fast_lease_acquire_hook.lock() = Some(Arc::new(move || {
        first_live_tx
            .send(())
            .expect("reader must reach the pre-acquire gate");
        while !release_reader_hook.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }));

    let reader_registry = Arc::clone(&registry);
    let reader_leases = Arc::clone(&leases);
    let reader_token = token.clone();
    let reader = std::thread::spawn(move || {
        reader_registry.lookup_handle::<TestObj>(&reader_token, &reader_leases)
    });

    // The reader has passed its first Live check but has not acquired a lease yet.
    first_live_rx
        .recv()
        .expect("reader must reach the pre-acquire gate");
    assert_eq!(leases.active(), 0);

    // Close runs completely, seals lease admission, drops canonical values, and returns.
    registry.close_with_leases(&leases).unwrap();
    assert_eq!(leases.active(), 0);

    // Reader resumes and attempts leases.acquire(), which returns None because admission is sealed.
    release_reader.store(true, Ordering::Release);
    let lookup = reader.join().unwrap();
    assert!(matches!(lookup, Err(XllError::Closing)));
    assert_eq!(leases.active(), 0);
}

#[test]
fn exhausted_generation_retires_the_slot_permanently() {
    let registry = HandleRegistry::new(2);
    insert_production(&registry, Arc::new(1_u32)).unwrap();
    registry.state.write().slots[0].generation = u64::MAX;
    let final_token = registry.format_token(0, u64::MAX);
    assert_eq!(*registry.remove::<u32>(&final_token).unwrap(), 1);
    assert!(registry.state.read().free.is_empty());

    let replacement = insert_production(&registry, Arc::new(2_u32)).unwrap();
    assert_eq!(registry.parse_token(&replacement).unwrap().slot, 1);
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
            diagnostic_id: HANDLE_ENTROPY_DIAGNOSTIC_ID
        }
    ));
}

#[test]
fn close_invalidates_tokens_but_existing_arcs_survive() {
    let registry = HandleRegistry::new(2);
    let token = insert_production(&registry, Arc::new(42_u32)).unwrap();
    let value = registry.lookup::<u32>(&token).unwrap();
    registry.close().unwrap();
    assert!(registry.lookup::<u32>(&token).is_err());
    assert_eq!(*value, 42);
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
            registry.close().unwrap();
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
    assert_eq!(*registry.lookup::<u32>(&token).unwrap(), 42);
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
    registry.close().unwrap();
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

    assert!(matches!(registry.close(), Err(XllError::Panic)));
    assert_eq!(registry.len(), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn escaped_handle_destructor_panic_poisons_terminal_close() {
    struct PanicOnDrop;
    impl ExcelHandleObject for PanicOnDrop {}
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("injected escaped handle destructor panic");
        }
    }

    let runtime = HandleRuntime::new(2);
    let key = test_topic_key("escaped-panic");
    let rtd_key = key.format_rtd_key();
    let (token, _) = runtime.prepare(key, || Ok(Arc::new(PanicOnDrop))).unwrap();
    let escaped = runtime.lookup::<PanicOnDrop>(&token).unwrap();

    // Remove the formula-owned registry root first. The escaped Handle now
    // owns the final Arc and must contain its destructor panic itself.
    runtime.rollback(&rtd_key);
    drop(escaped);

    assert!(matches!(runtime.close(), Err(XllError::Panic)));
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

unsafe fn convert_with_context<S, T>(
    runtime: &crate::Runtime<S>,
    argument: &'static str,
    raw: *mut xlfn_sys::XLOPER12,
) -> XllResult<T>
where
    T: for<'call> crate::FromExcel<'call>,
{
    crate::with_excel_call_scope(|scope| {
        // SAFETY: the test caller keeps the raw value and nested payload live.
        // SAFETY: forwarded from this helper's caller.
        unsafe { crate::argument_from_raw_with_context(scope, runtime, argument, raw) }
    })
}

#[test]
fn repeated_formula_identity_runs_factory_exactly_once() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("same");
    let rtd_key = key.format_rtd_key();
    let calls = AtomicUsize::new(0);

    let (first, created) = runtime
        .prepare(key, || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(DataRecord(1)))
        })
        .unwrap();
    assert!(created);

    let (second, created) = runtime
        .prepare(key, || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(DataRecord(2)))
        })
        .unwrap();
    assert!(!created);
    assert_eq!(first, second);
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    assert_eq!(runtime.lookup::<DataRecord>(&first).unwrap().0, 1);

    runtime.connect(1, 41, &rtd_key).unwrap();
    runtime.disconnect(1, 41);
    assert_eq!(runtime.len(), 0);
    assert!(matches!(
        runtime.lookup::<DataRecord>(&first),
        Err(XllError::StaleHandle)
    ));
}

#[test]
fn explicit_handle_argument_conversion_resolves_a_typed_token() {
    let runtime: crate::Runtime<()> = crate::Runtime::new();
    let handles = runtime.handles().unwrap();
    let (token, _) = handles
        .prepare(test_topic_key("argument"), || Ok(Arc::new(DataRecord(19))))
        .unwrap();
    let (_encoded, mut raw) = token_value(&token);

    type DataRecordHandle = Handle<DataRecord>;
    // SAFETY: `raw` and its counted UTF-16 storage remain live for conversion.
    let resolved: DataRecordHandle =
        unsafe { convert_with_context(&runtime, "dataset", &mut raw) }.unwrap();
    assert_eq!(resolved.0, 19);
}

#[test]
fn generic_handle_conversion_rejects_wrong_stale_foreign_and_tampered_tokens() {
    let runtime: crate::Runtime<()> = crate::Runtime::new();
    let handles = runtime.handles().unwrap();
    let key = test_topic_key("argument-errors");
    let rtd_key = key.format_rtd_key();
    let (token, _) = handles
        .prepare(key, || Ok(Arc::new(DataRecord(23))))
        .unwrap();
    handles.connect(1, 91, &rtd_key).unwrap();

    let (_wrong_encoded, mut wrong_raw) = token_value(&token);
    // SAFETY: `wrong_raw` and its counted UTF-16 storage remain live for conversion.
    let wrong = unsafe {
        convert_with_context::<_, Handle<SimpleResource>>(&runtime, "curve", &mut wrong_raw)
    };
    assert!(matches!(wrong, Err(XllError::InvalidHandle)));

    let foreign_runtime: crate::Runtime<()> = crate::Runtime::new();
    let (_foreign_encoded, mut foreign_raw) = token_value(&token);
    // SAFETY: `foreign_raw` and its counted UTF-16 storage remain live for conversion.
    let foreign = unsafe {
        convert_with_context::<_, Handle<DataRecord>>(&foreign_runtime, "dataset", &mut foreign_raw)
    };
    assert!(matches!(foreign, Err(XllError::InvalidHandle)));

    let mut tampered = token.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == '0' { '1' } else { '0' });
    let (_tampered_encoded, mut tampered_raw) = token_value(&tampered);
    // SAFETY: `tampered_raw` and its counted UTF-16 storage remain live for conversion.
    let tampered = unsafe {
        convert_with_context::<_, Handle<DataRecord>>(&runtime, "dataset", &mut tampered_raw)
    };
    assert!(matches!(tampered, Err(XllError::InvalidHandle)));

    handles.disconnect(1, 91);
    let (_stale_encoded, mut stale_raw) = token_value(&token);
    // SAFETY: `stale_raw` and its counted UTF-16 storage remain live for conversion.
    let stale = unsafe {
        convert_with_context::<_, Handle<DataRecord>>(&runtime, "dataset", &mut stale_raw)
    };
    assert!(matches!(stale, Err(XllError::StaleHandle)));
}

#[test]
fn optional_handle_conversion_preserves_blank_and_missing_policy() {
    let runtime: crate::Runtime<()> = crate::Runtime::new();
    let mut blank = xlfn_sys::XLOPER12::nil();
    let mut missing = xlfn_sys::XLOPER12::missing();
    // SAFETY: `blank` remains live for the duration of conversion.
    let blank_value = unsafe {
        convert_with_context::<_, Option<Handle<DataRecord>>>(&runtime, "dataset", &mut blank)
    }
    .unwrap();
    // SAFETY: `missing` remains live for the duration of conversion.
    let missing_value = unsafe {
        convert_with_context::<_, Option<Handle<DataRecord>>>(&runtime, "dataset", &mut missing)
    }
    .unwrap();
    assert!(blank_value.is_none());
    assert!(missing_value.is_none());

    // SAFETY: `blank` remains live for the duration of conversion.
    let direct_blank =
        unsafe { convert_with_context::<_, Handle<DataRecord>>(&runtime, "dataset", &mut blank) };
    assert!(direct_blank.is_err());
}

#[test]
fn existing_handle_publication_creates_an_independent_formula_owner() {
    let runtime = HandleRuntime::new(8);
    let shared = Arc::new(DataRecord(31));
    let source_key = test_topic_key("source");
    let source_rtd_key = source_key.format_rtd_key();
    let (source_token, _) = runtime
        .prepare(source_key, || Ok(Arc::clone(&shared)))
        .unwrap();
    runtime.connect(1, 1, &source_rtd_key).unwrap();

    let resolved = runtime.lookup::<DataRecord>(&source_token).unwrap();
    let alias_key = test_topic_key("alias");
    let alias_rtd_key = alias_key.format_rtd_key();
    let (alias_token, _) = runtime
        .prepare(alias_key, || Ok(resolved.into_arc()))
        .unwrap();
    runtime.connect(1, 2, &alias_rtd_key).unwrap();
    assert_ne!(source_token, alias_token);

    runtime.disconnect(1, 1);
    assert!(matches!(
        runtime.lookup::<DataRecord>(&source_token),
        Err(XllError::StaleHandle)
    ));
    assert_eq!(runtime.lookup::<DataRecord>(&alias_token).unwrap().0, 31);

    runtime.disconnect(1, 2);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn failed_rtd_connection_rolls_back_pending_object() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("pending");
    let rtd_key = key.format_rtd_key();
    runtime
        .prepare(key, || Ok(Arc::new(DataRecord(1))))
        .unwrap();
    runtime.rollback(&rtd_key);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn server_generation_prevents_stale_rtd_ownership_after_claim_and_rollback() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("server-generation");
    let rtd_key = key.format_rtd_key();
    runtime
        .prepare(key, || Ok(Arc::new(DataRecord(1))))
        .unwrap();

    runtime.claim_server(&rtd_key, 1).unwrap();
    assert!(matches!(
        runtime.claim_server(&rtd_key, 2),
        Err(XllError::InvalidHandle)
    ));
    assert!(matches!(
        runtime.connect(2, 7, &rtd_key),
        Err(XllError::InvalidHandle)
    ));

    let provisional = runtime.connect_transaction(1, 7, &rtd_key).unwrap();
    drop(provisional);
    assert!(matches!(
        runtime.connect(2, 7, &rtd_key),
        Err(XllError::InvalidHandle)
    ));

    runtime.connect(1, 8, &rtd_key).unwrap();
    runtime.disconnect(1, 8);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn uncalculated_rtd_connection_rolls_back_an_already_connected_topic() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("uncalculated");
    let rtd_key = key.format_rtd_key();
    runtime
        .prepare(key, || Ok(Arc::new(DataRecord(1))))
        .unwrap();
    runtime.connect(1, 9, &rtd_key).unwrap();
    runtime.rollback(&rtd_key);
    assert_eq!(runtime.len(), 0);
    runtime.disconnect(1, 9);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn uncommitted_connect_transaction_rolls_back_only_the_excel_connection() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("transactional");
    let rtd_key = key.format_rtd_key();
    let (token, _) = runtime
        .prepare(key, || Ok(Arc::new(DataRecord(1))))
        .unwrap();

    let connection = runtime.connect_transaction(1, 10, &rtd_key).unwrap();
    assert_eq!(connection.token(), token);
    drop(connection);

    assert_eq!(runtime.len(), 1);
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);

    let retry = runtime.connect_transaction(1, 10, &rtd_key).unwrap();
    assert_eq!(retry.token(), token);
    retry.commit().unwrap();
    runtime.disconnect(1, 10);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn concurrent_handle_connect_rejects_an_uncommitted_assignment() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("concurrent-transaction");
    let rtd_key = key.format_rtd_key();
    runtime
        .prepare(key, || Ok(Arc::new(DataRecord(3))))
        .unwrap();

    let connection = runtime.connect_transaction(1, 12, &rtd_key).unwrap();
    assert!(matches!(
        runtime.connect_transaction(1, 12, &rtd_key),
        Err(XllError::Overloaded)
    ));
    connection.commit().unwrap();

    let repeated = runtime.connect_transaction(1, 12, &rtd_key).unwrap();
    repeated.commit().unwrap();
    runtime.disconnect(1, 12);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn failed_repeated_connect_transaction_preserves_existing_connection() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("existing-transaction");
    let rtd_key = key.format_rtd_key();
    let (token, _) = runtime
        .prepare(key, || Ok(Arc::new(DataRecord(2))))
        .unwrap();
    runtime.connect(1, 11, &rtd_key).unwrap();

    let connection = runtime.connect_transaction(1, 11, &rtd_key).unwrap();
    assert_eq!(connection.token(), token);
    drop(connection);

    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 2);
    runtime.disconnect(1, 11);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn excel_topic_id_cannot_be_connected_to_two_formula_topics() {
    let runtime = HandleRuntime::new(8);
    let first_key = test_topic_key("first");
    let first_rtd_key = first_key.format_rtd_key();
    let second_key = test_topic_key("second");
    let second_rtd_key = second_key.format_rtd_key();
    runtime
        .prepare(first_key, || Ok(Arc::new(DataRecord(1))))
        .unwrap();
    runtime
        .prepare(second_key, || Ok(Arc::new(DataRecord(2))))
        .unwrap();
    runtime.connect(1, 9, &first_rtd_key).unwrap();
    assert!(matches!(
        runtime.connect(1, 9, &second_rtd_key),
        Err(XllError::InvalidHandle)
    ));
    runtime.disconnect(1, 9);
    assert_eq!(runtime.len(), 1);
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
    let (first, _) = runtime
        .prepare(first_key, || Ok(Arc::new(DataRecord(1))))
        .unwrap();
    let (second, _) = runtime
        .prepare(second_key, || Ok(Arc::new(DataRecord(1))))
        .unwrap();
    let (changed, _) = runtime
        .prepare(changed_key, || Ok(Arc::new(DataRecord(2))))
        .unwrap();
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
        .prepare(key, || Ok(Arc::new(CountedDataRecord(Arc::clone(&drops)))))
        .unwrap();
    runtime.connect(1, 7, &rtd_key).unwrap();
    let consumer = runtime.lookup::<CountedDataRecord>(&token).unwrap();
    runtime.disconnect(1, 7);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(consumer);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    runtime.disconnect(1, 7);
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
            .prepare(key, || Ok(Arc::new(CountedDataRecord(Arc::clone(&drops)))))
            .unwrap();
        runtime.claim_server(&rtd_key, 1).unwrap();
    }
    runtime.terminate_topics(1);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
    runtime.close().unwrap();
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
            let nested = runtime.prepare(key, || Ok(Arc::new(DataRecord(2))));
            assert!(matches!(nested, Err(XllError::ReentrantCall)));
            Ok(Arc::new(DataRecord(1)))
        })
        .unwrap();
    assert!(created);
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);
}

#[test]
fn different_key_factory_reentry_returns_an_error_without_waiting() {
    let runtime = HandleRuntime::new(8);
    let outer_key = test_topic_key("outer-factory");
    let inner_key = test_topic_key("inner-factory");
    let (token, created) = runtime
        .prepare(outer_key, || {
            let nested = runtime.prepare(inner_key, || Ok(Arc::new(DataRecord(2))));
            assert!(matches!(nested, Err(XllError::ReentrantCall)));
            Ok(Arc::new(DataRecord(1)))
        })
        .unwrap();
    assert!(created);
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);
    assert_eq!(runtime.len(), 1);
}

#[test]
fn same_thread_observer_reentry_returns_an_error_without_waiting() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("observer-reentry");
    let (token, created) = runtime
        .prepare_observed(
            key,
            || Ok(Arc::new(DataRecord(1))),
            |_, _| {
                let nested = runtime.prepare(key, || Ok(Arc::new(DataRecord(2))));
                assert!(matches!(nested, Err(XllError::ReentrantCall)));
                Ok(())
            },
        )
        .unwrap();
    assert!(created);
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);
}

#[test]
fn different_key_observer_reentry_returns_an_error_without_waiting() {
    let runtime = HandleRuntime::new(8);
    let outer_key = test_topic_key("outer-observer");
    let inner_key = test_topic_key("inner-observer");
    let (token, created) = runtime
        .prepare_observed(
            outer_key,
            || Ok(Arc::new(DataRecord(1))),
            |_, _| {
                let nested = runtime.prepare(inner_key, || Ok(Arc::new(DataRecord(2))));
                assert!(matches!(nested, Err(XllError::ReentrantCall)));
                Ok(())
            },
        )
        .unwrap();
    assert!(created);
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);
    assert_eq!(runtime.len(), 1);
}

#[test]
fn failed_observation_does_not_publish_a_topic_and_allows_retry() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("observed");
    let first = runtime.prepare_observed(
        key,
        || Ok(Arc::new(DataRecord(1))),
        |_, _| {
            Err(XllError::ExcelApi {
                function: "xlfRtd",
                code: xlfn_sys::XLRET_FAILED,
            })
        },
    );
    assert!(matches!(first, Err(XllError::ExcelApi { .. })));
    assert_eq!(runtime.len(), 0);

    let (token, created) = runtime
        .prepare_observed(key, || Ok(Arc::new(DataRecord(2))), |_, _| Ok(()))
        .unwrap();
    assert!(created);
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 2);
}

#[test]
fn cache_hit_observe_failure_does_not_invalidate_object() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("observed-memoized");
    let (token, created) = runtime
        .prepare_observed(key, || Ok(Arc::new(DataRecord(1))), |_, _| Ok(()))
        .unwrap();
    assert!(created);

    let calls = AtomicUsize::new(0);
    let result = runtime.prepare_observed(
        key,
        || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(DataRecord(2)))
        },
        |_, _| {
            Err(XllError::ExcelApi {
                function: "xlfRtd",
                code: xlfn_sys::XLRET_FAILED,
            })
        },
    );
    assert!(matches!(result, Err(XllError::ExcelApi { .. })));

    // factory was never invoked because cache hit skips it
    assert_eq!(calls.load(Ordering::Relaxed), 0);

    // original object is preserved
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);
    assert_eq!(runtime.len(), 1);
}

#[test]
fn cache_hit_observe_failure_preserves_existing_topic() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("observe-retry");
    let (token, created) = runtime
        .prepare_observed(key, || Ok(Arc::new(DataRecord(10))), |_, _| Ok(()))
        .unwrap();
    assert!(created);

    // Observation failure on warm hit
    let result = runtime.prepare_observed(
        key,
        || Ok(Arc::new(DataRecord(20))),
        |_, _| {
            Err(XllError::ExcelApi {
                function: "xlfRtd",
                code: xlfn_sys::XLRET_FAILED,
            })
        },
    );
    assert!(matches!(result, Err(XllError::ExcelApi { .. })));

    // Retry with successful observation still reuses the same object
    let (retry_token, created) = runtime
        .prepare_observed(key, || Ok(Arc::new(DataRecord(30))), |_, _| Ok(()))
        .unwrap();
    assert!(!created);
    assert_eq!(retry_token, token);
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 10);
}

#[test]
fn observation_cannot_commit_a_topic_removed_reentrantly() {
    let runtime = HandleRuntime::new(8);
    let result = runtime.prepare_observed(
        test_topic_key("removed-during-observation"),
        || Ok(Arc::new(DataRecord(1))),
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
        .prepare_observed(key, || Ok(Arc::new(DataRecord(1))), |_, _| Ok(()))
        .unwrap();
    assert!(created);

    let result = runtime.prepare_observed::<DataRecord, _>(
        key,
        || -> XllResult<Arc<DataRecord>> { panic!("warm factory must not run") },
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
        .prepare_observed(key, || Ok(Arc::new(DataRecord(1))), |_, _| Ok(()))
        .unwrap();
    assert!(created);

    runtime.claim_server(&rtd_key, 1).unwrap();

    let observed_runtime = Arc::clone(&runtime);
    let result = runtime.prepare_observed::<DataRecord, _>(
        key,
        || -> XllResult<Arc<DataRecord>> { panic!("warm factory must not run") },
        move |observed_rtd_key, observed_token| {
            assert_eq!(observed_rtd_key, rtd_key);
            assert_eq!(observed_token, token);
            observed_runtime.terminate_topics(1);
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
        .prepare_observed(key, || Ok(Arc::new(DataRecord(1))), |_, _| Ok(()))
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
            .prepare(key, || Ok(Arc::new(DataRecord(2))))
            .unwrap();
        assert!(created);
        assert_ne!(new_token, replacement_old_token);
        replacement_ready_tx.send(new_token).unwrap();
    });

    let observed_runtime = Arc::clone(&runtime);
    let result = runtime.prepare_observed::<DataRecord, _>(
        key,
        || -> XllResult<Arc<DataRecord>> { panic!("warm factory must not run") },
        move |observed_rtd_key, observed_token| {
            assert_eq!(observed_rtd_key, rtd_key);
            assert_eq!(observed_token, old_token);
            observation_started_tx.send(()).unwrap();
            let replacement_token = replacement_ready_rx.recv().unwrap();
            assert_ne!(replacement_token, observed_token);
            assert!(
                observed_runtime
                    .lookup::<DataRecord>(&replacement_token)
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
        || Ok(Arc::new(DataRecord(1))),
        move |rtd_key, token| {
            let connection = observed_runtime
                .connect_transaction(1, 17, rtd_key)
                .expect("ConnectData must be able to claim the visible topic");
            assert_eq!(connection.token(), token);

            // DisconnectData may enter while ConnectData still owns an
            // uncommitted connection transaction. The server operation gate
            // permits the two COM operations to overlap.
            let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
            let disconnect_runtime = Arc::clone(&observed_runtime);
            let disconnect = std::thread::spawn(move || {
                release_rx.recv().unwrap();
                disconnect_runtime.disconnect(1, 17);
            });
            release_tx.send(()).unwrap();
            disconnect.join().unwrap();

            // DisconnectData removes the visible topic and registry root
            // without inspecting the connection commit bit.
            assert!(matches!(
                observed_runtime.lookup::<DataRecord>(token),
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
        || Ok(Arc::new(DataRecord(1))),
        move |rtd_key, token| {
            let connection = observed_runtime
                .connect_transaction(1, 17, rtd_key)
                .expect("ConnectData must be able to claim the visible topic");
            assert_eq!(connection.token(), token);

            // DisconnectData may detach the topic before ConnectData commits
            // its provisional Excel connection.
            observed_runtime.disconnect(1, 17);

            // The commit must fail at the detached ownership boundary. Its
            // drop path must not recreate the topic or registry root.
            assert!(matches!(connection.commit(), Err(XllError::StaleHandle)));
            assert!(matches!(
                observed_runtime.lookup::<DataRecord>(token),
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
            || Ok(Arc::new(DataRecord(1))),
            |_, _| {
                observing_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Err(XllError::ExcelApi {
                    function: "xlfRtd",
                    code: xlfn_sys::XLRET_FAILED,
                })
            },
        )
    });
    observing_rx.recv().unwrap();

    let (waiting_tx, waiting_rx) = mpsc::channel();
    let second_runtime = Arc::clone(&runtime);
    let second = std::thread::spawn(move || {
        waiting_tx.send(()).unwrap();
        second_runtime.prepare_observed(key, || Ok(Arc::new(DataRecord(2))), |_, _| Ok(()))
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
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 2);
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
                Ok(Arc::new(DataRecord(100)))
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
                Ok(Arc::new(DataRecord(200)))
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
    assert_eq!(runtime.lookup::<DataRecord>(&res1.0).unwrap().0, 100);
    assert_eq!(runtime.len(), 1);
}

#[test]
fn handle_dependency_chain_propagates_identity_change() {
    let runtime = HandleRuntime::new(16);

    // Upstream: different argument digest → different key → different token
    let upstream_a_key = test_topic_key("sheet:A1:CURVE.CREATE:digest_a");
    let (upstream_a, created) = runtime
        .prepare(upstream_a_key, || Ok(Arc::new(DataRecord(10))))
        .unwrap();
    assert!(created);

    // Downstream uses upstream token as part of its key, simulating
    // MODEL.CREATE(Handle<Curve>, params). The raw upstream token becomes
    // part of the argument digest, so a different upstream token yields
    // a different downstream key.
    let downstream_label_a = format!("sheet:B1:MODEL.CREATE:{}:params", upstream_a);
    let downstream_key_a = test_topic_key(&downstream_label_a);
    let (downstream_a, created) = runtime
        .prepare(downstream_key_a, || Ok(Arc::new(DataRecord(100))))
        .unwrap();
    assert!(created);

    // Upstream changes (different arguments → different key)
    let upstream_b_key = test_topic_key("sheet:A1:CURVE.CREATE:digest_b");
    let (upstream_b, created) = runtime
        .prepare(upstream_b_key, || Ok(Arc::new(DataRecord(20))))
        .unwrap();
    assert!(created);
    assert_ne!(upstream_a, upstream_b);

    // Downstream key also changes because the upstream token changed
    let downstream_label_b = format!("sheet:B1:MODEL.CREATE:{}:params", upstream_b);
    let downstream_key_b = test_topic_key(&downstream_label_b);
    let (downstream_b, created) = runtime
        .prepare(downstream_key_b, || Ok(Arc::new(DataRecord(200))))
        .unwrap();
    assert!(created);
    assert_ne!(downstream_a, downstream_b);

    // Both downstream objects are distinct
    assert_eq!(runtime.lookup::<DataRecord>(&downstream_a).unwrap().0, 100);
    assert_eq!(runtime.lookup::<DataRecord>(&downstream_b).unwrap().0, 200);
}

#[test]
fn close_waits_for_all_escaped_handle_leases() {
    use std::sync::mpsc;
    use std::time::Duration;

    let runtime = Arc::new(HandleRuntime::new(8));
    let key = test_topic_key("leased");
    let rtd_key = key.format_rtd_key();
    let (token, _) = runtime
        .prepare(key, || Ok(Arc::new(DataRecord(41))))
        .unwrap();
    runtime.connect(1, 1, &rtd_key).unwrap();

    let first = runtime.lookup::<DataRecord>(&token).unwrap();
    let second = first.clone();
    assert_eq!(runtime.leases.active(), 2);

    let closing_runtime = Arc::clone(&runtime);
    let (closed_tx, closed_rx) = mpsc::sync_channel(1);
    let closer = std::thread::spawn(move || {
        closed_tx.send(closing_runtime.close()).unwrap();
    });

    while !runtime.registry.state.read().closed {
        std::thread::yield_now();
    }
    assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());

    drop(first);
    assert_eq!(runtime.leases.active(), 1);
    assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());

    drop(second);
    closed_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    closer.join().unwrap();
    assert_eq!(runtime.leases.active(), 0);
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
                Ok(Arc::new(DataRecord(1)))
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
        let result = waiter_runtime.prepare(key, || Ok(Arc::new(DataRecord(2))));
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
    let closer = std::thread::spawn(move || close_runtime.close());
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
fn nested_handle_in_registry_does_not_deadlock_on_close() {
    struct InnerObj;
    impl ExcelHandleObject for InnerObj {}

    struct OuterObj {
        _inner: Handle<InnerObj>,
    }
    impl ExcelHandleObject for OuterObj {}

    let runtime = Arc::new(HandleRuntime::new(16));
    let (inner_token, _) = runtime
        .prepare(test_topic_key("inner"), || Ok(Arc::new(InnerObj)))
        .unwrap();
    let inner_handle = runtime.lookup::<InnerObj>(&inner_token).unwrap();

    let (outer_token, _) = runtime
        .prepare(test_topic_key("outer"), move || {
            Ok(Arc::new(OuterObj {
                _inner: inner_handle,
            }))
        })
        .unwrap();
    let outer_handle = runtime.lookup::<OuterObj>(&outer_token).unwrap();

    assert_eq!(runtime.leases.active(), 2);
    drop(outer_handle);
    assert_eq!(runtime.leases.active(), 1);

    runtime.registry.close_with_leases(&runtime.leases).unwrap();
    assert_eq!(runtime.leases.active(), 0);
}

#[test]
fn handle_lease_admission_rejects_new_acquisition_after_seal() {
    let leases = Arc::new(HandleLeaseState::new());
    let existing = leases.acquire().expect("lease admission is open");

    leases.seal();

    assert!(leases.acquire().is_none());
    assert_eq!(leases.active(), 1);

    drop(existing);
    assert_eq!(leases.active(), 0);
}

#[test]
fn handle_lease_clone_remains_admitted_after_seal() {
    let leases = Arc::new(HandleLeaseState::new());
    let existing = leases.acquire().expect("lease admission is open");

    leases.seal();

    let clone = existing.clone();
    assert_eq!(leases.active(), 2);

    drop(existing);
    assert_eq!(leases.active(), 1);
    drop(clone);
    assert_eq!(leases.active(), 0);
}

#[test]
fn handle_lease_acquire_race_with_seal_drains_without_late_active_lease() {
    use std::sync::Barrier;

    for _ in 0..64 {
        let leases = Arc::new(HandleLeaseState::new());
        let barrier = Arc::new(Barrier::new(2));
        let worker_leases = Arc::clone(&leases);
        let worker_barrier = Arc::clone(&barrier);

        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            drop(worker_leases.acquire());
        });

        barrier.wait();
        leases.seal();
        worker.join().unwrap();

        assert_eq!(leases.active(), 0);
        assert!(leases.acquire().is_none());
    }
}

#[test]
fn handle_lease_waiter_observes_last_release() {
    let leases = Arc::new(HandleLeaseState::new());
    let lease = leases.acquire().expect("lease admission is open");

    let waiting = Arc::clone(&leases);
    let (started_tx, started_rx) = std::sync::mpsc::channel();

    let waiter = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        waiting.wait_for_idle();
    });

    started_rx.recv().unwrap();

    drop(lease);

    waiter.join().unwrap();
    assert_eq!(leases.active(), 0);
}

#[test]
fn handle_lease_waiter_rechecks_after_release() {
    use std::sync::Barrier;

    let leases = Arc::new(HandleLeaseState::new());
    let lease = leases.acquire().expect("lease admission is open");

    let barrier = Arc::new(Barrier::new(2));
    let barrier_hook = Arc::clone(&barrier);
    *leases.before_idle_wait_hook.lock() = Some(Arc::new(move || {
        barrier_hook.wait();
    }));

    let waiting = Arc::clone(&leases);
    let waiter = std::thread::spawn(move || {
        waiting.wait_for_idle();
    });

    barrier.wait();

    drop(lease);

    waiter.join().unwrap();
    assert_eq!(leases.active(), 0);
}

#[test]
fn registry_close_with_leases_waits_for_active_handle_and_blocks_new_lookups() {
    use std::sync::mpsc;
    use std::time::Duration;

    struct TestObj;
    impl ExcelHandleObject for TestObj {}

    let registry = Arc::new(HandleRegistry::new(8));
    let leases = Arc::new(HandleLeaseState::new());

    let (token, _) = registry
        .insert_pending(&mut Some(Arc::new(TestObj)))
        .map(|t| (t, ()))
        .unwrap();

    let handle: Handle<TestObj> = registry.lookup_handle(&token, &leases).unwrap();
    assert_eq!(leases.active(), 1);

    let closing_registry = Arc::clone(&registry);
    let closing_leases = Arc::clone(&leases);
    let (closed_tx, closed_rx) = mpsc::sync_channel(1);

    let closer = std::thread::spawn(move || {
        closed_tx
            .send(closing_registry.close_with_leases(&closing_leases))
            .unwrap();
    });

    while !registry.state.read().closed {
        std::thread::yield_now();
    }

    assert!(matches!(
        registry.lookup_handle::<TestObj>(&token, &leases),
        Err(XllError::Closing)
    ));

    assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());

    drop(handle);

    closed_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    closer.join().unwrap();
    assert_eq!(leases.active(), 0);
    assert!(
        leases.acquire().is_none(),
        "close must seal independent lease admission"
    );
}

#[test]
fn warm_hit_does_not_enter_single_flight_initialization() {
    let runtime = HandleRuntime::new(8);
    let key = test_topic_key("warm-fast");

    let (token, created) = runtime
        .prepare_observed(key, || Ok(Arc::new(DataRecord(1))), |_, _| Ok(()))
        .unwrap();

    assert!(created);

    let calls = AtomicUsize::new(0);

    let (second, created) = runtime
        .prepare_observed(
            key,
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(Arc::new(DataRecord(2)))
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
        .prepare_observed(key, || Ok(Arc::new(DataRecord(1))), |_, _| Ok(()))
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
        closed_tx.send(closing_runtime.close()).unwrap();
    });

    while !runtime.topics.read().closed {
        std::thread::yield_now();
    }

    //
    // close has started, but registry must remain alive while observe executes.
    //
    assert!(!runtime.registry.state.read().closed);

    assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());

    release_tx.send(()).unwrap();

    assert!(matches!(warm.join().unwrap(), Err(XllError::Closing)));

    closed_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();

    closer.join().unwrap();

    assert!(runtime.registry.state.read().closed);
}
