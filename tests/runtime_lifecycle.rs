mod common;

use grafton_ndi::{Runtime, RuntimeOptions, NDI};

// Keep this in its own integration-test process: runtime state is process-global.
#[test]
fn handles_keep_runtime_alive_until_last_drop_and_allow_reinitialization(
) -> Result<(), grafton_ndi::Error> {
    assert!(!NDI::is_running());
    let Some(original) = common::ndi() else {
        return Ok(());
    };
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
    let runtime = restarted.runtime();
    drop(restarted);
    assert!(!NDI::is_running());

    // The selection outlives every handle. It can still be inspected, and it
    // starts again without being selected anew.
    assert!(runtime.is_supported_cpu());
    assert!(runtime.info().version().is_some());
    let selected = Runtime::select(&RuntimeOptions::default())?;
    assert!(std::ptr::eq(selected.info(), runtime.info()));
    assert!(!NDI::is_running());
    let started = runtime.initialize()?;
    assert!(NDI::is_running());
    drop(started);
    assert!(!NDI::is_running());
    Ok(())
}
