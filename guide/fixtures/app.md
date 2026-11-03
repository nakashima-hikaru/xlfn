# use xlfn::prelude::*;
# pub struct AppTools;
# pub struct State { environment: String, version: String }
# #[excel_addin(name = "Guide app", id = "guide-app", category = "Guide")]
# impl Addin for AppTools {
#     type SharedState = State;
#     type AsyncExecutor = xlfn::BuiltinAsyncExecutor;
#     fn open(_: &OpenContext<Self>) -> xlfn::OpenResult<Self> {
#         let opened = Opened::new(State { environment: "test".into(), version: "test".into() });
#         #[cfg(feature = "async-builtin")]
#         let opened = opened.with_async_executor(xlfn::BuiltinAsyncExecutor::new(
#             xlfn::BuiltinExecutorConfig::new(),
#         ));
#         Ok(opened)
#     }
# }
# fn main() {}
