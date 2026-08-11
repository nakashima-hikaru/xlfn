use super::*;

fn insert_production<T>(registry: &HandleRegistry, value: Arc<T>) -> XllResult<String>
where
    T: Any + Send + Sync + 'static,
{
    let mut value = Some(value);
    registry.insert_pending(&mut value)
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
    let (token, _) = runtime
        .prepare("escaped-panic".to_owned(), || Ok(Arc::new(PanicOnDrop)))
        .unwrap();
    let escaped = runtime.lookup::<PanicOnDrop>(&token).unwrap();

    // Remove the formula-owned registry root first. The escaped Handle now
    // owns the final Arc and must contain its destructor panic itself.
    runtime.rollback("escaped-panic");
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
    let calls = AtomicUsize::new(0);

    let (first, created) = runtime
        .prepare("same".to_owned(), || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(DataRecord(1)))
        })
        .unwrap();
    assert!(created);

    let (second, created) = runtime
        .prepare("same".to_owned(), || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(DataRecord(2)))
        })
        .unwrap();
    assert!(!created);
    assert_eq!(first, second);
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    assert_eq!(runtime.lookup::<DataRecord>(&first).unwrap().0, 1);

    runtime.connect(1, 41, "same").unwrap();
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
        .prepare("argument".to_owned(), || Ok(Arc::new(DataRecord(19))))
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
    let (token, _) = handles
        .prepare("argument-errors".to_owned(), || {
            Ok(Arc::new(DataRecord(23)))
        })
        .unwrap();
    handles.connect(1, 91, "argument-errors").unwrap();

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
    let (source_token, _) = runtime
        .prepare("source".to_owned(), || Ok(Arc::clone(&shared)))
        .unwrap();
    runtime.connect(1, 1, "source").unwrap();

    let resolved = runtime.lookup::<DataRecord>(&source_token).unwrap();
    let (alias_token, _) = runtime
        .prepare("alias".to_owned(), || Ok(resolved.into_arc()))
        .unwrap();
    runtime.connect(1, 2, "alias").unwrap();
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
    runtime
        .prepare("pending".to_owned(), || Ok(Arc::new(DataRecord(1))))
        .unwrap();
    runtime.rollback("pending");
    assert_eq!(runtime.len(), 0);
}

#[test]
fn uncalculated_rtd_connection_rolls_back_an_already_connected_topic() {
    let runtime = HandleRuntime::new(8);
    runtime
        .prepare("uncalculated".to_owned(), || Ok(Arc::new(DataRecord(1))))
        .unwrap();
    runtime.connect(1, 9, "uncalculated").unwrap();
    runtime.rollback("uncalculated");
    assert_eq!(runtime.len(), 0);
    runtime.disconnect(1, 9);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn uncommitted_connect_transaction_rolls_back_only_the_excel_connection() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let (token, _) = runtime
        .prepare("transactional".to_owned(), || Ok(Arc::new(DataRecord(1))))
        .unwrap();

    let connection = runtime.connect_transaction(1, 10, "transactional").unwrap();
    assert_eq!(connection.token(), token);
    drop(connection);

    assert_eq!(runtime.len(), 1);
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);

    let retry = runtime.connect_transaction(1, 10, "transactional").unwrap();
    assert_eq!(retry.token(), token);
    retry.commit().unwrap();
    runtime.disconnect(1, 10);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn concurrent_handle_connect_rejects_an_uncommitted_assignment() {
    let runtime = Arc::new(HandleRuntime::new(8));
    runtime
        .prepare("concurrent-transaction".to_owned(), || {
            Ok(Arc::new(DataRecord(3)))
        })
        .unwrap();

    let connection = runtime
        .connect_transaction(1, 12, "concurrent-transaction")
        .unwrap();
    assert!(matches!(
        runtime.connect_transaction(1, 12, "concurrent-transaction"),
        Err(XllError::Overloaded)
    ));
    connection.commit().unwrap();

    let repeated = runtime
        .connect_transaction(1, 12, "concurrent-transaction")
        .unwrap();
    repeated.commit().unwrap();
    runtime.disconnect(1, 12);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn failed_repeated_connect_transaction_preserves_existing_connection() {
    let runtime = Arc::new(HandleRuntime::new(8));
    let (token, _) = runtime
        .prepare("existing-transaction".to_owned(), || {
            Ok(Arc::new(DataRecord(2)))
        })
        .unwrap();
    runtime.connect(1, 11, "existing-transaction").unwrap();

    let connection = runtime
        .connect_transaction(1, 11, "existing-transaction")
        .unwrap();
    assert_eq!(connection.token(), token);
    drop(connection);

    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 2);
    runtime.disconnect(1, 11);
    assert_eq!(runtime.len(), 0);
}

#[test]
fn excel_topic_id_cannot_be_connected_to_two_formula_topics() {
    let runtime = HandleRuntime::new(8);
    runtime
        .prepare("first".to_owned(), || Ok(Arc::new(DataRecord(1))))
        .unwrap();
    runtime
        .prepare("second".to_owned(), || Ok(Arc::new(DataRecord(2))))
        .unwrap();
    runtime.connect(1, 9, "first").unwrap();
    assert!(matches!(
        runtime.connect(1, 9, "second"),
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
    let (first, _) = runtime
        .prepare("sheet:A1:rate=1".to_owned(), || Ok(Arc::new(DataRecord(1))))
        .unwrap();
    let (second, _) = runtime
        .prepare("sheet:A2:rate=1".to_owned(), || Ok(Arc::new(DataRecord(1))))
        .unwrap();
    let (changed, _) = runtime
        .prepare("sheet:A1:rate=2".to_owned(), || Ok(Arc::new(DataRecord(2))))
        .unwrap();
    assert_ne!(first, second);
    assert_ne!(first, changed);
}

#[test]
fn disconnect_waits_for_an_in_flight_consumer_and_drops_once() {
    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime = HandleRuntime::new(8);
    let (token, _) = runtime
        .prepare("sheet:A1".to_owned(), || {
            Ok(Arc::new(CountedDataRecord(Arc::clone(&drops))))
        })
        .unwrap();
    runtime.connect(1, 7, "sheet:A1").unwrap();
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
    for key in ["one", "two"] {
        runtime
            .prepare(key.to_owned(), || {
                Ok(Arc::new(CountedDataRecord(Arc::clone(&drops))))
            })
            .unwrap();
        runtime.claim_server(key, 1).unwrap();
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
        let _ =
            runtime.prepare::<DataRecord>("panic".to_owned(), || panic!("injected factory panic"));
    }));
    assert!(panic.is_err());
    assert_eq!(runtime.len(), 0);
}

#[test]
fn same_thread_factory_reentry_returns_an_error_without_waiting() {
    let runtime = HandleRuntime::new(8);
    let (token, created) = runtime
        .prepare("factory-reentry".to_owned(), || {
            let nested =
                runtime.prepare("factory-reentry".to_owned(), || Ok(Arc::new(DataRecord(2))));
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
    let (token, created) = runtime
        .prepare("outer-factory".to_owned(), || {
            let nested =
                runtime.prepare("inner-factory".to_owned(), || Ok(Arc::new(DataRecord(2))));
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
    let (token, created) = runtime
        .prepare_observed(
            "observer-reentry".to_owned(),
            || Ok(Arc::new(DataRecord(1))),
            |_, _| {
                let nested = runtime.prepare("observer-reentry".to_owned(), || {
                    Ok(Arc::new(DataRecord(2)))
                });
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
    let (token, created) = runtime
        .prepare_observed(
            "outer-observer".to_owned(),
            || Ok(Arc::new(DataRecord(1))),
            |_, _| {
                let nested =
                    runtime.prepare("inner-observer".to_owned(), || Ok(Arc::new(DataRecord(2))));
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
    let first = runtime.prepare_observed(
        "observed".to_owned(),
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
        .prepare_observed(
            "observed".to_owned(),
            || Ok(Arc::new(DataRecord(2))),
            |_, _| Ok(()),
        )
        .unwrap();
    assert!(created);
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 2);
}

#[test]
fn cache_hit_observe_failure_does_not_invalidate_object() {
    let runtime = HandleRuntime::new(8);
    let (token, created) = runtime
        .prepare_observed(
            "observed-memoized".to_owned(),
            || Ok(Arc::new(DataRecord(1))),
            |_, _| Ok(()),
        )
        .unwrap();
    assert!(created);

    let calls = AtomicUsize::new(0);
    let result = runtime.prepare_observed(
        "observed-memoized".to_owned(),
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
    let (token, created) = runtime
        .prepare_observed(
            "observe-retry".to_owned(),
            || Ok(Arc::new(DataRecord(10))),
            |_, _| Ok(()),
        )
        .unwrap();
    assert!(created);

    // Observation failure on warm hit
    let result = runtime.prepare_observed(
        "observe-retry".to_owned(),
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
        .prepare_observed(
            "observe-retry".to_owned(),
            || Ok(Arc::new(DataRecord(30))),
            |_, _| Ok(()),
        )
        .unwrap();
    assert!(!created);
    assert_eq!(retry_token, token);
    assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 10);
}

#[test]
fn observation_cannot_commit_a_topic_removed_reentrantly() {
    let runtime = HandleRuntime::new(8);
    let result = runtime.prepare_observed(
        "removed-during-observation".to_owned(),
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
fn concurrent_waiter_retries_after_observation_failure() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let runtime = Arc::new(HandleRuntime::new(8));
    let (observing_tx, observing_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let first_runtime = Arc::clone(&runtime);
    let first = std::thread::spawn(move || {
        first_runtime.prepare_observed(
            "concurrent-observe".to_owned(),
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
        second_runtime.prepare_observed(
            "concurrent-observe".to_owned(),
            || Ok(Arc::new(DataRecord(2))),
            |_, _| Ok(()),
        )
    });
    waiting_rx.recv().unwrap();

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let waiter_is_blocked = {
            let topics = runtime.topics.lock();
            topics
                .initializing
                .get("concurrent-observe")
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

    let runtime1 = Arc::clone(&runtime);
    let factory_calls1 = Arc::clone(&factory_calls);
    let barrier1 = Arc::clone(&barrier);

    let t1 = std::thread::spawn(move || {
        runtime1
            .prepare("concurrent_key".to_owned(), || {
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
            .prepare("concurrent_key".to_owned(), || {
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
    let (upstream_a, created) = runtime
        .prepare("sheet:A1:CURVE.CREATE:digest_a".to_owned(), || {
            Ok(Arc::new(DataRecord(10)))
        })
        .unwrap();
    assert!(created);

    // Downstream uses upstream token as part of its key, simulating
    // MODEL.CREATE(Handle<Curve>, params). The raw upstream token becomes
    // part of the argument digest, so a different upstream token yields
    // a different downstream key.
    let downstream_key_a = format!("sheet:B1:MODEL.CREATE:{}:params", upstream_a);
    let (downstream_a, created) = runtime
        .prepare(downstream_key_a, || Ok(Arc::new(DataRecord(100))))
        .unwrap();
    assert!(created);

    // Upstream changes (different arguments → different key)
    let (upstream_b, created) = runtime
        .prepare("sheet:A1:CURVE.CREATE:digest_b".to_owned(), || {
            Ok(Arc::new(DataRecord(20)))
        })
        .unwrap();
    assert!(created);
    assert_ne!(upstream_a, upstream_b);

    // Downstream key also changes because the upstream token changed
    let downstream_key_b = format!("sheet:B1:MODEL.CREATE:{}:params", upstream_b);
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
    let (token, _) = runtime
        .prepare("leased".to_owned(), || Ok(Arc::new(DataRecord(41))))
        .unwrap();
    runtime.connect(1, 1, "leased").unwrap();

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
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (factory_started_tx, factory_started_rx) = mpsc::channel();
    let (release_factory_tx, release_factory_rx) = mpsc::channel();

    let creator_runtime = Arc::clone(&runtime);
    let creator_observed = Arc::clone(&observed);
    let creator = std::thread::spawn(move || {
        creator_runtime.prepare_observed(
            "closing".to_owned(),
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
        let result = waiter_runtime.prepare("closing".to_owned(), || Ok(Arc::new(DataRecord(2))));
        waiter_done_tx.send(result).unwrap();
    });

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let blocked = runtime
            .topics
            .lock()
            .initializing
            .get("closing")
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
    while !runtime.topics.lock().closed {
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
        .prepare("inner".to_string(), || Ok(Arc::new(InnerObj)))
        .unwrap();
    let inner_handle = runtime.lookup::<InnerObj>(&inner_token).unwrap();

    let (outer_token, _) = runtime
        .prepare("outer".to_string(), move || {
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
fn handle_lease_waiter_is_woken_by_last_release() {
    let leases = Arc::new(HandleLeaseState::new());
    let lease = leases.acquire();

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
fn handle_lease_waiter_synchronization_prevents_lost_wakeup() {
    use std::sync::Barrier;

    let leases = Arc::new(HandleLeaseState::new());
    let lease = leases.acquire();

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
fn final_lease_drop_does_not_take_wait_lock_without_waiters() {
    use std::sync::mpsc;
    use std::time::Duration;

    let leases = Arc::new(HandleLeaseState::new());
    let lease = leases.acquire();

    // Deliberately occupy the shutdown-only mutex.
    let wait_guard = leases.wait_lock.lock();

    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        drop(lease);
        done_tx.send(()).unwrap();
    });

    // The ordinary final-drop path must not depend on wait_lock when there
    // are no shutdown waiters.
    let completed = done_rx.recv_timeout(Duration::from_millis(100)).is_ok();

    drop(wait_guard);
    worker.join().unwrap();

    assert!(completed);
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
}

#[test]
fn warm_hit_does_not_enter_single_flight_initialization() {
    let runtime = HandleRuntime::new(8);

    let (token, created) = runtime
        .prepare_observed(
            "warm-fast".to_owned(),
            || Ok(Arc::new(DataRecord(1))),
            |_, _| Ok(()),
        )
        .unwrap();

    assert!(created);

    let calls = AtomicUsize::new(0);

    let (second, created) = runtime
        .prepare_observed(
            "warm-fast".to_owned(),
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(Arc::new(DataRecord(2)))
            },
            |key, _| {
                let topics = runtime.topics.lock();

                assert!(
                    !topics.initializing.contains_key(key),
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

    runtime
        .prepare_observed(
            "warm-close".to_owned(),
            || Ok(Arc::new(DataRecord(1))),
            |_, _| Ok(()),
        )
        .unwrap();

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let warm_runtime = Arc::clone(&runtime);
    let warm = std::thread::spawn(move || {
        warm_runtime.prepare_observed::<DataRecord>(
            "warm-close".to_owned(),
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

    while !runtime.topics.lock().closed {
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
