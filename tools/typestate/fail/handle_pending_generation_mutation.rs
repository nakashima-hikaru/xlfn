// module: handle
fn invalid<T: super::ExcelHandleObject>(mut pending: super::PendingHandleLease<T>, generation: crate::generation::RuntimeGeneration) {
    pending.generation = generation; // error: E0616
}
