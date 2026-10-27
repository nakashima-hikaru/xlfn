// module: runtime
use super::{PhysicalUnload, Runtime};

fn requires_physical_contract(_: &Runtime<(), PhysicalUnload>) {} // error: E0277
