// module: subscription/data_plane/refresh/publication
use super::{ArrayApi, OutputSlots, PendingOutputs};

fn handoff_twice<A: ArrayApi, O: OutputSlots<A>>(output: O) {
    let staged = PendingOutputs::<A, O>::stage(output);
    drop(staged.complete().output);
    drop(staged.complete().output); // error: E0382
}
