// module: async_udf/runtime
use super::RuntimeReservation;
use crate::cancellation::CancellationSource;
fn invalid<E: crate::AsyncExecutor>(reservation: RuntimeReservation<'_, E>, source: CancellationSource) {
    reservation.commit(std::future::ready(()), source); // error: E0599
}
