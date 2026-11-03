#[test]
fn distribution_commit_requires_prepared_verification() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/prepared_distribution.rs");
    tests.compile_fail("tests/ui/fail/*.rs");
}
