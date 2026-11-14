#[test]
fn compile_contracts() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass/*.rs");
    t.compile_fail("tests/ui/fail/*.rs");
    #[cfg(feature = "file-io")]
    t.compile_fail("tests/ui/file/fail/*.rs");
    #[cfg(any(feature = "tokio", feature = "smol"))]
    t.compile_fail("tests/ui/async/fail/*.rs");
}
