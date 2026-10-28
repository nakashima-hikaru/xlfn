// module: registration/host
use super::{ExcelHost, Register, RegisterEvent, RegistrationId, RegistrationMutation, invoke_operation, mutation_from_invocation};
use std::ptr::NonNull;
use xlfn_sys::XLOPER12;

fn register(host: ExcelHost<'_>, arguments: &[NonNull<XLOPER12>]) {
    let _: RegistrationMutation<RegistrationId> = mutation_from_invocation(
        invoke_operation(host, Register { excel_name: "example" }, arguments),
    );
    let _: RegistrationMutation<i32> = mutation_from_invocation(
        invoke_operation(host, RegisterEvent, arguments),
    );
}
