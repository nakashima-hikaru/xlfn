// module: async_udf
use super::calculation::{CalculationEpoch, CalculationState};
use super::registry::TaskRegistry;

fn drain_calculation() {
    let state = CalculationState::new(CalculationEpoch::INITIAL);
    let controls = state.close_admission().wait().drain_tasks();
    drop(controls);
}

fn close_registry(registry: &TaskRegistry) -> crate::XllResult<()> {
    registry.request_close()?.cancel().wait().finish();
    Ok(())
}
