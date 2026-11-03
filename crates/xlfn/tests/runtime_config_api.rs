use xlfn::RuntimeConfig;

#[test]
fn default_runtime_config_is_const_constructible() {
    const CONFIG: RuntimeConfig = RuntimeConfig::new();
    assert_eq!(CONFIG, RuntimeConfig::default());
}

#[cfg(feature = "async")]
#[test]
fn async_config_accepts_nonzero_task_limits() {
    use xlfn::{AsyncConfig, AsyncTaskLimit};

    const CONFIG: RuntimeConfig = RuntimeConfig::new()
        .with_async(AsyncConfig::new().with_task_limit(AsyncTaskLimit::new(1).unwrap()));
    assert_ne!(CONFIG, RuntimeConfig::default());
    assert!(AsyncTaskLimit::try_from(0).is_err());
    assert_eq!(AsyncTaskLimit::DEFAULT.get(), 4096);
    assert_eq!(
        AsyncTaskLimit::try_from(usize::MAX).unwrap().get(),
        usize::MAX
    );
}

#[cfg(feature = "async-builtin")]
#[test]
fn builtin_executor_config_bounds_poller_count_separately_from_task_limit() {
    use xlfn::{AsyncPollerCount, BuiltinAsyncExecutor, BuiltinExecutorConfig};

    assert!(AsyncPollerCount::new(0).is_none());
    assert!(AsyncPollerCount::new(33).is_none());
    assert_eq!(AsyncPollerCount::new(32).unwrap().get(), 32);
    let _executor = BuiltinAsyncExecutor::new(
        BuiltinExecutorConfig::new().with_poller_count(AsyncPollerCount::new(2).unwrap()),
    );
}

#[cfg(feature = "handles")]
#[test]
fn handle_config_accepts_only_supported_binding_limits() {
    use xlfn::{HandleBindingLimit, HandleConfig};

    const CONFIG: RuntimeConfig = RuntimeConfig::new()
        .with_handles(HandleConfig::new().with_binding_limit(HandleBindingLimit::new(1).unwrap()));
    assert_ne!(CONFIG, RuntimeConfig::default());
    assert!(HandleBindingLimit::try_from(0).is_err());
    assert_eq!(
        HandleBindingLimit::try_from(HandleConfig::MAX_SUPPORTED_BINDINGS)
            .unwrap()
            .get(),
        HandleConfig::MAX_SUPPORTED_BINDINGS,
    );
    assert!(HandleBindingLimit::try_from(HandleConfig::MAX_SUPPORTED_BINDINGS + 1).is_err());
}

#[cfg(feature = "rtd")]
#[test]
fn rtd_config_distinguishes_disabled_and_bounded_admission() {
    use xlfn::RtdConfig;
    use xlfn::rtd::{RtdCapacity, RtdLimits};

    const LIMITS: RtdLimits = RtdLimits::standard()
        .with_max_pending(RtdCapacity::disabled())
        .with_max_active(RtdCapacity::disabled_if_zero(1));
    const CONFIG: RuntimeConfig =
        RuntimeConfig::new().with_rtd(RtdConfig::new().with_limits(LIMITS));
    assert_ne!(CONFIG, RuntimeConfig::default());
    assert!(LIMITS.max_pending().is_disabled());
    assert_eq!(LIMITS.max_active().get(), 1);
    assert!(!LIMITS.max_active().is_disabled());
}

#[cfg(all(feature = "async", feature = "handles", feature = "rtd"))]
#[test]
fn configuring_one_feature_preserves_the_other_feature_settings() {
    use xlfn::rtd::{RtdCapacity, RtdLimits};
    use xlfn::{AsyncConfig, AsyncTaskLimit, HandleBindingLimit, HandleConfig, RtdConfig};

    const ASYNC: AsyncConfig = AsyncConfig::new().with_task_limit(AsyncTaskLimit::new(1).unwrap());
    const HANDLES: HandleConfig =
        HandleConfig::new().with_binding_limit(HandleBindingLimit::new(1).unwrap());
    const RTD: RtdConfig = RtdConfig::new()
        .with_limits(RtdLimits::standard().with_max_active(RtdCapacity::disabled()));
    const CONFIG: RuntimeConfig = RuntimeConfig::new()
        .with_async(ASYNC)
        .with_handles(HANDLES)
        .with_rtd(RTD);
    assert_eq!(
        CONFIG,
        RuntimeConfig::new()
            .with_rtd(RTD)
            .with_handles(HANDLES)
            .with_async(ASYNC),
    );
    assert_ne!(CONFIG, RuntimeConfig::new().with_async(ASYNC));
    assert_ne!(CONFIG, RuntimeConfig::new().with_handles(HANDLES));
    assert_ne!(CONFIG, RuntimeConfig::new().with_rtd(RTD));
}
