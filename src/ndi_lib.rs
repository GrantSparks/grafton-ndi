#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]
#![allow(unused_imports)]

include!(concat!(env!("OUT_DIR"), "/ndi_lib.rs"));

// NOTE: NDI Advanced SDK 6.1.1+ provides NDIlib_send_set_video_async_completion
// This function is not available in the standard SDK. The code is ready to use it
// when building against the Advanced SDK.
