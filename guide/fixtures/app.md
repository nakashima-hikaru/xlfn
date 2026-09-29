# use xlfn::prelude::*;
# #[excel_addin(name = "Guide app", id = "guide-app", category = "Guide")]
# pub struct AppTools;
# pub struct State { environment: String, version: String }
# impl Addin for AppTools {
#     type SharedState = State;
#     type LifecycleState = ();
#     type Error = XllError;
#     type Layers = ();
#     fn open(_: &OpenContext) -> XllResult<Opened<State>> {
#         Ok(Opened::new(State { environment: "test".into(), version: "test".into() }))
#     }
# }
# fn main() {}
