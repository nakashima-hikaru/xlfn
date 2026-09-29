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
#     fn open(_: &OpenContext) -> XllResult<Opened<()>> { Ok(Opened::new(())) }
# }
# fn main() {}
