//! The room the crate keeps after the frames it sends
//! (`PixelFormat::send_tail_room`), against a real runtime.
//!
//! The x86 builds of the SDK read past the last pixel of many UYVY and UYVA
//! frames; the build for 64-bit ARM Linux reads every frame exactly. One test
//! sends frames from memory that ends where an inaccessible page begins, with
//! exactly the room the crate says the SDK needs and with none: a read
//! further than that ends the test process with a segmentation fault. The
//! other pins what the SDK does with the bytes after a frame, and shows that
//! bytes after a caller's buffer reach the picture only when the caller
//! offers them as room.
//!
//! Frames hold slowly varying levels, as pictures do.
#![cfg(target_os = "linux")]

mod common;

// Standard 6.1.1 produces different lossy pixels for identical initialized
// submissions when this binary's guard-page stress sender runs concurrently.
// Keep the native workload serial so equality still detects leaked padding;
// SDK-independent descriptor/initialization tests verify the wrapper directly.
static NATIVE_WORKLOAD: std::sync::Mutex<()> = std::sync::Mutex::new(());

use grafton_ndi::{
    BorrowedVideoFrame, Ndi, PixelFormat, Receiver, ReceiverColorFormat, ReceiverOptions, ScanType,
    Sender, SenderOptions,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// `len` bytes that end where an inaccessible page begins.
struct Guarded {
    mapping: *mut libc::c_void,
    mapped: usize,
    data: *mut u8,
    len: usize,
}

impl Guarded {
    fn new(len: usize) -> Self {
        // SAFETY: plain queries and a fresh private mapping, whose last page
        // alone changes protection.
        unsafe {
            let page = usize::try_from(libc::sysconf(libc::_SC_PAGESIZE)).unwrap();
            let body = len.div_ceil(page) * page;
            let mapped = body + page;
            let mapping = libc::mmap(
                std::ptr::null_mut(),
                mapped,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert_ne!(mapping, libc::MAP_FAILED);
            let guard = mapping.cast::<u8>().add(body);
            assert_eq!(libc::mprotect(guard.cast(), page, libc::PROT_NONE), 0);
            Self {
                mapping,
                mapped,
                data: guard.sub(len),
                len,
            }
        }
    }

    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: `len` readable, writable bytes of the mapping this owns,
        // zeroed by the kernel, borrowed with `self`.
        unsafe { std::slice::from_raw_parts_mut(self.data, self.len) }
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        // SAFETY: the mapping made in `new`, which nothing borrows now.
        unsafe { libc::munmap(self.mapping, self.mapped) };
    }
}

/// Legal video levels that vary slowly, as pictures do.
fn smooth(bytes: &mut [u8]) {
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = 64 + (i >> 10) as u8 % 128;
    }
}

fn sender(ndi: &Ndi, label: &str) -> Sender {
    Sender::new(
        ndi,
        &SenderOptions::new(common::unique_name(label)).clock_video(false),
    )
    .unwrap()
}

fn uyvy_receiver(ndi: &Ndi, sender: &Sender) -> Receiver {
    let source = common::loopback_source(ndi, sender).unwrap();
    Receiver::new(
        ndi,
        &ReceiverOptions::new(source).color_format(ReceiverColorFormat::UyvyBgra),
    )
    .unwrap()
}

/// The room the crate keeps after a frame: the format's, and two rows more
/// for an interlaced or single-field frame whose height is not a multiple of
/// four.
fn room(format: PixelFormat, width: u32, height: u32, scan: ScanType) -> usize {
    let room = format.send_tail_room();
    if room == 0 || scan == ScanType::Progressive || height.is_multiple_of(4) {
        room
    } else {
        room + 2 * (width as usize * 2)
    }
}

/// Frames whose buffers end at the last byte the process can read are sent
/// by every path that takes one, in every scan type, at sizes where the x86
/// SDK reads from 4 bytes to two rows past the frame: first with exactly the
/// room the crate keeps, in place, and then with none.
///
/// Where the SDK needs room, the first pins how much. Where it needs none,
/// both send in place and pin that it reads frames exactly.
#[test]
fn frames_that_end_where_memory_ends_are_sent() {
    let _workload = NATIVE_WORKLOAD
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(ndi) = common::ndi() else {
        return;
    };
    let mut sender = sender(&ndi, "frames at the end of memory");
    // The SDK compresses what it is sent whether or not anything receives
    // it; a receiver makes the test independent of that.
    let _receiver = uyvy_receiver(&ndi, &sender);
    let deadline = Instant::now() + Duration::from_secs(15);
    while sender.connection_count(Duration::from_millis(100)).unwrap() == 0 {
        assert!(Instant::now() < deadline, "no receiver connected");
    }
    let sizes = [
        (18, 16),
        (642, 32),
        (720, 480),
        (720, 486),
        (854, 480),
        (1366, 768),
        (1920, 1080),
        (1920, 1082),
        (2560, 1083),
    ];
    for format in [PixelFormat::Uyvy, PixelFormat::Uyva] {
        for (width, height) in sizes {
            for scan in [
                ScanType::Progressive,
                ScanType::Interlaced,
                ScanType::Field0,
                ScanType::Field1,
            ] {
                let case = format!("{format:?} {width}x{height} {scan:?}");
                let len = format.buffer_len(width, height).unwrap();
                let room = room(format, width, height, scan);

                // The frame and exactly its room, then nothing.
                let mut roomy = Guarded::new(len + room);
                smooth(&mut roomy.bytes()[..len]);
                let frame = BorrowedVideoFrame::builder(roomy.bytes(), width, height, format)
                    .allow_send_from_padding_and_tail(true)
                    .scan_type(scan)
                    .build()
                    .unwrap();
                assert!(frame.sends_in_place(), "{case}");
                for _ in 0..2 {
                    sender.send_video(&frame).unwrap();
                }

                // The frame, then nothing.
                let mut exact = Guarded::new(len);
                smooth(exact.bytes());
                let frame = BorrowedVideoFrame::builder(exact.bytes(), width, height, format)
                    .scan_type(scan)
                    .build()
                    .unwrap();
                assert_eq!(frame.sends_in_place(), room == 0, "{case}");
                for _ in 0..2 {
                    sender.send_video(&frame).unwrap();
                }
                // The copies these make are owned frames, which hold the
                // room or are given it when lent.
                let owned = frame.to_owned();
                sender.send_video(&owned).unwrap();
                sender.send_video_async_copy(&frame).unwrap();
                let returned = sender.send_video_async(owned).unwrap();
                assert!(returned.is_some(), "{case}");
                assert!(sender.finish_video_async().is_some(), "{case}");
            }
        }
    }
}

/// Stops a sending loop when the scope that waits for it ends, by return or
/// by panic: a scope joins its threads either way, so a loop that only a
/// later statement stops would hang a failed test instead of reporting it.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// The active pixels of the first 642x32 frame a new receiver gets while
/// `frame` is sent.
fn received(
    ndi: &Ndi,
    label: &str,
    frame: &(impl grafton_ndi::VideoSource + Sync),
    format: PixelFormat,
) -> Vec<u8> {
    let expected_format = if format == PixelFormat::Uyva {
        PixelFormat::Bgra
    } else {
        PixelFormat::Uyvy
    };
    let sender = sender(ndi, label);
    let receiver = uyvy_receiver(ndi, &sender);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                sender.send_video(frame).unwrap();
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let _stop = StopOnDrop(&stop);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "{label}: no frame");
            let captured = receiver
                .video()
                .capture_ref(Duration::from_millis(100))
                .unwrap();
            if let Some(captured) = captured {
                if captured.width() == 642 {
                    assert_eq!(captured.pixel_format(), expected_format);
                    break captured.to_owned().data().to_vec();
                }
            }
        }
    })
}

/// Bytes after a frame reach the picture only where the SDK reads past
/// frames, and then only when the caller offers them as room: after a buffer
/// that ends with its frame, the sender's copy puts zeros there.
///
/// The first assertion pins the SDK's behavior on this architecture. When it
/// fails, the SDK reads a 642-pixel UYVY row differently than the crate
/// assumes, and `PixelFormat::send_tail_room` must be measured again.
///
/// Which builds read past is not the same question as which get room: the
/// x86-64 build reads past this frame; the 64-bit ARM build reads it exactly
/// and gets no room; the 32-bit x86 build reads it exactly too, but keeps the
/// x86 room, which the crate gives every build but 64-bit ARM Linux.
#[test]
fn bytes_after_a_buffer_reach_the_picture_only_as_room() {
    let _workload = NATIVE_WORKLOAD
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(ndi) = common::ndi() else {
        return;
    };
    let (width, height, format) = (642, 32, PixelFormat::Uyvy);
    let len = width as usize * 2 * height as usize;
    let room = format.send_tail_room();
    // Enough bytes after the frame for any room, and for a difference in
    // them to show where the SDK needs none.
    let after = 64;
    assert!(room <= after);
    let buffer = |value: u8| {
        let mut bytes = vec![value; len + after];
        smooth(&mut bytes[..len]);
        bytes
    };
    let (dark, bright, zero) = (buffer(16), buffer(235), buffer(0));

    // Offered as room, the bytes after the frame are sent in place.
    let in_place = |bytes: &[u8], label: &str| {
        let frame = BorrowedVideoFrame::builder(bytes, width, height, format)
            .allow_send_from_padding_and_tail(true)
            .build()
            .unwrap();
        assert!(frame.sends_in_place());
        received(&ndi, label, &frame, format)
    };
    let (dark_room, bright_room, zero_room) = (
        in_place(&dark, "dark room"),
        in_place(&bright, "bright room"),
        in_place(&zero, "zero room"),
    );
    let reads_past = cfg!(target_arch = "x86_64");
    if reads_past {
        assert_ne!(
            dark_room, bright_room,
            "the SDK did not read the bytes after a 642-pixel UYVY frame"
        );
    } else {
        assert_eq!(
            (&dark_room, &bright_room),
            (&zero_room, &zero_room),
            "the SDK read the bytes after a 642-pixel UYVY frame"
        );
    }

    // After a buffer that ends with the frame, they never reach the picture:
    // the sender copies the frame and the SDK reads the copy's zeroed room.
    let exact = |bytes: &[u8], label: &str| {
        let frame = BorrowedVideoFrame::builder(&bytes[..len], width, height, format)
            .build()
            .unwrap();
        assert_eq!(frame.sends_in_place(), room == 0);
        received(&ndi, label, &frame, format)
    };
    assert_eq!(exact(&dark, "dark after"), zero_room);
    assert_eq!(exact(&bright, "bright after"), zero_room);

    // Vouched for, they are the room again, read where they lie.
    let builder = BorrowedVideoFrame::builder(&bright[..len], width, height, format)
        .allow_send_from_padding_and_tail(true);
    // SAFETY: `after` more bytes of `bright` follow the slice, and nothing
    // writes them.
    let vouched = unsafe { builder.readable_after(after) }.build().unwrap();
    assert!(vouched.sends_in_place());
    assert_eq!(received(&ndi, "vouched", &vouched, format), bright_room);

    // Declaring initialized room alone must not bypass default staging.
    let builder = BorrowedVideoFrame::builder(&bright[..len], width, height, format);
    // SAFETY: the same initialized allocation extends `after` bytes past
    // the slice and remains stable throughout the send.
    let default = unsafe { builder.readable_after(after) }.build().unwrap();
    assert_eq!(
        received(&ndi, "default vouched", &default, format),
        zero_room
    );
    let default = BorrowedVideoFrame::builder(&bright, width, height, format)
        .build()
        .unwrap();
    assert_eq!(
        received(&ndi, "default trailing slice", &default, format),
        zero_room
    );

    // An owned frame's room is zero too.
    let owned = BorrowedVideoFrame::builder(&bright[..len], width, height, format)
        .build()
        .unwrap()
        .to_owned();
    assert_eq!(received(&ndi, "owned", &owned, format), zero_room);
}

/// Row padding and all forms of trailing room are excluded by default,
/// including the extra strides needed by interlaced and field frames.
#[test]
fn default_yuv_sends_depend_only_on_active_pixels() {
    let _workload = NATIVE_WORKLOAD
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(ndi) = common::ndi() else {
        return;
    };
    for format in [PixelFormat::Uyvy, PixelFormat::Uyva] {
        if format.send_tail_room() == 0 {
            continue;
        }
        for (label, padding, height, scan, extra, vouched, last_only) in [
            (
                "earlier rows",
                4,
                32,
                ScanType::Progressive,
                0,
                false,
                false,
            ),
            ("last row", 4, 32, ScanType::Progressive, 0, false, true),
            (
                "sufficient padding",
                128,
                32,
                ScanType::Progressive,
                0,
                false,
                false,
            ),
            (
                "trailing slice",
                0,
                32,
                ScanType::Progressive,
                64,
                false,
                false,
            ),
            (
                "asserted room",
                0,
                32,
                ScanType::Progressive,
                64,
                true,
                false,
            ),
            (
                "interlaced",
                4,
                34,
                ScanType::Interlaced,
                2700,
                false,
                false,
            ),
        ] {
            let width = 642;
            let stride = width as usize * 2 + padding;
            let len =
                stride * height as usize * if format == PixelFormat::Uyva { 3 } else { 2 } / 2;
            let make = |value| {
                let mut data = vec![0; len + extra];
                for y in 0..height as usize {
                    smooth(&mut data[y * stride..y * stride + width as usize * 2]);
                    if (y == height as usize - 1) == last_only {
                        data[y * stride + width as usize * 2..(y + 1) * stride].fill(value);
                    }
                }
                if format == PixelFormat::Uyva {
                    let base = stride * height as usize;
                    for y in 0..height as usize {
                        data[base + y * stride / 2..base + y * stride / 2 + width as usize]
                            .fill(255);
                        if (y == height as usize - 1) == last_only {
                            data[base + y * stride / 2 + width as usize
                                ..base + (y + 1) * stride / 2]
                                .fill(value);
                        }
                    }
                }
                data[len..].fill(value);
                data
            };
            let (dark, bright) = (make(16), make(235));
            let frame = |data: &[u8], name: &str| {
                let slice = if vouched { &data[..len] } else { data };
                let builder = BorrowedVideoFrame::builder(slice, width, height, format)
                    .line_stride_bytes(stride)
                    .scan_type(scan);
                // SAFETY: `extra` initialized bytes follow the slice within
                // the same allocation when vouched; all data stays stable.
                let frame = unsafe { builder.readable_after(if vouched { extra } else { 0 }) }
                    .build()
                    .unwrap();
                assert!(!frame.sends_in_place());
                received(&ndi, &format!("{format:?} {label} {name}"), &frame, format)
            };
            assert_eq!(
                frame(&dark, "dark"),
                frame(&bright, "bright"),
                "{format:?} {label}"
            );
        }
    }
}
