// module: async_udf
use super::calculation::ClosingCalculation;

fn invalid(closing: ClosingCalculation<'_>) {
    closing.drain_tasks(); // error: E0599
}
