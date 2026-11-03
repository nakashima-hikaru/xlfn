# pub struct GuideAddin;
# #[xlfn::excel_addin(name = "Guide example", id = "guide-example", category = "Guide")]
# impl xlfn::Addin for GuideAddin {
#     type SharedState = ();
#     fn open(_: &xlfn::OpenContext<Self>) -> xlfn::XllResult<xlfn::Opened<()>> {
#         Ok(xlfn::Opened::new(()))
#     }
# }
# fn main() {}
