#[cfg(target_os = "windows")]
#[test]
fn test_ndi_dll_availability() {
    use std::os::windows::ffi::OsStrExt;

    // Windows API bindings
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(lpFileName: *const u16) -> *mut std::ffi::c_void;
        fn GetLastError() -> u32;
        fn FreeLibrary(hModule: *mut std::ffi::c_void) -> i32;
    }

    fn to_wide_string(s: &str) -> Vec<u16> {
        use std::ffi::OsStr;
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    println!("=== Windows DLL Availability Check ===");

    // List of DLLs to check
    let dlls = if cfg!(target_arch = "x86_64") {
        vec!["Processing.NDI.Lib.x64.dll"]
    } else {
        vec!["Processing.NDI.Lib.x86.dll"]
    };

    for dll_name in dlls {
        print!("Checking {}: ", dll_name);

        let wide_name = to_wide_string(dll_name);
        let handle = unsafe { LoadLibraryW(wide_name.as_ptr()) };

        if handle.is_null() {
            let error = unsafe { GetLastError() };
            println!("❌ FAILED (Error code: {})", error);

            // Common error codes:
            // 126 = ERROR_MOD_NOT_FOUND - The specified module could not be found
            // 127 = ERROR_PROC_NOT_FOUND - The specified procedure could not be found
            match error {
                126 => println!("  → DLL not found in PATH or application directory"),
                127 => println!("  → DLL found but has missing dependencies"),
                _ => println!("  → Unknown error"),
            }

            // Try to provide the full PATH for debugging
            if let Ok(path) = std::env::var("PATH") {
                println!("  → Current PATH entries with 'ndi':");
                for p in path.split(';') {
                    if p.to_lowercase().contains("ndi") {
                        println!("    - {}", p);

                        // Check if the DLL exists in this path
                        let dll_path = std::path::Path::new(p).join(dll_name);
                        if dll_path.exists() {
                            println!("      ✓ {} exists here", dll_name);
                        }
                    }
                }
            }
        } else {
            println!("✅ OK");
            unsafe { FreeLibrary(handle) };
        }
    }

    println!("=== End DLL Check ===");
}

#[cfg(not(target_os = "windows"))]
#[test]
fn test_ndi_dll_availability() {
    // Skip on non-Windows platforms
}
