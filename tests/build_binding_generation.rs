#[path = "../build_bindings.rs"]
mod build_bindings;

use std::collections::BTreeSet;

#[test]
fn formatted_and_token_stream_declarations_have_identical_functions() {
    let formatted = r#"
        unsafe extern "C" {
            #[deprecated(note = "synthetic fixture")]
            pub fn NDIlib_alpha(value: i32) -> bool;
            pub fn NDIlib_beta(
                callback: Option<unsafe extern "C" fn(i32)>,
            );
        }
    "#;
    let compact = r#"unsafe extern "C" { # [ deprecated ( note = "synthetic fixture" ) ] pub fn NDIlib_alpha ( value : i32 ) -> bool ; pub fn NDIlib_beta ( callback : Option < unsafe extern "C" fn ( i32 ) > , ) ; }"#;
    let expected = BTreeSet::from(["NDIlib_alpha".to_owned(), "NDIlib_beta".to_owned()]);
    assert_eq!(
        build_bindings::foreign_functions(formatted).unwrap(),
        expected
    );
    assert_eq!(
        build_bindings::foreign_functions(compact).unwrap(),
        expected
    );
}

#[test]
fn only_public_foreign_functions_in_the_generated_root_are_selected() {
    let source = r#"
        // pub fn NDIlib_comment();
        const TEXT: &str = "pub fn NDIlib_string();";
        pub type NDIlib_callback = Option<unsafe extern "C" fn()>;
        pub fn NDIlib_rust_definition() {}
        extern "C" {
            pub static NDIlib_variable: i32;
            fn NDIlib_private();
            pub(crate) fn NDIlib_crate_visible();
            pub fn unrelated_function();
            pub fn NDIlib_actual();
        }
        mod unrelated { extern "C" { pub fn NDIlib_nested(); } }
    "#;
    assert_eq!(
        build_bindings::foreign_functions(source).unwrap(),
        BTreeSet::from(["NDIlib_actual".to_owned()])
    );
}

#[test]
fn malformed_bindings_fail_instead_of_publishing_a_partial_address_map() {
    assert!(build_bindings::foreign_functions(
        "extern \"C\" { pub fn NDIlib_partial(); malformed tokens }"
    )
    .is_err());
}

#[test]
fn static_map_requires_both_a_declaration_and_an_actual_archive_export() {
    let source = r#"extern "C" {
        pub fn NDIlib_initialize(); pub fn NDIlib_destroy(); pub fn NDIlib_version();
        pub fn NDIlib_optional();
    }"#;
    let essential = BTreeSet::from([
        "NDIlib_initialize".to_owned(),
        "NDIlib_destroy".to_owned(),
        "NDIlib_version".to_owned(),
    ]);
    let essential_exports = essential.iter().cloned().collect();
    let mut exports: std::collections::HashSet<String> = essential.iter().cloned().collect();
    exports.insert("NDIlib_not_declared".to_owned());
    assert_eq!(
        build_bindings::static_functions(source, &exports).unwrap(),
        essential
    );
    exports.remove("NDIlib_version");
    assert!(build_bindings::static_functions(source, &exports)
        .unwrap_err()
        .contains("NDIlib_version"));
    let missing_declaration = source.replace("pub fn NDIlib_initialize();", "");
    assert!(
        build_bindings::static_functions(&missing_declaration, &essential_exports)
            .unwrap_err()
            .contains("NDIlib_initialize")
    );
    assert!(build_bindings::static_functions("", &essential_exports).is_err());
}
