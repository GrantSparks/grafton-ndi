# Image encoding

With the `image-encoding` feature, every video frame type encodes itself as a
PNG, JPEG or WebP image, or as a data URL of one: frames you capture
from a receiver or a FrameSync, owned frames, and frames you build to send.
Every uncompressed pixel format is supported. YUV is converted to RGB as the
NDI SDK converts it, with BT.709, limited range and repeated chroma, unless a
frame's metadata names another matrix. Frames tagged HDR are refused unless you
ask for their code values, and images can be reduced by 2, 4 or 8 on the way.

```toml
[dependencies]
grafton-ndi = { version = "3.0", features = ["image-encoding"] }
```

The feature adds the `png`, `jpeg-encoder`, `image-webp`, `libwebpop` and `base64` crates,
all pure Rust. The `ColorInfo` color description and every frame's
`color_info()` need no feature.

## Encoding a frame

`ImageOptions` configures encoding:

| Method | Returns |
| --- | --- |
| `encode_image(&options)` | The image, in a new `Vec<u8>` |
| `encode_image_to(&options, writer)` | `()`, having written the image into any `io::Write` |
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

// Lossy WebP for a bandwidth-sensitive preview, keeping transparency exact.
let preview = ImageOptions::new(ImageFormat::WebpLossy(75)).downscale(Downscale::Half);
let webp = frame.encode_image(&preview)?;
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

PNG, JPEG and lossless WebP write as they encode: wrap a file or socket in a
`BufWriter`, because those codecs write in small pieces. Lossy WebP buffers
the complete encoded image before writing it. Every check of the
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
| Lossy WebP | 4:2:0 color; exact alpha unless discarded | 8 | `ImageFormat::WebpLossy(quality)`, quality 0 to 100 |

Lossy WebP trades color fidelity for fewer bytes. Lower quality values generally
produce smaller images; 100 still changes color, so choose `WebpLossless` when
pixels must be exact. The encoder uses `libwebpop`, a scalar Rust port of
libwebp, at method 4 with SharpYUV conversion and alpha quality 100. SharpYUV
falls back to ordinary conversion for images narrower or shorter than four
pixels. `AlphaMode::Discard` makes the image opaque before any reduction.

`SampleDepth::Sixteen` writes a 16-bit PNG: P216, PA16, NA4G and NB4G frames
keep their precision, and 8-bit sources scale exactly (times 257). JPEG and
WebP are 8-bit only.

## Color

Under the default `ColorPolicy::Sdk`, a YUV frame is converted with the
matrix its `ndi_color_info` metadata names, or BT.709 when it names none, and
limited range. `ColorPolicy::Override { matrix, range }` replaces both. RGB
frames are encoded as they are.

Use `ColorPolicy::Override` for an untagged source whose matrix or range differs
from the default. Tagged frames follow their matrix, even though the SDK's own
RGB conversion may ignore it. See [color validation](sdk-validation.md#image-color-conversion)
for measured behavior and conversion equations.

### `ndi_color_info`

`color_info()` reads the element on any frame, with no feature:

```rust
match frame.color_info()? {
    Some(color) if color.is_hdr() => println!("HDR: {:?}", color.transfer()),
    Some(color) => println!("SDR, matrix {:?}", color.matrix()),
    None => println!("BT.709 SDR"),
}
```

- The element may stand alone or inside `ndi_metadata_group`. Comments,
  CDATA sections and processing instructions are skipped.
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
- On Linux the Standard 6.4 runtime passes PQ-tagged P216 frames, tag intact,
  from its sender to its receivers on loopback, although the manual says the
  Standard SDK cannot send HDR metadata.

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

Encoding can take tens to hundreds of milliseconds per 1080p frame. Keep it off
capture and executor threads. Choose `PngCompression::Fast` or lossless WebP for
camera snapshots; compare output sizes on your content.

PNG and JPEG convert rows incrementally. Both WebP codecs may hold a whole
converted image. Lossless WebP uses RGB with alpha discarded, while lossy WebP's
backend always takes RGBA, with alpha set to 255 when discarded. Lossy WebP
also holds a complete encoded image before writing, even with `encode_image_to`.
Codec state and encoded output use additional memory. See the
[performance measurements](image-encoding-performance.md) for timings, memory bounds
and the hardware and inputs used.

## Errors

| Problem | Error |
| --- | --- |
| A JPEG quality outside 1 to 100, from `encode_jpeg` or `ImageOptions` | `InvalidInput` |
| A lossy WebP quality outside 0 to 100 | `InvalidInput` |
| JPEG or WebP with `SampleDepth::Sixteen` | `InvalidInput` |
| JPEG or WebP with `HdrPolicy::CodeValues` for a PQ or HLG frame | `InvalidInput` |
| A PQ or HLG frame under `HdrPolicy::Refuse`, in any pixel format | `InvalidFrame`, naming the transfer and `HdrPolicy::CodeValues` |
| Unreadable `ndi_color_info` under `ColorPolicy::Sdk` | `InvalidFrame`, naming the attribute and value |
| An image wider or taller than JPEG's 65,535, lossless WebP's 16,384 or lossy WebP's 16,383 pixels | `InvalidFrame`, naming the dimension; `Downscale` can bring it within |
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

The example discovers the player's source in its private group, receives at full
bandwidth and reduces each image with `Downscale::Quarter`. After seeking, it
waits for a frame matching the `FirstFrame` event's timecode to avoid queued frames
from before the seek. This workflow was measured with a SpeedHQ recording;
H.264 and HEVC seek behavior was not measured.

## Validation

See [SDK validation](sdk-validation.md#image-encoding) and the
[performance notes](image-encoding-performance.md) for tests and benchmark commands.
