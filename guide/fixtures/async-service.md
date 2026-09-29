# use xlfn::prelude::*;
# #[excel_addin(name = "Guide service", id = "guide-service", category = "Guide")]
# pub struct ServiceAddin;
# pub struct Client;
# impl Client {
#     async fn fetch(&self, _: &str) -> XllResult<f64> { Ok(42.0) }
#     fn fetch_blocking(&self, _: &str) -> XllResult<f64> { Ok(42.0) }
# }
# fn main() {}
# pub struct State { client: Client, adapter: Client }
# impl Addin for ServiceAddin {
#     type SharedState = State;
#     type LifecycleState = ();
#     type Error = XllError;
#     type Layers = ();
#     fn open(_: &OpenContext) -> XllResult<Opened<State>> {
#         Ok(Opened::new(State { client: Client, adapter: Client }))
#     }
# }
