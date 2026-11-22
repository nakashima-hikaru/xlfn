// module: subscription/data_plane/refresh/windows
use super::RefreshOutputSlots;
use crate::excel_rtd::WindowsRefreshArrayApi;
use xlfn_host::refresh_array::ReadyArray;

fn invalid(array: ReadyArray<WindowsRefreshArrayApi>, output: RefreshOutputSlots<'_>) {
    let _ = array.handoff(output); // error: E0599
}
