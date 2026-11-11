use super::*;
use crate::ndi_lib as c;
use std::mem::{align_of, offset_of, size_of};

#[test]
fn advertiser_private_create_matches_both_installed_c_types() {
    assert_eq!(
        size_of::<Create>(),
        size_of::<c::NDIlib_send_advertiser_create_t>()
    );
    assert_eq!(
        align_of::<Create>(),
        align_of::<c::NDIlib_send_advertiser_create_t>()
    );
    assert_eq!(
        offset_of!(Create, url),
        offset_of!(c::NDIlib_send_advertiser_create_t, p_url_address)
    );
    assert_eq!(
        size_of::<Create>(),
        size_of::<c::NDIlib_recv_advertiser_create_t>()
    );
    assert_eq!(
        align_of::<Create>(),
        align_of::<c::NDIlib_recv_advertiser_create_t>()
    );
    assert_eq!(
        offset_of!(Create, url),
        offset_of!(c::NDIlib_recv_advertiser_create_t, p_url_address)
    );
}
