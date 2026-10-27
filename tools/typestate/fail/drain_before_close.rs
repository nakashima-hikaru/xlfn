// module: async_udf
use super::calculation::CalculationState;

fn invalid(state: &CalculationState) {
    state.drain_tasks(); // error: E0599
}
