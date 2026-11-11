//! Every function the Advanced-only headers declare must be classified.

use std::collections::BTreeMap;

use super::TABLES;

/// The routing and discovery modules resolve these by name.
const BY_NAME: &str = "resolved by name through `advanced::ffi::required`";

/// Declared functions that are in no table, with the reason.
const UNWRAPPED: &[(&str, &str)] = &[
    ("NDIlib_routing_create_v2", BY_NAME),
    ("NDIlib_routing_add_connection_metadata", BY_NAME),
    ("NDIlib_routing_clear_connection_metadata", BY_NAME),
    ("NDIlib_send_advertiser_create_ex", BY_NAME),
    ("NDIlib_recv_advertiser_create_ex", BY_NAME),
    ("NDIlib_send_listener_create_ex", BY_NAME),
    ("NDIlib_recv_listener_create_ex", BY_NAME),
];

/// Names of the foreign functions the installed Advanced headers declare.
fn declared_functions() -> Vec<String> {
    let declarations = include_str!(concat!(env!("OUT_DIR"), "/ndi_advanced_declarations.rs"));
    let mut names = Vec::new();
    for item in syn::parse_file(declarations)
        .expect("generated Advanced declarations parse")
        .items
    {
        let syn::Item::ForeignMod(block) = item else {
            continue;
        };
        for item in block.items {
            if let syn::ForeignItem::Fn(function) = item {
                names.push(function.sig.ident.to_string());
            }
        }
    }
    names
}

#[test]
fn every_declared_function_is_in_one_table_or_explicitly_unwrapped() {
    // Where each classified name lives; inserting twice is a duplicate.
    let mut classified = BTreeMap::new();
    let mut classify = |name: String, owner: String| {
        if let Some(earlier) = classified.insert(name.clone(), owner.clone()) {
            panic!("{name} is classified twice: {earlier} and {owner}");
        }
    };
    for (table, symbols) in TABLES {
        for symbol in *symbols {
            classify(
                symbol.to_str().unwrap().to_owned(),
                format!("table {table}"),
            );
        }
    }
    for (name, reason) in UNWRAPPED {
        assert!(!reason.is_empty());
        classify((*name).to_owned(), "the unwrapped list".to_owned());
    }

    let declared = declared_functions();
    assert!(!declared.is_empty());
    for name in &declared {
        assert!(
            classified.remove(name).is_some(),
            "{name} is declared by the Advanced SDK headers but is in no table and not in UNWRAPPED"
        );
    }
    assert!(
        classified.is_empty(),
        "classified names the Advanced SDK headers do not declare: {classified:?}"
    );
}
