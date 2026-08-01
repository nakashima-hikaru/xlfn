#[test]
fn trait_driven_function_signatures_compile() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/*.rs");
    tests.compile_fail("tests/ui/fail/*.rs");

    #[cfg(feature = "async")]
    {
        tests.pass("tests/ui/pass_async/*.rs");
        tests.compile_fail("tests/ui/fail_async/*.rs");
    }

    #[cfg(not(feature = "async"))]
    tests.compile_fail("tests/ui/fail_no_async/*.rs");
}
