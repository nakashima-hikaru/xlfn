// module: kernel/rotating_read_domain
use super::RotatingRetirementDomain;
use std::sync::Mutex;
fn invalid(_: &RotatingRetirementDomain<1, Mutex<Vec<usize>>>) {} // error: E0277
