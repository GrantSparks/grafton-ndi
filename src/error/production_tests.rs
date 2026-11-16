//! Keep public causes tied to production constructors rather than test goldens.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use syn::{
    punctuated::Punctuated,
    visit::{self, Visit},
    Attribute, Item, Meta, Token,
};

const PAYLOADS: &[&str] = &[
    "FrameDefect",
    "InputDefect",
    "LayoutDefect",
    "SdkResultDefect",
    "DiscoveryDefect",
];

fn production_cfg(meta: &Meta) -> bool {
    match meta {
        Meta::Path(path) => !path.is_ident("test"),
        Meta::List(list) if list.path.is_ident("all") || list.path.is_ident("any") => {
            let Ok(children) =
                list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            else {
                return true;
            };
            if list.path.is_ident("all") {
                children.iter().all(production_cfg)
            } else {
                children.iter().any(production_cfg)
            }
        }
        // Other conditions can hold on some production feature/target. In
        // particular not(test) is included, as are native ABI cfg predicates.
        _ => true,
    }
}

fn production(attrs: &[Attribute]) -> bool {
    attrs.iter().all(|attr| {
        !attr.path().is_ident("cfg")
            || attr
                .parse_args::<Meta>()
                .map_or(true, |meta| production_cfg(&meta))
    })
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

fn expression_attrs(expression: &syn::Expr) -> &[Attribute] {
    macro_rules! attrs {
        ($($kind:ident),+ $(,)?) => {
            match expression {
                $(syn::Expr::$kind(expression) => &expression.attrs,)+
                // Verbatim tokens have no parsed attributes or constructors.
                _ => &[],
            }
        };
    }
    attrs!(
        Array, Assign, Async, Await, Binary, Block, Break, Call, Cast, Closure, Const, Continue,
        Field, ForLoop, Group, If, Index, Infer, Let, Lit, Loop, Macro, Match, MethodCall, Paren,
        Path, Range, RawAddr, Reference, Repeat, Return, Struct, Try, TryBlock, Tuple, Unary,
        Unsafe, While, Yield,
    )
}

#[derive(Default)]
struct Producers {
    found: BTreeSet<(String, String)>,
    module_dir: PathBuf,
    file_dir: PathBuf,
}

impl Producers {
    fn scan(&mut self, path: &Path) {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let syntax =
            syn::parse_file(&source).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let file_dir = path.parent().unwrap().to_path_buf();
        let module_dir = match path.file_name().and_then(|name| name.to_str()) {
            Some("lib.rs" | "mod.rs") => file_dir.clone(),
            _ => file_dir.join(path.file_stem().unwrap()),
        };
        let previous = (
            std::mem::replace(&mut self.module_dir, module_dir),
            std::mem::replace(&mut self.file_dir, file_dir),
        );
        self.visit_file(&syntax);
        (self.module_dir, self.file_dir) = previous;
    }

    fn record(&mut self, path: &syn::Path) {
        let names: Vec<_> = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        for pair in names.windows(2) {
            if PAYLOADS.contains(&pair[0].as_str()) {
                self.found.insert((pair[0].clone(), pair[1].clone()));
            }
        }
    }
}

impl<'ast> Visit<'ast> for Producers {
    fn visit_pat(&mut self, _: &'ast syn::Pat) {
        // A match arm can mention a public cause without ever producing it.
    }

    fn visit_item(&mut self, item: &'ast Item) {
        if production(item_attrs(item)) {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let attrs = match item {
            syn::ImplItem::Const(item) => &item.attrs,
            syn::ImplItem::Fn(item) => &item.attrs,
            syn::ImplItem::Type(item) => &item.attrs,
            syn::ImplItem::Macro(item) => &item.attrs,
            _ => return,
        };
        if production(attrs) {
            visit::visit_impl_item(self, item);
        }
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        let attrs = match item {
            syn::TraitItem::Const(item) => &item.attrs,
            syn::TraitItem::Fn(item) => &item.attrs,
            syn::TraitItem::Type(item) => &item.attrs,
            syn::TraitItem::Macro(item) => &item.attrs,
            _ => return,
        };
        if production(attrs) {
            visit::visit_trait_item(self, item);
        }
    }

    fn visit_expr(&mut self, expression: &'ast syn::Expr) {
        if production(expression_attrs(expression)) {
            visit::visit_expr(self, expression);
        }
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if module.content.is_some() {
            let previous = self.module_dir.clone();
            self.module_dir.push(module.ident.to_string());
            visit::visit_item_mod(self, module);
            self.module_dir = previous;
        } else {
            let explicit = module.attrs.iter().find_map(|attr| {
                if let Meta::NameValue(value) = &attr.meta {
                    if value.path.is_ident("path") {
                        if let syn::Expr::Lit(value) = &value.value {
                            if let syn::Lit::Str(value) = &value.lit {
                                return Some(self.file_dir.join(value.value()));
                            }
                        }
                    }
                }
                None
            });
            let path = explicit.unwrap_or_else(|| {
                let leaf = self.module_dir.join(format!("{}.rs", module.ident));
                if leaf.is_file() {
                    leaf
                } else {
                    self.module_dir
                        .join(module.ident.to_string())
                        .join("mod.rs")
                }
            });
            // ABI modules generated in OUT_DIR are not source files and carry
            // no application error constructors.
            if path.is_file() {
                self.scan(&path);
            }
        }
    }

    fn visit_expr_path(&mut self, expression: &'ast syn::ExprPath) {
        self.record(&expression.path);
        visit::visit_expr_path(self, expression);
    }

    fn visit_expr_struct(&mut self, expression: &'ast syn::ExprStruct) {
        self.record(&expression.path);
        visit::visit_expr_struct(self, expression);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // native!(Capability, table, uses(...) => { ... }) contains ordinary
        // Rust after its dispatch prefix. Parse it so patterns and comments
        // cannot masquerade as constructor expressions.
        if mac.path.is_ident("native") {
            if let Some((_, body)) = mac.tokens.to_string().split_once("=>") {
                let expression =
                    syn::parse_str::<syn::Expr>(body).expect("native dispatch body is Rust");
                self.visit_expr(&expression);
            }
        }
    }
}

#[test]
fn every_public_defect_has_a_production_construction_site() {
    let mut producers = Producers::default();
    producers.scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"));
    let declarations = syn::parse_file(include_str!("defects.rs")).unwrap();
    let mut missing = Vec::new();
    for item in declarations.items {
        if let Item::Enum(item) = item {
            if PAYLOADS.iter().any(|name| item.ident == name) {
                for variant in item.variants {
                    let cause = (item.ident.to_string(), variant.ident.to_string());
                    if !producers.found.contains(&cause) {
                        missing.push(format!("{}::{}", cause.0, cause.1));
                    }
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "public causes without production constructors: {}",
        missing.join(", ")
    );
}

#[test]
fn test_only_items_and_match_patterns_do_not_count_as_producers() {
    let syntax = syn::parse_file(r#"
        fn production() { let _ = FrameDefect::NullVideoData; }
        #[cfg(test)] fn test_only() { let _ = FrameDefect::NullAudioData; }
        #[cfg(all(test, feature = "advanced"))]
        mod more_tests { fn helper() { let _ = FrameDefect::NullAudioData; } }
        struct Owner;
        impl Owner {
            #[cfg(test)] const ONLY_IN_TESTS: () = { let _ = FrameDefect::NullAudioData; };
        }
        trait Contract {
            #[cfg(test)] const ONLY_IN_TESTS: () = { let _ = FrameDefect::NullAudioData; };
        }
        fn expression_only() {
            #[cfg(test)] { let _ = FrameDefect::NullAudioData; }
        }
        fn match_only(value: FrameDefect) { match value { FrameDefect::NullAudioData => {}, _ => {} } }
    "#).unwrap();
    let mut producers = Producers::default();
    producers.visit_file(&syntax);
    assert_eq!(
        producers.found,
        BTreeSet::from([("FrameDefect".into(), "NullVideoData".into())])
    );
}
