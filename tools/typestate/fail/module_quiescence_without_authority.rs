// module: runtime
use crate::excel_rtd_protocol::RtdModuleState;

fn invalid(module: &RtdModuleState) {
    module.certify_logical_quiescence(); // error: E0061
}
