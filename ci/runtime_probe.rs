fn main() -> Result<(), Box<dyn std::error::Error>> {
    let expected = std::env::var_os("NDI_EXPECTED_RUNTIME")
        .ok_or("NDI_EXPECTED_RUNTIME must identify the exact runtime under test")?;
    let ndi = grafton_ndi::NDI::new()?;
    let info = ndi.info();
    let actual = info
        .library_path()
        .ok_or("expected a shared runtime module")?;
    if std::fs::canonicalize(actual)? != std::fs::canonicalize(expected)? {
        return Err("the loaded runtime differs from the selected SDK artifact".into());
    }
    println!("Runtime version: {}", info.raw_version());
    println!("Runtime edition: {:?}", info.edition());
    println!("Runtime edition verified: {}", info.edition_verified());
    let version = info.version().ok_or("runtime version is not recognized")?;
    let edition = match info.edition() {
        grafton_ndi::SdkEdition::Standard => "standard",
        grafton_ndi::SdkEdition::Advanced => "advanced",
        _ => return Err("runtime edition is not recognized".into()),
    };
    println!(
        "NDI_RUNTIME_IDENTITY=1|{}.{}.{}.{}|{}|{}",
        version.major(),
        version.minor(),
        version.patch(),
        version.build(),
        edition,
        info.edition_verified()
    );
    Ok(())
}
