use grafton_ndi::{
    NDIlib_destroy, NDIlib_find_create_t, NDIlib_find_create_v2, NDIlib_find_destroy,
    NDIlib_find_get_current_sources, NDIlib_find_wait_for_sources, NDIlib_initialize,
};
use std::ffi::CStr;
use std::ptr;
use std::time::{Duration, Instant};

fn main() {
    unsafe {
        println!("Initializing NDI library...");

        if !NDIlib_initialize() {
            eprintln!("Failed to initialize NDI library.");
            return;
        }
        println!("NDI library initialized successfully.");

        // Generate the IP addresses within the additional range to include in our search
        let extra_ips: Vec<String> = (107..=111).map(|i| format!("192.168.0.{}", i)).collect();
        let extra_ips_cstr = extra_ips.join(",");

        // Initialize the find_create_instance
        let find_create_instance = NDIlib_find_create_t {
            show_local_sources: false,
            p_groups: ptr::null(),
            p_extra_ips: extra_ips_cstr.as_ptr() as *const i8,
        };

        println!("Creating NDI find instance...");
        let p_ndi_find = NDIlib_find_create_v2(&find_create_instance);
        if p_ndi_find.is_null() {
            eprintln!("Failed to create NDI find instance.");
            return;
        }
        println!("NDI find instance created successfully.");

        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(60) {
            println!("Waiting for sources (timeout 5000 ms)...");
            if !NDIlib_find_wait_for_sources(p_ndi_find, 5000) {
                println!("No change to the sources found.");
                continue;
            }
            println!("Sources have changed.");

            let mut no_sources: u32 = 0;
            let p_sources = NDIlib_find_get_current_sources(p_ndi_find, &mut no_sources);
            println!("Network sources ({} found).", no_sources);

            for i in 0..no_sources {
                let source = *p_sources.add(i as usize);
                let source_name = CStr::from_ptr(source.p_ndi_name)
                    .to_str()
                    .unwrap_or("Unknown");
                println!("{}. {}", i + 1, source_name);
            }
        }

        println!("Destroying NDI find instance...");
        NDIlib_find_destroy(p_ndi_find);
        println!("NDI find instance destroyed.");

        println!("Destroying NDI library...");
        NDIlib_destroy();
        println!("NDI library destroyed. Program finished.");
    }
}
