use grafton_ndi::NDI;

// Keep this in its own integration-test process: runtime state is process-global.
#[test]
fn handles_keep_runtime_alive_until_last_drop_and_allow_reinitialization(
) -> Result<(), grafton_ndi::Error> {
    assert!(!NDI::is_running());
    let original = NDI::new()?;
    let cloned = original.clone();
    let independent = NDI::new()?;
    assert!(NDI::is_running());

    drop(original);
    assert!(NDI::is_running());
    drop(independent);
    assert!(NDI::is_running());
    // Clone the surviving handle after its original has been released.
    let survivor = cloned.clone();
    drop(cloned);
    assert!(NDI::is_running());
    drop(survivor);
    assert!(!NDI::is_running());

    let restarted = NDI::new()?;
    assert!(NDI::is_running());
    drop(restarted);
    assert!(!NDI::is_running());
    Ok(())
}
