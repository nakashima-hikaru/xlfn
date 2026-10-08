# use xlfn::prelude::*;
# #[derive(ExcelHandleObject)]
# pub struct Dataset;
# impl Dataset {
#     fn total(&self) -> f64 { 42.0 }
#     fn evaluate(&self, time: f64) -> XllResult<f64> { Ok(self.total() * time) }
# }
# #[excel_addin(name = "Guide dataset", id = "guide-dataset", category = "Guide")]
# pub struct DatasetAddin;
# impl Addin for DatasetAddin {
#     type SharedState = ();
#     type LifecycleState = ();
#     type Error = XllError;
#     type Layers = ();
#     #[cfg(feature = "async-builtin")]
#     type AsyncExecutor = xlfn::BuiltinAsyncExecutor;
#     #[cfg(all(feature = "async", not(feature = "async-builtin")))]
#     type AsyncExecutor = xlfn::NoAsyncExecutor;
#     fn open(_: &OpenContext) -> xlfn::OpenResult<Self> {
#         let opened = Opened::new(());
#         #[cfg(feature = "async-builtin")]
#         let opened = opened.with_async_executor(xlfn::BuiltinAsyncExecutor::new(
#             xlfn::BuiltinExecutorConfig::new(),
#         ));
#         Ok(opened)
#     }
# }
# fn main() {}
