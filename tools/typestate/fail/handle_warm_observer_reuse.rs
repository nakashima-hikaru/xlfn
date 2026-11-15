// module: handle/runtime
use super::{FormulaHandleService, HandleTopicKey};

fn invalid<F>(service: &FormulaHandleService, key: HandleTopicKey, observe: F)
where
    F: FnOnce(&str, &str) -> crate::XllResult<()>,
{
    let _ = service.prepare_warm(key, observe);
    let _ = service.prepare_warm(key, observe); // error: E0382
}
