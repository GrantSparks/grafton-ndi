#[test]
fn compile_contracts() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass/*.rs");
    t.compile_fail("tests/ui/fail/*.rs");
    // Advanced adds another sealed audio implementation to rustc's help text.
    // Keep the same implementation proof with an explicit expectation per set.
    #[cfg(feature = "advanced")]
    t.compile_fail("tests/ui/sealed/advanced.rs");
    #[cfg(not(feature = "advanced"))]
    t.compile_fail("tests/ui/sealed/default.rs");
    #[cfg(feature = "file-io")]
    t.compile_fail("tests/ui/file/fail/*.rs");
    #[cfg(any(feature = "tokio", feature = "smol"))]
    t.compile_fail("tests/ui/async/fail/*.rs");
}
