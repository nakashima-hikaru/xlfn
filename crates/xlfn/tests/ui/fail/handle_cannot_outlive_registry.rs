use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct TestObj(i32);

fn main() {
    xlfn::__private::v1::with_branded_call_scope(|storage, brand| {
        let mut context = xlfn::__private::v1::DecodeCallContext::with_storage(storage, brand);
        let registry = xlfn::__private::handle_test::HandleRegistry::new(16);
        let token = registry.insert_object(TestObj(42)).unwrap();
        let handle = registry.lookup_handle::<TestObj>(&mut context, &token).unwrap();
        drop(handle);
        // The storage retains its admission permit after the handle is dropped.
    });
}
