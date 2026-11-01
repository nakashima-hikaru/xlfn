// module: runtime
use crate::lifecycle::OpenStart;

fn invalid<A: crate::Addin>(start: OpenStart<'_, A>) {
    start.transfer_to_opening();
    start.transfer_to_opening(); // error: E0382
}
