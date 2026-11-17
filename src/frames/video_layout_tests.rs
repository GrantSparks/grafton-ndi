use super::*;

/// Independent SDK geometry expectations, recorded against the old cached
/// layout fields before making the plane table the only stored geometry.
#[test]
fn video_layout_quantities_match_sdk_geometry_for_every_format() {
    for format in PixelFormat::ALL {
        let bytes = match format {
            PixelFormat::Bgra | PixelFormat::Bgrx | PixelFormat::Rgba | PixelFormat::Rgbx => 4,
            PixelFormat::Uyvy
            | PixelFormat::Uyva
            | PixelFormat::P216
            | PixelFormat::Pa16
            | PixelFormat::Na4g
            | PixelFormat::Nb4g => 2,
            _ => 1,
        };
        for width in [2u32, 642] {
            for height in [1u32, 2, 4] {
                for padding in [0usize, 1, 2, 64] {
                    let active = width as usize * bytes;
                    let stride = active + padding;
                    let rows = height as usize;
                    let geometries = match format {
                        PixelFormat::Yv12 | PixelFormat::I420 => {
                            if !height.is_multiple_of(2) || !stride.is_multiple_of(2) {
                                assert!(ValidatedVideoLayout::new_uncompressed(
                                    format,
                                    width,
                                    height,
                                    Some(stride)
                                )
                                .is_err());
                                continue;
                            }
                            vec![
                                (stride, active, rows),
                                (stride / 2, active / 2, rows / 2),
                                (stride / 2, active / 2, rows / 2),
                            ]
                        }
                        PixelFormat::Nv12 => {
                            if !height.is_multiple_of(2) {
                                assert!(ValidatedVideoLayout::new_uncompressed(
                                    format,
                                    width,
                                    height,
                                    Some(stride)
                                )
                                .is_err());
                                continue;
                            }
                            vec![(stride, active, rows), (stride, active, rows / 2)]
                        }
                        PixelFormat::Uyva => {
                            if !stride.is_multiple_of(2) {
                                assert!(ValidatedVideoLayout::new_uncompressed(
                                    format,
                                    width,
                                    height,
                                    Some(stride)
                                )
                                .is_err());
                                continue;
                            }
                            vec![(stride, active, rows), (stride / 2, active / 2, rows)]
                        }
                        PixelFormat::P216 => vec![(stride, active, rows); 2],
                        PixelFormat::Pa16 | PixelFormat::Na48 | PixelFormat::Na4g => {
                            vec![(stride, active, rows); 3]
                        }
                        PixelFormat::Nb48 | PixelFormat::Nb4g => vec![(stride, active, rows); 4],
                        _ => vec![(stride, active, rows)],
                    };
                    let storage = geometries
                        .iter()
                        .map(|(stride, _, rows)| stride * rows)
                        .sum();
                    let payload = geometries
                        .iter()
                        .map(|(_, active, rows)| active * rows)
                        .sum();
                    let &(last_stride, last_active, _) = geometries.last().unwrap();
                    let layout =
                        ValidatedVideoLayout::new_uncompressed(format, width, height, Some(stride))
                            .unwrap();
                    let quantities = |layout: ValidatedVideoLayout| {
                        (
                            layout.sdk_line_stride(),
                            layout.data_len_bytes(),
                            layout.payload_len_bytes(),
                            layout.planes().len(),
                            layout.trailing_padding(),
                        )
                    };
                    assert_eq!(
                        quantities(layout),
                        (
                            stride as i32,
                            storage,
                            payload,
                            geometries.len(),
                            last_stride - last_active
                        ),
                        "{format:?}, {width}x{height}, padding={padding}"
                    );
                    assert_eq!(
                        quantities(layout.packed()),
                        (active as i32, payload, payload, geometries.len(), 0),
                        "packed {format:?}, {width}x{height}, padding={padding}"
                    );
                }
            }
        }
    }
}

#[test]
fn video_layout_stores_no_derived_geometry() {
    // The old layout occupied 176/92 bytes: removing its four usize caches
    // leaves only the three source values and the four-plane table.
    let expected = if size_of::<usize>() == 8 { 144 } else { 76 };
    assert_eq!(size_of::<ValidatedVideoLayout>(), expected);
}
