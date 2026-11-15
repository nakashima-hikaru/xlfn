// module: async_udf
use super::registry::TaskRegistry;
use super::task::ActiveReservation;
use crate::addin_impl::AsyncTaskLimit;

fn invalid() -> crate::XllResult<()> {
    let _reservation;
    {
        let registry = TaskRegistry::new();
        registry.prepare_start(AsyncTaskLimit::DEFAULT)?.publish();
        _reservation = ActiveReservation::try_acquire(&registry); // error: E0597
    }
    Ok(())
}
