use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct TestObj(i32);

fn main() {
    let scope = xlfn::__private::handle_test::new_call_scope();
    {
        let registry = xlfn::__private::handle_test::HandleRegistry::new(16);
        let token = registry.insert_object(TestObj(42)).unwrap();
        let handle = registry.lookup_handle::<TestObj>(&scope, &token).unwrap();
        drop(handle);
    }
    drop(scope);
}
