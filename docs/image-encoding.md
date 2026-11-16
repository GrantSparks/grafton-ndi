# Image encoding

Enable `image-encoding` to encode captured, owned or borrowed uncompressed video
frames as PNG, JPEG, lossless WebP, lossy WebP or data URLs:

```toml
[dependencies]
grafton-ndi = { version = "3.0", features = ["image-encoding"] }
```

## Encoding a frame

Run the [`receive_png` example](../examples/receive_png.rs) with a source
visible on your network:

```sh
cargo run --example receive_png --features image-encoding -- --output snapshot.png
```

It selects the first discovered source, captures one frame and writes
`snapshot.png`. Use `ImageOptions::new(format)` with a frame's `encode_image`
to allocate a `Vec<u8>`, `encode_image_to` to write to an `io::Write`, or
`encode_image_data_url` for a base64 URL. Import options and formats through
`grafton_ndi::image_encoding`. The same encoding surface is available to generic
`VideoSource` code. See [`ImageOptions`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/image_encoding/struct.ImageOptions.html)
for compile-checked examples and exact option/error contracts.

All frame and option validation occurs before writing. A later codec or writer
failure can leave partial output, so publish files only after a successful
encode. Reuse a cleared `Vec<u8>` with `encode_image_to` when retaining output
capacity is useful. For files or sockets, consider `BufWriter` for the codecs
that write incrementally.

For repeated snapshots, `ImageWorkspace::new()` provides `encode(&frame,
&options)`, `encode_to(&frame, &options, &mut output)` and
`encode_data_url(&frame, &options)`. It retains conversion
scratch across calls, including WebP's whole converted image, PNG rows and
downscale sums. `retained_bytes()` reports wrapper capacity; `release()` frees
it. Use a separate workspace for each concurrent worker. Codec allocations
and the encoded result are separate; both WebP formats buffer their output.
`encode` remembers the last successful encoded length for matching format settings
and uses a hint bounded by the next image's size. It retains no output buffer;
`release()` also resets the hint. `encode_to` leaves output allocation to its writer.

Encoding is synchronous CPU work. Keep it on a blocking worker in async
applications, with an owned frame if the worker outlives the capture borrow.
Bound active workers and pending snapshots. [Performance guidance](image-encoding-performance.md)
explains the buffering choices and measurement boundaries.

## Picture aspect

PNG and JPEG carry a frame's non-square picture aspect in PNG pHYs and JPEG
JFIF relative pixel-density tags. The tags preserve the displayed width/height
ratio, including after downscaling with a partial final block. Encoders keep the
stored pixel dimensions and samples; viewers that ignore the tags still display
those dimensions. A zero aspect or an explicit square-pixel aspect stays
untagged, with byte-identical output. WebP carries no pixel-shape tag; callers
needing its display aspect retain `picture_aspect_ratio()` separately.

Both density components are positive integers no larger than 65,535. Ordinary
ratios are approximated to that precision; extreme ratios clamp to the nearest
representable endpoint. No resampling to square pixels is performed.

## Formats

All `PixelFormat` uncompressed variants are accepted; row padding is skipped.
See [pixel layouts](https://grantsparks.github.io/grafton-ndi/grafton_ndi/frames/enum.PixelFormat.html)
instead of repacking a captured frame solely for encoding.

| Output | Choose it for | Relevant settings |
| --- | --- | --- |
| PNG | Lossless RGB/RGBA at 8 or 16 bits per channel | `png_compression`, `depth`, `alpha` |
| JPEG | Lossy 8-bit output without alpha | Quality 1–100 |
| `WebpLossless` | Lossless 8-bit RGB/RGBA | `alpha` |
| `WebpLossy(quality)` | Lossy 8-bit color with exact alpha unless discarded | Quality 0–100, `webp_effort` 0–6, `webp_sharp_yuv` |

JPEG drops alpha. PNG and WebP keep it by default; `AlphaMode::Discard`
removes it. Lossy WebP quality 100 is still lossy for color. Its default effort
is 4 and SharpYUV is enabled. Effort and conversion settings affect only lossy
WebP; they impose no time or memory limit. Measure their effect on your content.

JPEG uses runtime AVX2 acceleration where supported, with a scalar fallback.
Full-size contiguous RGBA/RGBX/BGRA/BGRX input goes directly to the backend;
padded rows, YUV and downscaling retain incremental conversion. Encoded JPEG
bytes and reconstructed pixels can differ between accelerated and scalar
execution. Compare decoded quality rather than relying on byte identity.

`SampleDepth::Sixteen` is PNG-only: P216, PA16, NA4G and NB4G sources retain
precision. Eight-bit RGB and alpha samples scale by 257; YUV sources are
converted at 16-bit precision. JPEG and WebP reject that setting. Codec
dimension limits apply after downscaling; invalid options return `InvalidInput`
and unsupported output dimensions return `InvalidFrame`.

## Color

`ColorPolicy::Sdk`, the default, converts YUV using the matrix in readable
`ndi_color_info`, or BT.709 when absent, with limited range. For an untagged
source with another matrix or range, specify
`ColorPolicy::Override { matrix, range }`.

The SDK's own YUV-to-RGB conversion always uses BT.709 limited range, whatever
`ndi_color_info` says. RGB-received frames, including those from
`ReceiverOptions::snapshot_preset` and `ReceiverOptions::high_quality_preset`,
already carry that conversion. Encoding keeps those RGB pixels; it cannot
apply the tag's matrix afterward. The tag still controls HDR refusal and a
PNG's `cICP` chunk.

`ReceiverColorFormat::UyvyBgra` and `ReceiverColorFormat::UyvyRgba` keep
opaque sources in YUV, but deliver BGRA or RGBA for sources with alpha.
When a tagged source may carry alpha, use `ReceiverColorFormat::Fastest`
for UYVA or `ReceiverColorFormat::Best` for UYVA or PA16. `Best` also retains
high bit depth and HDR code values. Both deliver individual fields even with
`allow_video_fields(false)`, so interlaced snapshots may need field assembly.

### `ndi_color_info`

Every frame's `color_info()` exposes this metadata without `image-encoding`.
Malformed or conflicting descriptions produce `InvalidFrame`, as does received
metadata withheld with `FrameMetadataStatus::NotUtf8` or `Unterminated`. Owned
copies and clones preserve that status without retaining the unreadable text,
so encoding on a blocking thread has the same behavior as encoding a capture.
The default encoding policy refuses these frames before writing output; an
explicit `ColorPolicy::Override` ignores unreadable color metadata and treats
it as SDR with unknown primaries. Readable metadata still
controls the HDR check and PNG tagging. See
[`ColorInfo`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/struct.ColorInfo.html)
for accepted metadata values.

## HDR

`HdrPolicy::Refuse` rejects PQ- and HLG-tagged frames, including RGB frames.
Use `HdrPolicy::CodeValues` to preserve their nonlinear values in a tagged PNG;
combine it with `SampleDepth::Sixteen` for high-bit-depth sources. JPEG and WebP
reject that HDR mode. The encoder does not tone-map; create an SDR rendering
explicitly if that is what the consumer needs.

An RGB-received HDR frame already holds the SDK's BT.709 limited-range
conversion, whatever its passed-through `ndi_color_info` says. Increasing the
output to 16 bits cannot recover the original code values. Receive YUV with
`ReceiverColorFormat::Best` to keep their high bit depth; the tag still decides
HDR refusal and the PNG `cICP` chunk for RGB as well as YUV.

### Tagging a PNG

The encoder writes a `cICP` color tag for BT.2020 primaries or PQ/HLG transfer
metadata. It leaves BT.601/BT.709 SDR and untagged inputs without that chunk.
See [`HdrPolicy`](https://grantsparks.github.io/grafton-ndi/grafton_ndi/image_encoding/enum.HdrPolicy.html)
for the code-value contract.

## Reducing an image

`Downscale::Half`, `Quarter` and `Eighth` average blocks during conversion.
Output dimensions round upward; partial edge blocks use their available pixels.
With alpha retained, colors are weighted by alpha to avoid darkening edges.
Color channels are treated as straight (not premultiplied by alpha), and PNG
and WebP output store straight alpha. The [NDI SDK frame-type reference](https://docs.ndi.video/all/developing-with-ndi/sdk/frame-types)
documents this convention for BGRA and RGBA. That reference does not specify it
for UYVA, PA16, NB48 or NB4G; the encoder assumes straight color for those formats
too.
The reduction operates on code values and does not allocate a full-size RGB
image. It reduces full-size frames independently of the receiver's bandwidth
selection; a `snapshot_preset` receiver already requests the preview stream.

## Cost and memory

PNG and JPEG convert rows incrementally. WebP may need a complete converted
image; both WebP formats also buffer their complete encoded output, even when using
`encode_image_to`. Codec state and output add memory beyond conversion storage.
See [buffering and concurrency](image-encoding-performance.md#whole-image-buffering-and-concurrency)
for application planning.

## Errors

Inspect the method's `Result`: invalid quality, effort or depth settings, and
`HdrPolicy::CodeValues` with JPEG or WebP for an HDR frame, return `InvalidInput`.
Malformed color descriptions, HDR refusal, excessive dimensions
or codec failures return `InvalidFrame`. Conversion scratch allocation failures
return `AllocationFailed`. The lossy WebP encoder's own allocation failures are
the exception: they remain `InvalidFrame(FrameDefect::EncodingFailed)` until its
supported error API can classify them. Write and flush failures retain their
original `Io` error. A successful writer encode has written every byte and
flushed the writer. The [API reference](https://grantsparks.github.io/grafton-ndi/grafton_ndi/image_encoding/struct.ImageOptions.html#errors)
lists the complete option rules.

## Thumbnails from a recording

The [`file_thumbnails` example](../examples/file_thumbnails.rs) plays a recording
into a private NDI group, seeks to spaced positions and writes quarter-size
JPEGs. It requires exactly one video track: with program and proxy tracks, the
6.4 File player's `FirstFrame` timecode can fail to match the received video.
The [`file_record` example](../examples/file_record.rs) uses
`FileRecorderOptions::proxy(false)` to produce suitable recordings.

The receiver connects before playback starts. Video and events are drained
together; playback pauses at `FirstFrame` while the receiver catches up and
encoding completes. The example matches the event's timecode when the SDK
provides one; otherwise it takes the next available frame. Conversion scratch
and encoded-output capacity are reused:

```sh
cargo run --example file_thumbnails --features file-io,image-encoding -- take_001.mkv 8
```

Its accuracy depends on the recording and native seek behavior; validate the
workflow with your media. See [File API requirements](file-io.md#requirements).

## Validation

See [testing from source](../ci/README.md) for runtime checks and
[performance guidance](image-encoding-performance.md) for the resource harness.
