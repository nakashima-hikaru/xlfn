#[test]
fn trait_driven_function_signatures_compile() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/borrowed_inputs.rs");
    tests.pass("tests/ui/pass/case_distinct_udfs.rs");
    tests.pass("tests/ui/pass/cfg_gating.rs");
    tests.pass("tests/ui/pass/excel_enum.rs");
    tests.compile_fail("tests/ui/fail/argument_trait_missing.rs");
    tests.compile_fail("tests/ui/fail/context_*.rs");
    tests.compile_fail("tests/ui/fail/lookalike_*.rs");
    tests.compile_fail("tests/ui/fail/nested_return_array.rs");
    tests.compile_fail("tests/ui/fail/removed_export_macro.rs");
    tests.compile_fail("tests/ui/fail/return_trait_missing.rs");
    tests.compile_fail("tests/ui/fail/sys_module_is_not_public.rs");

    #[cfg(feature = "handles")]
    {
        tests.pass("tests/ui/pass/custom_conversions.rs");
        tests.pass("tests/ui/pass/renamed_crate.rs");
        tests.pass("tests/ui/pass/trait_roles.rs");
        tests.compile_fail("tests/ui/fail/formula_identity_missing.rs");
        tests.compile_fail("tests/ui/fail/handle_return_*.rs");
        tests.compile_fail("tests/ui/fail/nested_handle_storage.rs");
    }

    #[cfg(feature = "unstable-output")]
    tests.pass("tests/ui/pass_unstable/*.rs");

    #[cfg(feature = "async")]
    {
        tests.pass("tests/ui/pass_async/owned_matrix.rs");
        tests.compile_fail("tests/ui/fail_async/borrowed_array_alias.rs");
        tests.compile_fail("tests/ui/fail_async/borrowed_array_async.rs");
        tests.compile_fail("tests/ui/fail_async/borrowed_inputs.rs");

        #[cfg(feature = "handles")]
        {
            tests.pass("tests/ui/pass_async/async_handle.rs");
            tests.compile_fail("tests/ui/fail_async/async_handle_input.rs");
            tests.compile_fail("tests/ui/fail_async/handle_return_async.rs");
        }
    }

    #[cfg(not(feature = "async"))]
    tests.compile_fail("tests/ui/fail_no_async/*.rs");
}
