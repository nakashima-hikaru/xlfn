use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=BENCH_EXTRA_FUNCTIONS");
    let count: usize = env::var("BENCH_EXTRA_FUNCTIONS")
        .unwrap_or_else(|_| "0".into())
        .parse()
        .expect("BENCH_EXTRA_FUNCTIONS must be an integer");
    assert!(
        count <= 5_000,
        "registration fixture is capped at 5,000 extras"
    );
    let mut source = String::new();
    for index in 0..count {
        source.push_str(&format!(
            "#[excel_function(name = \"BENCH.EXTRA.{index:04}\", thread_safe)]\n\
             pub fn bench_extra_{index:04}(value: f64) -> f64 {{ value }}\n"
        ));
    }
    let destination = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("extra.rs");
    fs::write(destination, source).unwrap();
}
