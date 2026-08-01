#[cfg(feature = "sdk-bindgen")]
fn main() {
    use std::env;
    use std::path::{Path, PathBuf};

    println!("cargo:rerun-if-env-changed=XLFN_SDK_INCLUDE");
    println!("cargo:rerun-if-changed=bindgen-wrapper.h");
    println!("cargo:rerun-if-changed=native/callback_trampoline.cpp");

    let sdk_include = PathBuf::from(
        env::var_os("XLFN_SDK_INCLUDE").expect("sdk-bindgen requires XLFN_SDK_INCLUDE"),
    );
    let header = sdk_include.join("XLCALL.H");
    assert!(
        header.is_file(),
        "Excel SDK header not found: {}",
        header.display()
    );

    let target = env::var("TARGET").expect("Cargo always sets TARGET for build scripts");
    let bindings = bindgen::Builder::default()
        .header("bindgen-wrapper.h")
        .clang_arg(format!("-I{}", sdk_include.display()))
        .clang_arg(format!("--target={target}"))
        .allowlist_type("^(IDSHEET|XLOPER12|XLREF12|XLMREF12)$")
        .allowlist_var("^(xl|xlf|xlerr|xlbit|xlevent).*")
        .derive_default(false)
        .generate_comments(false)
        .layout_tests(false)
        .generate()
        .expect("bindgen could not parse XLCALL.H");

    let output =
        Path::new(&env::var_os("OUT_DIR").expect("Cargo always sets OUT_DIR")).join("xlcall.rs");
    bindings
        .write_to_file(output)
        .expect("failed to write generated Excel SDK bindings");

    let mut trampoline = cc::Build::new();
    trampoline
        .cpp(true)
        .file("native/callback_trampoline.cpp")
        .include(&sdk_include);
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        trampoline.flag("/W4").flag("/WX").flag("/permissive-");
    }
    trampoline.compile("xlfn_callback_probe");
}

#[cfg(not(feature = "sdk-bindgen"))]
fn main() {}
