use xlfn::value::XlValueRef;

fn inspect(value: XlValueRef<'_>) {
    let _ = value.raw();
}

fn main() {
    let _ = XlValueRef::from_raw;
}
