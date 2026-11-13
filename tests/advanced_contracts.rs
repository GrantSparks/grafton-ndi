//! Ownership contracts remain identical with Standard or Advanced build headers.
#![cfg(feature = "advanced")]

#[test]
fn advanced_ownership_contracts() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/advanced/pass/*.rs");
    cases.compile_fail("tests/ui/advanced/fail/*.rs");
}
