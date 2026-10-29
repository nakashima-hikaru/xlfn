# use xlfn::prelude::*;
# #[derive(ExcelHandleObject)]
# pub struct Dataset;
# impl Dataset {
#     fn total(&self) -> f64 { 42.0 }
#     fn evaluate(&self, time: f64) -> XllResult<f64> { Ok(self.total() * time) }
# }
# pub struct DatasetAddin;
# #[excel_addin(name = "Guide dataset", id = "guide-dataset", category = "Guide")]
# impl Addin for DatasetAddin {
#     type SharedState = ();
#     type AsyncExecutor = xlfn::BuiltinAsyncExecutor;
#     fn open(_: &OpenContext<Self>) -> xlfn::OpenResult<Self> {
#         let opened = Opened::new(());
#         #[cfg(feature = "async-builtin")]
#         let opened = opened.with_async_executor(xlfn::BuiltinAsyncExecutor::new(
#             xlfn::BuiltinExecutorConfig::new(),
#         ));
#         Ok(opened)
#     }
# }
# fn main() {}
