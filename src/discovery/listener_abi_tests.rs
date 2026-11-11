use super::*;
use crate::ndi_lib as c;
use std::mem::{align_of, offset_of, size_of};

macro_rules! layout {
    ($rust:ty, $native:ty; $($r:ident => $n:ident),+ $(,)?) => {{
        assert_eq!(size_of::<$rust>(), size_of::<$native>(), stringify!($native));
        assert_eq!(align_of::<$rust>(), align_of::<$native>(), stringify!($native));
        $(assert_eq!(offset_of!($rust, $r), offset_of!($native, $n), concat!(stringify!($native), ".", stringify!($n)));)+
    }};
}

#[test]
fn listener_private_boundaries_match_installed_c_types() {
    layout!(Create, c::NDIlib_send_listener_create_t; url => p_url_address);
    layout!(Create, c::NDIlib_recv_listener_create_t; url => p_url_address);
    layout!(RawEvent, c::NDIlib_listener_event; uuid => p_uuid, name => p_name, value => p_value);
    layout!(RawSender, c::NDIlib_sender_t;
        uuid => p_uuid, name => p_name, metadata => p_metadata, address => p_address,
        port => port, groups => p_groups, num_groups => num_groups, subscribed => events_subscribed);
    layout!(RawReceiver, c::NDIlib_receiver_t;
        uuid => p_uuid, name => p_name, input_uuid => p_input_uuid, input_name => p_input_name,
        address => p_address, streams => p_streams, num_streams => num_streams,
        commands => p_commands, num_commands => num_commands, subscribed => events_subscribed);
    assert_eq!(size_of::<u32>(), size_of::<c::NDIlib_receiver_type_e>());
    assert_eq!(align_of::<u32>(), align_of::<c::NDIlib_receiver_type_e>());
    assert_eq!(size_of::<u32>(), size_of::<c::NDIlib_receiver_command_e>());
    assert_eq!(
        align_of::<u32>(),
        align_of::<c::NDIlib_receiver_command_e>()
    );
}

#[test]
fn discovery_values_match_c_constants_and_preserve_unknowns() {
    assert_eq!(
        ReceiverStream::from(u32::from_ne_bytes(
            c::NDIlib_receiver_type_e_NDIlib_receiver_type_none.to_ne_bytes()
        )),
        ReceiverStream::None
    );
    assert_eq!(
        ReceiverStream::from(u32::from_ne_bytes(
            c::NDIlib_receiver_type_e_NDIlib_receiver_type_metadata.to_ne_bytes()
        )),
        ReceiverStream::Metadata
    );
    assert_eq!(
        ReceiverStream::from(u32::from_ne_bytes(
            c::NDIlib_receiver_type_e_NDIlib_receiver_type_video.to_ne_bytes()
        )),
        ReceiverStream::Video
    );
    assert_eq!(
        ReceiverStream::from(u32::from_ne_bytes(
            c::NDIlib_receiver_type_e_NDIlib_receiver_type_audio.to_ne_bytes()
        )),
        ReceiverStream::Audio
    );
    assert_eq!(
        ReceiverCommand::from(u32::from_ne_bytes(
            c::NDIlib_receiver_command_e_NDIlib_receiver_command_none.to_ne_bytes()
        )),
        ReceiverCommand::None
    );
    assert_eq!(
        ReceiverCommand::from(u32::from_ne_bytes(
            c::NDIlib_receiver_command_e_NDIlib_receiver_command_connect.to_ne_bytes()
        )),
        ReceiverCommand::Connect
    );
    assert_eq!(
        ReceiverStream::from(u32::MAX),
        ReceiverStream::Unknown(u32::MAX)
    );
    assert_eq!(
        ReceiverCommand::from(u32::MAX),
        ReceiverCommand::Unknown(u32::MAX)
    );
}
