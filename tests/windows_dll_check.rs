#[cfg(target_os = "windows")]
#[test]
#[ignore = "Requires real NDI SDK and reviewed network confinement"]
fn test_ndi_dll_availability() {
    use std::{os::windows::ffi::OsStrExt, path::PathBuf};

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(lpFileName: *const u16) -> *mut std::ffi::c_void;
        fn GetLastError() -> u32;
        fn FreeLibrary(hModule: *mut std::ffi::c_void) -> i32;
    }

    // The confined runner supplies the exact edition/architecture selected by
    // its verified SDK receipt. Manual runs must make that selection explicit.
    let path = PathBuf::from(
        std::env::var_os("NDI_EXPECTED_RUNTIME")
            .expect("set NDI_EXPECTED_RUNTIME to the absolute selected NDI DLL path"),
    );
    assert!(path.is_absolute(), "selected NDI DLL path must be absolute");
    assert!(path.is_file(), "selected NDI DLL must exist");
    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();

    // SAFETY: wide_path is a terminated UTF-16 path and stays alive for the call.
    let handle = unsafe { LoadLibraryW(wide_path.as_ptr()) };
    if handle.is_null() {
        let error = unsafe { GetLastError() };
        panic!("selected NDI DLL failed to load (Windows error {error})");
    }
    // Release the successful load before any assertion or diagnostic can panic.
    let released = unsafe { FreeLibrary(handle) };
    if released == 0 {
        let error = unsafe { GetLastError() };
        panic!("selected NDI DLL failed to unload (Windows error {error})");
    }
    println!("Selected NDI DLL loaded and released successfully");
}

#[cfg(not(target_os = "windows"))]
#[test]
#[ignore = "Requires real NDI SDK and reviewed network confinement"]
fn test_ndi_dll_availability() {
    // Explicit no-op: the Windows loader API does not exist on this host.
}
