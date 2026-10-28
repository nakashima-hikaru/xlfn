# #[xlfn::excel_addin(name = "Guide example", id = "guide-example", category = "Guide")]
# pub struct GuideAddin;
# impl xlfn::Addin for GuideAddin {
#     type RtdSubscription = ();
#     type RtdSources = xlfn::DynamicRtdSources<Self::RtdSubscription>;
#     type SharedState = ();
#     type LifecycleState = ();
#     type Error = xlfn::XllError;
#     type Layers = ();
#     #[cfg(feature = "async")]
#     type AsyncExecutor = xlfn::NoAsyncExecutor;
#     fn open(_: &xlfn::OpenContext<Self>) -> xlfn::XllResult<xlfn::Opened<()>> {
#         Ok(xlfn::Opened::new(()))
#     }
# }
# fn main() {}
