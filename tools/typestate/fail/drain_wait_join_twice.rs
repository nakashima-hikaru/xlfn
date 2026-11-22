// module: host/drain/windows
use super::UnstartedDrain;

fn invalid() {
    std::thread::scope(|scope| {
        let worker = UnstartedDrain::new(|| 7_u32)
            .spawn(scope, std::thread::Builder::new(), || {})
            .unwrap_or_else(|_| panic!());
        let _joined = worker.join();
        let _joined = worker.join(); // error: E0382
    });
}
