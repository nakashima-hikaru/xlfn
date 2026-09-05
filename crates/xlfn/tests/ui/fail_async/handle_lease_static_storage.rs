use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct Dataset;

static SAVED: std::sync::Mutex<Option<HandleLease<'static, Dataset>>> =
    std::sync::Mutex::new(None);

fn stash<'a>(lease: HandleLease<'a, Dataset>) {
    *SAVED.lock().unwrap() = Some(lease);
}

fn main() {}
