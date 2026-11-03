// module: macro_abi
use crate::__private::v1::*;
fn cannot_replace_callback_issuer<'call>(
    mut environment: CallEnvironment<'call>,
    callbacks: &'call crate::host_callback::HostCallbackSession,
) {
    environment.callbacks = callbacks; // error: E0616
}
