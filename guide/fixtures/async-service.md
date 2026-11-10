# use xlfn::prelude::*;
# pub struct ServiceAddin;
# pub struct Client;
# impl Client {
#     async fn fetch(&self, _: &str) -> XllResult<f64> { Ok(42.0) }
#     fn fetch_blocking(&self, _: &str) -> XllResult<f64> { Ok(42.0) }
# }
# fn main() {}
# pub struct State { client: Client, adapter: Client }
# #[excel_addin(name = "Guide service", id = "guide-service", category = "Guide")]
# impl Addin for ServiceAddin {
#     type SharedState = State;
#     type AsyncExecutor = xlfn::task::BuiltinAsyncExecutor;
#     fn open(_: &OpenContext<Self>) -> xlfn::addin::OpenResult<Self> {
#         Ok(Opened::new(State { client: Client, adapter: Client })
#             .with_async_executor(xlfn::task::BuiltinAsyncExecutor::new(xlfn::task::BuiltinExecutorConfig::new())))
#     }
# }
