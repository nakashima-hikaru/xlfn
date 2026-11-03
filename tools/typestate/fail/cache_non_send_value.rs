// module: async_udf
use crate::cache::CalculationCache;

fn invalid(cache: CalculationCache<(), std::rc::Rc<()>>) { // error: E0277
    drop(cache); // error: E0277
}
