use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct Dataset;

fn move_to_thread<'a>(lease: HandleLease<'a, Dataset>) {
    std::thread::spawn(move || drop(lease));
}

fn main() {}
