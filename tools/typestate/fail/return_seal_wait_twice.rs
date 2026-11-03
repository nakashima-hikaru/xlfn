// module: runtime
fn invalid(protocol: &crate::runtime_components::ReturnProtocol) {
    let seal = protocol.close_admission();
    let _first = seal.wait();
    let _second = seal.wait(); // error: E0382
}
