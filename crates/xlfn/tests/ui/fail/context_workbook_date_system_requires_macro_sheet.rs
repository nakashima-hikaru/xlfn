
use xlfn::addin::{MainThreadContext};
use xlfn::addin::{ThreadSafeContext};
use xlfn::reference::ExcelReference;

fn main_thread(context: MainThreadContext<'_, ()>, reference: ExcelReference<'_>) {
    let _ = context.workbook_date_system(&reference);
}

fn thread_safe(context: ThreadSafeContext<'_, ()>, reference: ExcelReference<'_>) {
    let _ = context.workbook_date_system(&reference);
}

fn main() {}
