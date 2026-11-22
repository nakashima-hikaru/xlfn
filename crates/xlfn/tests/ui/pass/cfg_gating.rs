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

#[xlfn::excel_function(name = "TEST.INNER.CFG.DISABLED")]
fn inner_cfg_disabled() -> f64 {
    #![cfg(any())]
    does_not_exist()
}

#[xlfn::excel_function(name = "TEST.INNER.CFG_ATTR.DISABLED")]
fn inner_cfg_attr_disabled() -> f64 {
    #![cfg_attr(all(), cfg(any()))]
    does_not_exist()
}

#[xlfn::excel_addin(name = "Disabled", id = "disabled", category = "Test")]
#[cfg(any())]
impl xlfn::Addin for DisabledAddin {}

#[xlfn::excel_function(name = "TEST.CFG_ATTR.BOOL")]
#[cfg_attr(true, cfg_attr(true, cfg(any())))]
fn cfg_attr_bool_disabled() -> f64 {
    does_not_exist()
}

#[xlfn::excel_function(name = "TEST.CFG_ATTR.FALSE")]
#[cfg_attr(false, cfg(any()))]
fn cfg_attr_false_enabled() -> f64 {
    3.0
}

struct LiveAddin;

#[xlfn::excel_addin(name = "Enabled", id = "enabled", category = "Test")]
#[cfg_attr(false, cfg(any()))]
impl xlfn::Addin for LiveAddin {
    type SharedState = ();

    fn open(_: &xlfn::addin::OpenContext<Self>) -> xlfn::addin::OpenResult<Self> {
        Ok(xlfn::addin::Opened::new(()))
    }
}

fn main() {
    assert_eq!(cfg_attr_false_enabled(), 3.0);
}
