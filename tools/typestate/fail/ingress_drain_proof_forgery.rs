// module: runtime
use crate::ingress::ExportsDrained;

fn invalid() {
    let _ = ExportsDrained { epoch: 0 }; // error: E0451
}
