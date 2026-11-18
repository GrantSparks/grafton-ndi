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

#[cfg(feature = "image-encoding")]
#[test]
fn plane_roles_name_the_documented_planes_for_every_format() {
    for format in PixelFormat::ALL {
        for padding in [0, 2, 64] {
            let stride = 4 * usize::from(format.info().bytes_per_pixel()) + padding;
            let layout =
                ValidatedVideoLayout::new_uncompressed(format, 4, 4, Some(stride)).unwrap();
            let data: Vec<_> = (0..layout.data_len_bytes()).map(|i| i as u8).collect();
            // SAFETY: the initialized buffer covers this layout for both borrows.
            let roles = unsafe { layout.plane_roles(data.as_ptr()) };
            let planes: Vec<_> = unsafe { layout.view_planes(data.as_ptr()) }.collect();
            let equal = |role: VideoPlane<'_>, index: usize| {
                let expected = planes[index];
                assert_eq!(
                    role.row_stride_bytes(),
                    expected.row_stride_bytes(),
                    "{format:?}"
                );
                assert_eq!(
                    role.active_row_bytes(),
                    expected.active_row_bytes(),
                    "{format:?}"
                );
                assert_eq!(role.row_count(), expected.row_count(), "{format:?}");
                for (actual, expected) in role.rows().zip(expected.rows()) {
                    assert_eq!(actual.as_ptr(), expected.as_ptr(), "{format:?}");
                    assert_eq!(actual, expected, "{format:?}");
                }
            };
            match roles {
                PlaneRoles::PackedRgb { pixels, order } => {
                    assert!(matches!(
                        (format, order),
                        (PixelFormat::Rgba | PixelFormat::Rgbx, RgbOrder::Rgb)
                            | (PixelFormat::Bgra | PixelFormat::Bgrx, RgbOrder::Bgr)
                    ));
                    equal(pixels, 0);
                }
                PlaneRoles::Packed422 { pixels } => {
                    assert_eq!(format, PixelFormat::Uyvy);
                    equal(pixels, 0);
                }
                PlaneRoles::PackedAlpha { pixels, alpha } => {
                    assert_eq!(format, PixelFormat::Uyva);
                    equal(pixels, 0);
                    equal(alpha, 1);
                }
                PlaneRoles::SemiPlanar420 { luma, chroma } => {
                    assert_eq!(format, PixelFormat::Nv12);
                    equal(luma, 0);
                    equal(chroma, 1);
                }
                PlaneRoles::Planar420 { luma, cb, cr } => {
                    assert!(matches!(format, PixelFormat::Yv12 | PixelFormat::I420));
                    equal(luma, 0);
                    if format == PixelFormat::Yv12 {
                        equal(cr, 1);
                        equal(cb, 2);
                    } else {
                        equal(cb, 1);
                        equal(cr, 2);
                    }
                }
                PlaneRoles::SemiPlanar422 {
                    luma,
                    chroma,
                    alpha,
                } => {
                    assert!(matches!(format, PixelFormat::P216 | PixelFormat::Pa16));
                    assert_eq!(alpha.is_some(), format == PixelFormat::Pa16);
                    equal(luma, 0);
                    equal(chroma, 1);
                    if let Some(alpha) = alpha {
                        equal(alpha, 2);
                    }
                }
                PlaneRoles::Planar444 {
                    luma,
                    cb,
                    cr,
                    alpha,
                } => {
                    assert!(matches!(
                        format,
                        PixelFormat::Na48
                            | PixelFormat::Nb48
                            | PixelFormat::Na4g
                            | PixelFormat::Nb4g
                    ));
                    assert_eq!(
                        alpha.is_some(),
                        matches!(format, PixelFormat::Nb48 | PixelFormat::Nb4g)
                    );
                    equal(luma, 0);
                    equal(cb, 1);
                    equal(cr, 2);
                    if let Some(alpha) = alpha {
                        equal(alpha, 3);
                    }
                }
            }
        }
    }
}
