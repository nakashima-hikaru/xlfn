use xlfn::value::XlValueRef;

fn inspect(value: XlValueRef<'_>) {
    let _ = value.raw();
    let _ = value.raw_xltype();
}

fn main() {
    let _ = XlValueRef::from_raw;
}
