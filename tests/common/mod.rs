//! Select focused helper groups for one integration test binary.
//!
//! Import this module with `#[macro_use]`, then write, for example,
//! `common_helpers!(runtime, name, loopback);`. The expansion includes only
//! those modules, so the compiler checks every included helper for actual use.
//! `runtime` supplies prerequisite reporting and runtime identity checks;
//! `policy` selects prerequisite reporting without loading a runtime.
//! `advanced`, `file`, `hx`, and `identity` extend `runtime`; `prerequisites`
//! extends either policy selection, and `endpoint` extends `loopback`.

macro_rules! common_helpers {
    ($($group:ident),+ $(,)?) => {
        mod common {
            $(common_helpers!(@ $group);)+
        }
    };
    (@ runtime) => {
        use grafton_ndi as api;
        #[path = "../../src/test_support/policy.rs"]
        mod policy;
        #[path = "../../src/test_support/identity.rs"]
        mod identity;
        #[path = "../../src/test_support/runtime.rs"]
        mod runtime;
        pub use runtime::ndi;
    };
    (@ policy) => {
        #[path = "../../src/test_support/policy.rs"]
        mod policy;
    };
    (@ prerequisites) => {
        pub use policy::{skip, Requirement};
    };
    (@ identity) => {
        pub use identity::assert_expected_identity;
    };
    (@ name) => {
        #[path = "../../src/test_support/unique_name.rs"]
        mod unique_name;
        pub use unique_name::unique_name;
    };
    (@ advanced) => {
        #[path = "../../src/test_support/advanced_runtime.rs"]
        mod advanced_runtime;
        pub use advanced_runtime::advanced_ndi;
    };
    (@ file) => {
        #[path = "../../src/test_support/file_api.rs"]
        mod file_api;
        pub use file_api::file_api;
    };
    (@ hx) => {
        #[path = "../../src/test_support/hx_decoder.rs"]
        mod hx_decoder;
        #[path = "../../src/test_support/hx_placeholder.rs"]
        mod hx_placeholder;
        pub use hx_decoder::hx_decoder;
        pub use hx_placeholder::is_hx_placeholder;
    };
    (@ unsupported) => {
        #[path = "../../src/test_support/unsupported.rs"]
        mod unsupported;
        pub use unsupported::unsupported;
    };
    (@ loopback) => {
        mod loopback;
        pub use loopback::loopback_source;
    };
    (@ endpoint) => {
        pub use loopback::loopback_endpoint;
    };
    (@ socket) => {
        mod socket;
        pub use socket::{port_of, socket_table};
    };
    (@ discovery_server) => {
        #[cfg(all(target_os = "linux", not(ndi_link_static)))]
        pub mod discovery_server;
    };
}
