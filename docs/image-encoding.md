# Image encoding

With the `image-encoding` feature, every video frame type encodes itself as a
PNG, JPEG or lossless WebP image, or as a data URL of one: frames you capture
from a receiver or a FrameSync, owned frames, and frames you build to send.
Every uncompressed pixel format is supported. YUV is converted to RGB as the
NDI SDK converts it, with BT.709, limited range and repeated chroma, unless a
frame's metadata names another matrix. Frames tagged HDR are refused unless you
ask for their code values, and images can be reduced by 2, 4 or 8 on the way.

```toml
[dependencies]
grafton-ndi = { version = "3.0", features = ["image-encoding"] }
```

The feature adds the `png`, `jpeg-encoder`, `image-webp` and `base64` crates,
all pure Rust. The `ColorInfo` color description and every frame's
`color_info()` need no feature.

## Encoding a frame

One options value, `ImageOptions`, describes an encode, and three methods on
every frame type take it:

| Method | Returns |
| --- | --- |
| `encode_image(&options)` | The image, in a new `Vec<u8>` |
| `encode_image_to(&options, writer)` | `()`, having written the image into any `io::Write` as it encoded |
| `encode_image_data_url(&options)` | A `data:image/...;base64,` URL, allocated at its final length |
| `encode_png()`, `encode_jpeg(quality)`, `encode_data_url(format)` | The same, with `ImageOptions::new(format)` |

```rust
use grafton_ndi::{Downscale, ImageFormat, ImageOptions, PngCompression};

// A PNG, fast enough for a snapshot loop of camera video.
let options = ImageOptions::new(ImageFormat::Png).png_compression(PngCompression::Fast);
let png = frame.encode_image(&options)?;

// A quarter-size JPEG thumbnail as a data URL.
let thumbnail = ImageOptions::new(ImageFormat::Jpeg(85)).downscale(Downscale::Quarter);
let url = frame.encode_image_data_url(&thumbnail)?;
```

A loop that takes a snapshot on a timer can reuse one buffer:

```rust
let options = ImageOptions::new(ImageFormat::WebpLossless);
let mut image = Vec::new();
while let Some(frame) = receiver.video().capture_ref(timeout)? {
    image.clear();
    frame.encode_image_to(&options, &mut image)?;
    publish(&image);
}
```

`encode_image_to` writes as it encodes: wrap a file or socket in a
`BufWriter`, because the codecs write in small pieces. Every check of the
options and the frame happens before anything is written. After a later error,
which is a failing writer or codec, the writer may hold part of an image.

Encoding is CPU work, tens to hundreds of milliseconds for a 1080p frame.
Async code runs it on a blocking thread with an owned frame:

```rust
let options = ImageOptions::new(ImageFormat::Jpeg(85));
let jpeg = tokio::task::spawn_blocking(move || frame.encode_image(&options)).await??;
```

`ImageOptions` holds settings only, so one value serves any number of threads.

## Formats

### What a frame can be

| Format | Planes | Sampling | Bits | Alpha |
| --- | --- | --- | --- | --- |
| BGRA, RGBA | 1 packed | RGB | 8 | Kept |
| BGRX, RGBX | 1 packed | RGB | 8 | Written as 255 |
| UYVY | 1 packed: Cb Y0 Cr Y1 for each pixel pair | 4:2:2 | 8 | None |
| UYVA | UYVY, then an 8-bit alpha plane | 4:2:2 | 8 | 8-bit |
| P216 | 16-bit Y, then interleaved 16-bit CbCr, both full height | 4:2:2 | 16 | None |
| PA16 | P216's planes, then a 16-bit alpha plane | 4:2:2 | 16 | 16-bit |
| NV12 | Y, then interleaved CbCr at half height | 4:2:0 | 8 | None |
| I420, YV12 | Y, then Cb and Cr (YV12: Cr and Cb) at half width and height | 4:2:0 | 8 | None |
| NA48, NB48 | Y, Cb, Cr (and alpha) at full resolution | 4:4:4 | 8 | NB48: 8-bit |
| NA4G, NB4G | Y, Cb, Cr (and alpha) at full resolution | 4:4:4 | 16 | NB4G: 16-bit |

Rows are read in place, skipping any row padding, so a frame captured with
the SDK's padded stride encodes exactly as its packed copy does.

### What an image can be

| Output | Channels | Bits a channel | Settings |
| --- | --- | --- | --- |
| PNG, `AlphaMode::Keep` (default) | RGBA; a source without alpha gets alpha 255 | 8 or 16 | `PngCompression::Fast`, `Balanced` (default) or `High` |
| PNG, `AlphaMode::Discard` | RGB | 8 or 16 | As above |
| JPEG | YCbCr; alpha is dropped | 8 | Quality 1 to 100; chroma halved both ways below 90, whole from 90 |
| Lossless WebP | RGBA, or RGB with `AlphaMode::Discard` | 8 | None |

`SampleDepth::Sixteen` writes a 16-bit PNG: P216, PA16, NA4G and NB4G frames
keep their precision, and 8-bit sources scale exactly (times 257). JPEG and
WebP are 8-bit only.

An untagged frame, or one tagged BT.601 or BT.709 SDR, in one of the four RGB
formats encodes, with the default options, to the same bytes as in
grafton-ndi 2.0. Unit tests hold
SHA-256 hashes of 2.0's PNG and JPEG output to keep it so.

Lossy WebP is not offered. No pure-Rust encoder met the crate's constraints
when 3.0 was made: a permissive licence, no `unsafe`, and Rust 1.88. At JPEG
85's quality it measured 17 to 36% smaller than JPEG on camera content and
about 80% smaller on graphics, so it can be added in a 3.x release once one
exists; `ImageFormat` is `#[non_exhaustive]`.

## Color

Under the default `ColorPolicy::Sdk`, a YUV frame is converted with the
matrix its `ndi_color_info` metadata names, or BT.709 when it names none, and
limited range. `ColorPolicy::Override { matrix, range }` replaces both. RGB
frames are encoded as they are.

### What the SDK does

Measured on Linux x86_64 with the 6.4.0.2 Standard and Advanced runtimes,
sender and receivers in one process on loopback, where the transport carried
flat content exactly:

| Question | Finding |
| --- | --- |
| Matrix used to send RGB as YUV | BT.709, limited range, at 720×576, 1920×1080 and 3840×2160: black became Y 16, white Y 235 |
| Matrix used to deliver YUV as RGB | BT.709 at every size tried, from 720×576 to 3840×2160, across both of the manual's boundaries |
| Does `ndi_color_info` change either conversion? | No: frames tagged BT.601 or BT.2020, alone or inside `ndi_metadata_group`, converted alike |
| Does the tag reach receivers? | Yes, unchanged, on UYVY, P216 and RGBX receivers |
| Chroma upsampling to RGB | Repeated: chroma changes only between the two pixels a sample covers and the next |
| 4:2:0 to 4:2:2 | Repeated: each NV12 chroma row became two identical rows |
| Precision of the SDK's RGB | A fixed-point approximation: white comes out 253, saturated blues fall up to 10 codes short, and green alternates by one code between even and odd pixels |

The NDI manual's table, BT.601 for SD, BT.709 for HD and BT.2020 for UHD,
describes neither conversion the 6.4 SDK performs, so the encoder does not
follow it. `tests/image_color_sdk.rs` pins these findings against a real
runtime; it passed on the Standard 6.1.1, 6.3.2 and 6.4.0.2 runtimes and the
Advanced 6.4.0.2 runtime.

What follows:

- For an untagged frame, matrix and chroma follow the SDK, so a snapshot of a
  UYVY frame matches what an RGB receiver of the same source shows, within the
  SDK's approximation: at most 10 code values, 1.5 on average.
- The coefficients are exact, so white is 255.
- A tagged frame follows its tag, although the SDK's RGB conversion ignores
  it: a source tagged BT.2020 encodes correctly here, and with BT.709
  coefficients in an RGB receiver.
- An untagged BT.601 source, such as older SD hardware, needs
  `ColorPolicy::Override`.

### The conversion

Readers hand the color stage 16-bit codes (an 8-bit sample shifted left by 8,
which keeps limited-range codes exact), so one kernel serves every depth. With
n the source's bits a sample and s = 2^(16 − n):

- limited range: Y' = (Y − 4096) / 56064, P = (C − 32768) / 57344;
- full range: Y' = Y / ((2^n − 1)·s), P = (C − 2^15) / ((2^n − 1)·s);
- R = Y' + 2(1 − K<sub>R</sub>)·P<sub>R</sub>, B = Y' + 2(1 − K<sub>B</sub>)·P<sub>B</sub>,
  G = (Y' − K<sub>R</sub>·R − K<sub>B</sub>·B) / (1 − K<sub>R</sub> − K<sub>B</sub>).

| Matrix | K<sub>R</sub> | K<sub>B</sub> |
| --- | --- | --- |
| BT.601 | 0.299 | 0.114 |
| BT.709 | 0.2126 | 0.0722 |
| BT.2020 | 0.2627 | 0.0593 |

R, G and B are clamped to 0 to 1 and rounded to the image's depth. The kernel
works in fixed point, `i32` for 8-bit images and `i64` for 16-bit ones, and its
tests hold it within one code of an `f64` reference at both depths.

### `ndi_color_info`

`color_info()` reads the element on any frame, with no feature:

```rust
match frame.color_info()? {
    Some(color) if color.is_hdr() => println!("HDR: {:?}", color.transfer()),
    Some(color) => println!("SDR, matrix {:?}", color.matrix()),
    None => println!("BT.709 SDR"),
}
```

- The element may stand alone or inside `ndi_metadata_group`. Comments and
  CDATA sections are skipped.
- `primaries`, `transfer` and `matrix` may come in any order, quoted with `"`
  or `'`, and each may be missing: a missing `matrix` means BT.709, a missing
  `transfer` SDR, and missing `primaries` unknown.
- Values are the SDK's: `bt_601`, `bt_709`, `bt_2020` and `bt_2100` for
  primaries and matrix, and `bt_601`, `bt_709`, `bt_2020`, `bt_2100_hlg` and
  `bt_2100_pq` for transfer. `bt_2100` primaries and matrix read as BT.2020,
  which they equal.
- An element that is malformed, truncated, gives an attribute twice or a value
  outside the list, or differs from another one in the same metadata, is an
  `Error::InvalidFrame` naming the attribute and value. Under
  `ColorPolicy::Sdk` such a frame is refused, so the encoder never guesses at a
  frame that says something it cannot read; under `ColorPolicy::Override` it
  counts as SDR with unknown primaries.

## HDR

| The frame's `transfer` | `HdrPolicy::Refuse` (default) | `HdrPolicy::CodeValues` |
| --- | --- | --- |
| None, `bt_601`, `bt_709` or `bt_2020` | Encoded | Encoded |
| `bt_2100_pq` | `Error::InvalidFrame` naming PQ | A PNG of the PQ code values, tagged |
| `bt_2100_hlg` | `Error::InvalidFrame` naming HLG | A PNG of the HLG code values, tagged |

- The check applies to RGB frames as well: an RGBX receiver of a PQ-tagged
  P216 source was measured to get the PQ code values, put through the SDK's
  BT.709 approximation without tone mapping, with the tag still attached.
- JPEG and WebP refuse PQ and HLG frames under `HdrPolicy::CodeValues` with
  `Error::InvalidInput`: neither can say what its numbers mean.
- Nothing tone-maps. A program that wants an SDR rendering of HDR video makes
  it itself, from a 16-bit PNG or from the frame.
- The Standard 6.4 runtime passed PQ-tagged P216 frames, tag intact, from its
  sender to its receivers on loopback, although the manual says the Standard
  SDK cannot send HDR metadata.

### Tagging a PNG

A PNG is tagged with a `cICP` chunk when an untagged viewer would show its
colors wrongly: BT.2020 primaries, or a PQ or HLG transfer. BT.601 and BT.709
SDR stay untagged, since viewers assume sRGB for them, and a frame without
`ndi_color_info` is never tagged. The chunk follows the header and comes
before the image data, as PNG requires. Its code points are ITU-T H.273's:

| Field | Values |
| --- | --- |
| Primaries | BT.709 1, BT.601 6, BT.2020 9, and 2 (unspecified) for an HDR frame that does not give its primaries |
| Transfer | PQ 16, HLG 18, and 1 for every SDR transfer, which H.273 defines as one curve |
| Matrix | 0, the only value PNG allows for RGB |
| Full range | 1, because the RGB written is full range |

PNG's `mDCV` and `cLLI` chunks are not written: NDI metadata carries no
mastering display or content light level.

## Reducing an image

`Downscale::Half`, `Quarter` and `Eighth` average 2×2, 4×4 or 8×8 blocks of
converted pixels inside the row loop, so a thumbnail of a full-size frame
never holds a full-size image.

- The image is ceil(width / f) by ceil(height / f); a block at the right or
  bottom edge averages the pixels it has.
- Averages are taken on code values, as previews usually are, and round half
  up.
- With alpha kept, color is weighted by alpha, Σ(a·c) / Σa, so transparent
  pixels do not darken edges; a block transparent throughout takes the plain
  mean. Alpha itself is a plain mean.

It serves frames that arrive full size: from a FrameSync, from receivers with
`ReceiverOptions::high_quality_preset`, and frames being sent. A
`snapshot_preset` receiver already gets the source's smaller preview stream.

## Cost and memory

Encoding cost is mostly compression. Measured on one thread for a 1920×1080
RGBA frame, a photograph and a graphics frame of flat boxes, a gradient and
text:

| 1920×1080 content | PNG `Balanced` | PNG `Fast` | PNG `High` | Lossless WebP | JPEG 85 |
| --- | --- | --- | --- | --- | --- |
| Photographic | 3.38 MB, 554 ms | 3.76 MB, 24 ms | 3.35 MB, 1,198 ms | 2.80 MB, 19 ms | 0.45 MB, 21 ms |
| Graphics | 150 KB, 40 ms | 492 KB, 8 ms | 135 KB, 104 ms | 329 KB, 8 ms | 97 KB, 16 ms |

- `PngCompression::Fast` is about 23 times faster than `Balanced` for 11%
  more bytes on the photograph, but 3.3 times the bytes on graphics.
  `Balanced` stays the default because it suits graphics and keeps 2.0's
  bytes.
- Lossless WebP is the lossless choice for camera video: 17% smaller than PNG
  `Balanced` and about 29 times faster. On graphics it is 2.2 times PNG's
  size.
- Converting YUV adds about 6 ms at 1080p, 24 ms at 4K and 95 ms at 8K. The
  [performance notes](image-encoding-performance.md) give every output by
  source format and size.

PNG and JPEG convert one row at a time, so their conversion scratch never
grows with the frame's height:

| Configuration | PNG scratch | JPEG heap scratch | WebP buffer |
| --- | --- | --- | --- |
| RGBA, contiguous rows | None: rows are read in place | None | None: the frame is read in place |
| RGBA with padded rows | None | None | width × height × 4 bytes |
| Otherwise, 8 bits | One row: width × 4 bytes, × 3 with `AlphaMode::Discard` | None: pixels convert into the codec's rows | width × height × 4 bytes, × 3 with `Discard` |
| 16 bits, any format | One row: width × 8 bytes, × 6 with `Discard` | — | — |
| With `Downscale` by f | ceil(width / f) × 16 bytes of sums (× 32 at 16 bits) and one reduced row | The sums | The reduced image, and the sums |

Codec state, the encoded image and data-URL text are extra.

## Errors

| Problem | Error |
| --- | --- |
| A JPEG quality outside 1 to 100, from `encode_jpeg` or `ImageOptions` | `InvalidInput` |
| JPEG or WebP with `SampleDepth::Sixteen` | `InvalidInput` |
| JPEG or WebP with `HdrPolicy::CodeValues` for a PQ or HLG frame | `InvalidInput` |
| A PQ or HLG frame under `HdrPolicy::Refuse`, in any pixel format | `InvalidFrame`, naming the transfer and `HdrPolicy::CodeValues` |
| Unreadable `ndi_color_info` under `ColorPolicy::Sdk` | `InvalidFrame`, naming the attribute and value |
| An image wider or taller than JPEG's 65,535 or WebP's 16,384 pixels | `InvalidFrame`, naming the dimension; `Downscale` can bring it within |
| The writer given to `encode_image_to` fails | `Io`, the writer's own error |
| The codec fails for another reason | `InvalidFrame`: "PNG encoding failed", "JPEG encoding failed" or "WebP encoding failed" |

`PngCompression` has no effect on JPEG or WebP and is no error with them.

## Thumbnails from a recording

[`examples/file_thumbnails.rs`](../examples/file_thumbnails.rs), which needs
`file-io` and `image-encoding`, is where the two features meet: it plays a
recording into a private NDI group, seeks to evenly spaced positions and
writes a quarter-size JPEG of the frame at each.

```bash
cargo run --example file_thumbnails --features file-io,image-encoding -- take_001.mkv 8
```

It is an example rather than an API, because it advertises an NDI source and
needs the Advanced runtime on Linux, macOS and Android. Its choices rest on
measurements with the 6.4.0.2 Advanced runtime and File library and a SpeedHQ
recording of a 1280×720 source:

| Question | Finding |
| --- | --- |
| Does a played frame keep its sender's timecode? | Yes: the `FirstFrame` event's source timecode equalled the next frame's, at the start and after each of 15 seeks |
| Which frame arrives first after a seek? | Whatever the receiver had queued; the target frame followed within milliseconds |
| Can a receiver use `player.advertised_source()`? | No: it has no address. A `Finder` in the player's groups lists the source with one |
| Does a `Lowest`-bandwidth receiver get video? | Only from a recording with a proxy track |

So it finds the player with a finder in its group, receives at full bandwidth
with `Downscale::Quarter` rather than through `snapshot_preset`, and after each
seek discards frames until one carries the `FirstFrame` event's timecode.
H.264 and HEVC recordings, where a seek lands on the keyframe at or before the
time asked for, were not measured.

## Testing

- Unit tests, with no runtime: `cargo test --lib --features image-encoding`.
  They check every reader, matrix, range and depth against an `f64` reference
  of the formulas, flat colors through a round trip of every layout, chroma
  siting, padded and odd-sized layouts, the 2.0 byte-identity hashes,
  `ndi_color_info` parsing (with a seeded mutation test), the `cICP` chunk, the
  HDR and option checks, every PNG level and lossless WebP decoding back
  exactly, and the allocation bounds above.
- `tests/image_color_sdk.rs` pins the SDK's color behavior on a real runtime,
  on loopback with `--config ci/offline.toml`.
- `tests/video_padding_sdk.rs` encodes UYVY and P216 frames read in place from
  the SDK's padded rows; run it under Valgrind Memcheck by hand to check that
  no padding is read.
- `cargo test --release --lib --features image-encoding image_encoding_resources -- --ignored --nocapture`
  measures time and peak memory; see the
  [performance notes](image-encoding-performance.md).
