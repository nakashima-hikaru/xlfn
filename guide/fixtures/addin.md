# pub struct GuideAddin;
# #[xlfn::excel_addin(name = "Guide example", id = "guide-example", category = "Guide")]
# impl xlfn::Addin for GuideAddin {
#     type SharedState = ();
#     fn open(_: &xlfn::addin::OpenContext<Self>) -> xlfn::XllResult<xlfn::addin::Opened<()>> {
#         Ok(xlfn::addin::Opened::new(()))
#     }
# }
# fn main() {}
