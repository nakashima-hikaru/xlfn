// module: subscription/runtime
fn invalid(runtime: &super::SubscriptionRuntime<crate::excel_rtd::RtdSubscriptionHost>) {
    let _ = runtime.termination_coordinator.coordinator.begin(); // error: E0616
}
