use xlfn::handle::{ExcelHandleObject, Handle, HandleLease};

struct Dataset;
impl ExcelHandleObject for Dataset {}

fn escape_handle<'a>(handle: Handle<'a, Dataset>) -> &'a Dataset {
    handle.as_ref()
}

fn escape_lease<'a>(lease: HandleLease<'a, Dataset>) -> &'a Dataset {
    lease.as_ref()
}

fn main() {}
