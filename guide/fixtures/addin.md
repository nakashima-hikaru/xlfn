# #[xlfn::excel_addin(name = "Guide example", id = "guide-example", category = "Guide")]
# pub struct GuideAddin;
# impl xlfn::Addin for GuideAddin {
#     type SharedState = ();
#     type LifecycleState = ();
#     type Error = xlfn::XllError;
#     type Layers = ();
#     fn open(_: &xlfn::OpenContext) -> xlfn::XllResult<xlfn::Opened<()>> {
#         Ok(xlfn::Opened::new(()))
#     }
# }
# fn main() {}
