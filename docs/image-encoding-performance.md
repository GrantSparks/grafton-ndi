# Image encoding performance

Choose image settings by measuring the application path: capture, conversion,
encoding, transfer and any consumer decode. Compare formats at acceptable visual
quality on representative content; equal JPEG and WebP quality numbers do not
establish equal quality. The [encoding guide](image-encoding.md) explains settings
and color/HDR handling.

## Whole-image buffering and concurrency

PNG and JPEG convert rows incrementally. WebP borrows eligible contiguous RGBA
input or constructs a complete converted image. Lossless WebP uses three bytes
per output pixel with alpha discarded, otherwise four; lossy WebP always takes
four, with opaque alpha when discarded. Lossy WebP also buffers the complete
encoded result before `encode_image_to` writes it. These are conversion/output
costs; codec state, source frames, reduction scratch and consumer buffers add to
memory use.

`ImageWorkspace` reuses wrapper conversion capacity across frames, with
`retained_bytes()` for accounting and `release()` to discard it. Warm reuse
removes scratch allocation churn; the full WebP image still occupies memory.
Each concurrent worker needs its own workspace. Codec/output allocations
remain separate. The [#140 report](benchmarks/issue-140/README.md) retains raw
public-API measurements and their limitations.

Bound both active workers and pending requests. Run synchronous encoding on
blocking workers when using an async executor. Downscaling can reduce the
converted image and output dimensions without allocating a full-size RGB
intermediate. Lossy WebP effort and SharpYUV settings can change encoded size
and reconstructed pixels; they do not impose resource limits.

## Measure an application

Record input hashes, source pixel formats, strides, dimensions, color metadata,
output settings, dependency versions, commit, toolchain, CPU target and hardware.
Define whether the measurement includes capture waits, conversion, base64,
consumer decoding, thread startup and publication. Measure a realistic queue and
concurrency level, warmup and multiple rounds. Report wall time, output sizes,
visual/reconstruction quality and memory separately. Heap allocations, process
RSS and total system memory answer different questions.

The repository's ignored resource test measures a single encode's elapsed time,
output bytes and peak live heap allocations on its thread, excluding the
preexisting source frame. It does not measure network or consumer costs:

```sh
GRAFTON_NDI_SIZES=640x360,1920x1080 \
GRAFTON_NDI_OUTPUTS=jpeg85,webp-lossy75,webp-lossy75-effort0,webp-lossy75-fast \
  cargo test --release --locked --lib --features image-encoding \
  image_encoding_resources -- --ignored --nocapture
```

Without input files it uses synthetic photographic and graphics stand-ins.
`GRAFTON_NDI_PHOTO` and `GRAFTON_NDI_GRAPHICS` can name raw 1920×1080 RGBA
files; the harness scales them to each requested size and converts them to
several source formats. Preserve those inputs or their hashes with the results.
See the [harness](../src/image/tests.rs) for its output selections and repetition
rules. Synthetic measurements demonstrate controls, rather than establish a
production default.

## Historical measurements

The [archived report](https://github.com/GrantSparks/grafton-ndi/blob/9db3bf6a0305e1aa9c1da10288cbe6bc3c42f298/docs/image-encoding-performance.md)
preserves earlier single-encode tables and the dated downstream snapshot and
effort experiments. Their inputs and measurement boundaries differ. The
[snapshot CSV](https://github.com/GrantSparks/grafton-ndi/blob/9db3bf6a0305e1aa9c1da10288cbe6bc3c42f298/docs/benchmarks/image-formats-2026-11-13.csv)
and [effort CSV](https://github.com/GrantSparks/grafton-ndi/blob/9db3bf6a0305e1aa9c1da10288cbe6bc3c42f298/docs/benchmarks/image-effort-2026-11-13.csv)
are retained evidence for those experiments. The report's original timing and
memory tables have no accompanying raw results in this checkout, and its
downstream harness is not included. Treat those observations as historical,
with their stated scope, rather than current performance guarantees.
