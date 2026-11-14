# Image encoding

Enable `image-encoding` to encode captured, owned or borrowed uncompressed video
frames as PNG, JPEG, lossless WebP, lossy WebP or data URLs:

```toml
[dependencies]
grafton-ndi = { version = "3.0", features = ["image-encoding"] }
```

## Encoding a frame

Run the [`NDIlib_Recv_PNG` example](../examples/NDIlib_Recv_PNG.rs) with a source
visible on your network:

```sh
cargo run --example NDIlib_Recv_PNG --features image-encoding -- --output snapshot.png
```

It selects the first discovered source, captures one frame and writes
`snapshot.png`. Use `ImageOptions::new(format)` with a frame's `encode_image`
to allocate a `Vec<u8>`, `encode_image_to` to write to an `io::Write`, or
`encode_image_data_url` for a base64 URL. `encode_png` and `encode_jpeg` are
conveniences. See [`ImageOptions`](https://docs.rs/grafton-ndi/3.0.0/grafton_ndi/struct.ImageOptions.html)
for compile-checked examples and exact option/error contracts.

All frame and option validation occurs before writing. A later codec or writer
failure can leave partial output, so publish files only after a successful
encode. Reuse a cleared `Vec<u8>` with `encode_image_to` when retaining output
capacity is useful. For files or sockets, consider `BufWriter` for the codecs
that write incrementally.

For repeated snapshots, `ImageWorkspace::new()` provides `encode(&frame,
&options)` and `encode_to(&frame, &options, &mut output)`. It retains conversion
scratch across calls, including WebP's whole converted image, PNG rows and
downscale sums. `retained_bytes()` reports wrapper capacity; `release()` frees
it. Use a separate workspace for each concurrent worker. Codec allocations
and the encoded result are separate; lossy WebP still buffers its output.

Encoding is synchronous CPU work. Keep it on a blocking worker in async
applications, with an owned frame if the worker outlives the capture borrow.
Bound active workers and pending snapshots. [Performance guidance](image-encoding-performance.md)
explains the buffering choices and measurement boundaries.

## Formats

All `PixelFormat` uncompressed variants are accepted; row padding is skipped.
See [pixel layouts](https://docs.rs/grafton-ndi/3.0.0/grafton_ndi/frames/enum.PixelFormat.html)
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

`SampleDepth::Sixteen` is PNG-only: high-bit-depth sources retain precision,
while 8-bit inputs scale by 257. JPEG and WebP reject that setting. Codec
dimension limits apply after downscaling; invalid options return `InvalidInput`
and unsupported output dimensions return `InvalidFrame`.

## Color

`ColorPolicy::Sdk`, the default, converts YUV using the matrix in readable
`ndi_color_info`, or BT.709 when absent, with limited range. RGB pixels are
encoded directly. For an untagged source with another matrix or range, specify
`ColorPolicy::Override { matrix, range }`.

### `ndi_color_info`

Every frame's `color_info()` exposes this metadata without `image-encoding`.
Malformed or conflicting descriptions produce `InvalidFrame`. The default
encoding policy refuses them; an explicit override ignores unreadable color
metadata and treats it as SDR with unknown primaries. Readable metadata still
controls the HDR check and PNG tagging. See
[`ColorInfo`](https://docs.rs/grafton-ndi/3.0.0/grafton_ndi/struct.ColorInfo.html)
for accepted metadata values.

## HDR

`HdrPolicy::Refuse` rejects PQ- and HLG-tagged frames, including RGB frames.
Use `HdrPolicy::CodeValues` to preserve their nonlinear values in a tagged PNG;
combine it with `SampleDepth::Sixteen` for high-bit-depth sources. JPEG and WebP
reject that HDR mode. The encoder does not tone-map; create an SDR rendering
explicitly if that is what the consumer needs.

### Tagging a PNG

The encoder writes a `cICP` color tag for BT.2020 primaries or PQ/HLG transfer
metadata. It leaves BT.601/BT.709 SDR and untagged inputs without that chunk.
See [`HdrPolicy`](https://docs.rs/grafton-ndi/3.0.0/grafton_ndi/enum.HdrPolicy.html)
for the code-value contract.

## Reducing an image

`Downscale::Half`, `Quarter` and `Eighth` average blocks during conversion.
Output dimensions round upward; partial edge blocks use their available pixels.
With alpha retained, colors are weighted by alpha to avoid darkening edges.
The reduction operates on code values and does not allocate a full-size RGB
image. It reduces full-size frames independently of the receiver's bandwidth
selection; a `snapshot_preset` receiver already requests the preview stream.

## Cost and memory

PNG and JPEG convert rows incrementally. WebP may need a complete converted
image; lossy WebP also buffers its complete encoded output, even when using
`encode_image_to`. Codec state and output add memory beyond conversion storage.
See [buffering and concurrency](image-encoding-performance.md#whole-image-buffering-and-concurrency)
for application planning.

## Errors

Inspect the method's `Result`: invalid quality, effort or depth settings return
`InvalidInput`; malformed color descriptions, HDR refusal, excessive dimensions
or codec failures return `InvalidFrame`; writer failures retain their `Io`
error. The [API reference](https://docs.rs/grafton-ndi/3.0.0/grafton_ndi/struct.ImageOptions.html#errors)
lists the complete option rules.

## Thumbnails from a recording

The [`file_thumbnails` example](../examples/file_thumbnails.rs) plays a recording
into a private NDI group, seeks to spaced positions and writes quarter-size
JPEGs. After each seek it waits for a frame matching the player's `FirstFrame`
timecode when the SDK provides one; otherwise it takes the next frame:

```sh
cargo run --example file_thumbnails --features file-io,image-encoding -- take_001.mkv 8
```

Its accuracy depends on the recording and native seek behavior; validate the
workflow with your media. See [File API requirements](file-io.md#requirements).

## Validation

See [SDK validation](sdk-validation.md) for configured coverage and
[performance guidance](image-encoding-performance.md) for the resource harness.
