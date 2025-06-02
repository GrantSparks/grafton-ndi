#[test]
fn test_ndi_environment() {
    println!("=== NDI Environment Diagnostics ===");

    // Print environment variables
    if let Ok(sdk_dir) = std::env::var("NDI_SDK_DIR") {
        println!("NDI_SDK_DIR: {}", sdk_dir);

        // Check if DLL directory exists
        let dll_path = format!("{}\\Bin\\x64", sdk_dir);
        println!("Checking DLL path: {}", dll_path);

        if std::path::Path::new(&dll_path).exists() {
            println!("✓ DLL directory exists");

            // List DLLs in the directory
            if let Ok(entries) = std::fs::read_dir(&dll_path) {
                println!("DLLs found:");
                for entry in entries.flatten() {
                    if let Some(name) = entry.file_name().to_str() {
                        if name.ends_with(".dll") {
                            println!("  - {}", name);
                        }
                    }
                }
            }
        } else {
            println!("✗ DLL directory does not exist!");
        }
    } else {
        println!("✗ NDI_SDK_DIR not set!");
    }

    // Check PATH
    if let Ok(path) = std::env::var("PATH") {
        println!("\nPATH contains NDI references:");
        for p in path.split(';') {
            if p.to_lowercase().contains("ndi") {
                println!("  - {}", p);
            }
        }
    }

    // Test CPU support
    println!("\nCPU Support: {}", grafton_ndi::NDI::is_supported_cpu());

    // Try to get version without initializing
    println!("\nAttempting to get NDI version...");
    match grafton_ndi::NDI::version() {
        Ok(v) => println!("NDI Version: {}", v),
        Err(e) => println!("Failed to get version: {}", e),
    }

    println!("=== End Diagnostics ===");
}

#[test]
#[ignore = "Manual diagnostic test"]
fn test_ndi_init_with_timeout() {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    println!("Testing NDI initialization with timeout...");

    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        println!("Thread: Attempting NDI::new()...");
        match grafton_ndi::NDI::new() {
            Ok(_) => {
                println!("Thread: NDI initialized successfully!");
                tx.send(Ok(())).unwrap();
            }
            Err(e) => {
                println!("Thread: NDI initialization failed: {}", e);
                tx.send(Err(e)).unwrap();
            }
        }
    });

    // Wait for up to 10 seconds
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => println!("Main: NDI initialized successfully"),
        Ok(Err(e)) => println!("Main: NDI initialization failed: {}", e),
        Err(_) => {
            println!("Main: NDI initialization timed out after 10 seconds!");
            println!("This suggests NDIlib_initialize() is hanging.");
        }
    }
}
