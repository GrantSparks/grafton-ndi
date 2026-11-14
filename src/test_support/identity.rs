//! Check that the loaded runtime matches the provisioned SDK.

use super::api::capabilities;

/// Assert the identity promised by SDK provisioning against the module loaded
/// in this process. These values are separate from header selection, allowing
/// builds with newer headers to be tested against an older runtime.
pub fn assert_expected_identity(info: &capabilities::RuntimeInfo) {
    let expected =
        |name: &str| std::env::var(name).unwrap_or_else(|error| panic!("{name}: {error}"));
    if std::env::var_os("GRAFTON_NDI_EXPECTED_VERSION").is_some() {
        let actual = info
            .version()
            .expect("the selected SDK version must be recognized");
        assert_eq!(
            actual.to_string(),
            expected("GRAFTON_NDI_EXPECTED_VERSION"),
            "the loaded runtime differs from the provisioned SDK version"
        );
    }
    if std::env::var_os("GRAFTON_NDI_EXPECTED_EDITION").is_some() {
        let actual = if info.edition() == capabilities::SdkEdition::Standard {
            "standard"
        } else if info.edition() == capabilities::SdkEdition::Advanced {
            "advanced"
        } else {
            panic!("the selected SDK edition must be recognized")
        };
        assert_eq!(
            actual,
            expected("GRAFTON_NDI_EXPECTED_EDITION"),
            "the loaded runtime differs from the provisioned SDK edition"
        );
    }
    if let Some(expected) = std::env::var_os("GRAFTON_NDI_EXPECTED_RUNTIME") {
        let actual = info
            .library_path()
            .expect("the provisioned SDK is a shared runtime");
        assert_eq!(
            actual.canonicalize().expect("the loaded module exists"),
            std::path::Path::new(&expected)
                .canonicalize()
                .expect("the provisioned runtime exists"),
            "the test loaded a different module than SDK provisioning selected"
        );
    }
}
