// module: async_udf
use super::calculation::CalculationState;

fn invalid(state: &CalculationState) {
    let _ = &state.admission; // error: E0616
    let _ = &state.shards; // error: E0616
    let _ = &state.pins; // error: E0616
}
