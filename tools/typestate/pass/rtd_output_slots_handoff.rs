// module: subscription/data_plane/refresh/publication
use super::{ArrayApi, OutputSlots, PendingOutputs};

fn handoff<A: ArrayApi, O: OutputSlots<A>>(output: O) -> Result<(), i32> {
    let mut staged = PendingOutputs::<A, O>::stage(output);
    staged.get_mut().output.write_count(0)?;
    staged.get_mut().output.write_array(None)?;
    drop(staged.complete().output);
    Ok(())
}
