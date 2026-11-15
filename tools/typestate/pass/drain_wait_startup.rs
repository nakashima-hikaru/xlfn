// module: excel_rtd/windows
use crate::excel_rtd::drain_wait::UnstartedDrain;

fn ordered() {
    let result = std::thread::scope(|scope| {
        let drain = UnstartedDrain::new(|| 7_u32);
        match drain.spawn(scope, std::thread::Builder::new(), || {}) {
            Ok(transferred) => transferred.join().into_output(),
            Err((_, unstarted)) => unstarted.into_work()(),
        }
    });
    assert_eq!(result, 7);
}
