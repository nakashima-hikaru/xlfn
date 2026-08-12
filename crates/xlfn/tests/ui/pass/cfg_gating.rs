#[xlfn::excel_function(name = "TEST.CFG.DISABLED")]
#[cfg(any())]
fn cfg_disabled() -> f64 {
    1.0
}

#[xlfn::excel_function(name = "TEST.CFG_ATTR.DISABLED")]
#[cfg_attr(all(), cfg(any()))]
fn cfg_attr_disabled() -> f64 {
    2.0
}

#[xlfn::excel_addin(name = "Disabled", id = "disabled", category = "Test")]
#[cfg(any())]
pub struct DisabledAddin;

fn main() {}
