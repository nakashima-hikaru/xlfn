use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct TestObj(i32);

fn main() {
    let scope = xlfn::__private::handle_test::new_call_scope();
    let handle = {
        let registry = xlfn::__private::handle_test::HandleRegistry::new(16);
        let token = registry.insert_object(TestObj(42)).unwrap();
        registry.lookup_handle::<TestObj>(&scope, &token).unwrap()
    };
    let _ = *handle;
}
