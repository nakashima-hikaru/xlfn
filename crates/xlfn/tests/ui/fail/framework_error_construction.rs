use xlfn::XllError;

fn main() {
    let _ = XllError::WindowsApi {
        function: "invented framework call",
        code: 42,
    };
}
