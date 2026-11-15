// module: excel_rtd/windows
use super::drain_with_com_dispatch;
use crate::excel_rtd::drain_wait::{DrainWork, UnstartedDrain};

fn ordered<W: DrainWork>(
    drain: UnstartedDrain<W>,
) -> Result<W::Output, (crate::XllError, UnstartedDrain<W>)> {
    let prepared = drain_with_com_dispatch(drain)?;
    let joined = prepared.run()?;
    Ok(joined.into_output())
}
