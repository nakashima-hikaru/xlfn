// Diagnostic source only: append to resident_index.rs in a disposable workspace copy.
// Compile as a release library with unstable-cache, without test/bench-internals.
#[allow(
    unreachable_pub,
    private_interfaces,
    reason = "Temporary production codegen probes"
)]
mod codegen_probe {
    use super::*;
    type Raw = Cache<VersionedKey<u64>, Entry<u64>>;
    type Wrapped = ResidentIndex<u64, u64>;
    const _: () = assert!(std::mem::size_of::<Raw>() == std::mem::size_of::<Wrapped>());
    const _: () = assert!(std::mem::align_of::<Raw>() == std::mem::align_of::<Wrapped>());
    #[unsafe(no_mangle)]
    pub fn resident_codegen_get_wrapped(
        index: &Wrapped,
        key: &VersionedKeyRef<'_, u64>,
    ) -> Option<Entry<u64>> {
        index.get(key)
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_get_direct(
        index: &Raw,
        key: &VersionedKeyRef<'_, u64>,
    ) -> Option<Entry<u64>> {
        index.get(key)
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_insert_wrapped(
        index: &Wrapped,
        key: &VersionedKey<u64>,
        init: fn() -> XllResult<Entry<u64>>,
    ) -> Result<Entry<u64>, Arc<XllError>> {
        index.insert(key, init)
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_insert_direct(
        index: &Raw,
        key: &VersionedKey<u64>,
        init: fn() -> XllResult<Entry<u64>>,
    ) -> Result<Entry<u64>, Arc<XllError>> {
        index.try_get_with_by_ref(key, init)
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_invalidate_wrapped(index: &Wrapped, key: &VersionedKey<u64>) {
        index.invalidate(key);
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_invalidate_direct(index: &Raw, key: &VersionedKey<u64>) {
        index.invalidate(key);
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_maintenance_wrapped(index: &Wrapped) {
        index.maintenance();
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_maintenance_direct(index: &Raw) {
        index.run_pending_tasks();
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_clear_wrapped(index: &Wrapped) {
        index.clear();
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_clear_direct(index: &Raw) {
        index.invalidate_all();
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_count_wrapped(index: &Wrapped) -> u64 {
        index.resident_count()
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_count_direct(index: &Raw) -> u64 {
        index.entry_count()
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_weight_wrapped(index: &Wrapped) -> u64 {
        index.resident_weight()
    }
    #[unsafe(no_mangle)]
    pub fn resident_codegen_weight_direct(index: &Raw) -> u64 {
        index.weighted_size()
    }
}
