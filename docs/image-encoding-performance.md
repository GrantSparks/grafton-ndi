# Image encoding performance

Choose image settings by measuring the application path: capture, conversion,
encoding, transfer and any consumer decode. Compare formats at acceptable visual
quality on representative content; equal JPEG and WebP quality numbers do not
establish equal quality. The [encoding guide](image-encoding.md) explains settings
and color/HDR handling.

## Whole-image buffering and concurrency

PNG and JPEG convert rows incrementally. JPEG retains its final converted
component row (three bytes per output pixel) for repeated padding requests.
WebP borrows eligible contiguous RGBA input or constructs a complete converted
image. Lossless WebP uses three bytes
per output pixel with alpha discarded, otherwise four; lossy WebP always takes
four, with opaque alpha when discarded. Both WebP formats also buffer the complete
encoded result before `encode_image_to` writes it. These are conversion/output
costs; codec state, source frames, reduction scratch and consumer buffers add to
memory use.

`ImageWorkspace` reuses wrapper conversion capacity, including the PNG/JPEG row,
across frames, with `retained_bytes()` for accounting and `release()` to discard it. Warm reuse
removes scratch allocation churn; the full WebP image still occupies memory.
Each concurrent worker needs its own workspace. Codec/output allocations
remain separate. Warm reuse changes allocation frequency, while the retained
conversion image still contributes to the working set.

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

## Compare workspace reuse

The workspace measurement compares cold and warm conversion storage for PNG,
lossless WebP, downscaled WebP and lossy WebP at 1080p and 4K:

```sh
cargo test --release --locked --lib --features image-encoding \
  workspace_resources -- --ignored --nocapture
```

It reports allocation counts, output size, retained scratch and elapsed time.
Peak newly allocated thread heap excludes buffers retained before tracking;
account for those retained bytes when comparing total memory. Linux process
RSS includes the entire test process and allocator retention, so it may remain
unchanged even when an encode allocates less scratch. Use representative inputs
and a bounded workload before choosing application settings.

## Thumbnail measurements, November 2026

These measurements compare the original encoder (`eadf64a`), the blocked
accumulator and full-block shifts (`974fcef`, A), and A plus the final JPEG
component-row cache (`1af7629`, A+B). The reference received only the measurement
harness from `8d79ed7`; typed-plan changes from `4b8b715` precede A. Dependency
versions were locked: `png` 0.18.1, `jpeg-encoder` 0.7.1, `image-webp` 0.2.4 and
`libwebpop` 0.1.0.

The exact measurement commits are retained in
[`benchmarks/issue-213`](https://github.com/GrantSparks/grafton-ndi/tree/benchmarks/issue-213),
so each encoder and the reference harness can be checked out independently.

The host was Linux x86-64, Intel Xeon w7-2475X, with Rust
`1.100.0 (0ef1bc386 2026-11-09)`, the default release profile and target settings.
The test process was pinned to logical CPU 39. Other development work used the
same host, so affinity reduced scheduling variation without isolating the core.
Each case warmed once, then recorded five complete encode samples, including
conversion, reduction, codec, output allocation and destruction of wrapper
scratch. Source creation, capture, network, data URLs, decoding and publication
were excluded. The returned encoded image remained live at the end of each
sample. Medians below use the unrounded sample times.

The corpus contains 240 cases: 1920×1080 and 3840×2160; synthetic `graphics` and
`graphics-alpha`; packed RGBA, UYVY, NV12, P216 and NA4G sources; and half,
quarter and eighth outputs for PNG Fast, JPEG quality 85, lossless WebP and
lossy WebP quality 75. Alpha was kept, depth was eight bits, and lossy WebP used
effort 4 and SharpYUV. Frames carried no color metadata; YUV conversion used
BT.709 limited range. `graphics-alpha` adds fully transparent, opaque and
varying-alpha regions to the RGBA input; the four selected YUV source formats
have no alpha. The harness prints input hashes and every sample:

```sh
GRAFTON_NDI_SIZES=1920x1080,3840x2160 \
GRAFTON_NDI_CONTENTS=graphics,graphics-alpha \
GRAFTON_NDI_OUTPUTS=png-fast-half,png-fast-quarter,png-fast-eighth,jpeg85-half,jpeg85-quarter,jpeg85-eighth,webp-half,webp-quarter,webp-eighth,webp-lossy75-half,webp-lossy75-quarter,webp-lossy75-eighth \
  taskset -c 39 cargo test --release --locked --lib --features image-encoding \
  image_encoding_resources -- --ignored --nocapture --test-threads=1
```

SHA-256 of the generated RGBA inputs, before conversion to each source format:

| Content | Dimensions | SHA-256 |
| --- | --- | --- |
| graphics | 1920×1080 | `9218b1fff684216fde696bb58dcb1c155ad6dba87ad810d93e8377a5458cd771` |
| graphics-alpha | 1920×1080 | `7cc3cf49a28914de7de0490f8a4efda107b6c663c26e9c50f630c0aa9b310372` |
| graphics | 3840×2160 | `178c2c0ccebf9738e1e657f78d5fc24270d835ce75f55f596c8e23c8efd829bb` |
| graphics-alpha | 3840×2160 | `86696e7c693c3cb0e92ecdc4744090eb5fc575970f7d5d81ac24991203e2d52d` |

The final paired sequence ran reference → A+B → A, following an earlier full
comparison. Each codec column contains 60 cases. Entries are geometric means
of the per-case median elapsed-time ratios; smaller than 1 means faster.

| Output | A / reference | A+B / reference |
| --- | ---: | ---: |
| PNG Fast | 0.9724 | 0.9673 |
| JPEG 85 | 0.9803 | 0.9567 |
| Lossless WebP | 0.9826 | 0.9980 |
| Lossy WebP 75 | 0.9659 | 0.9682 |

The separate JPEG cache improved A's JPEG cases by a geometric mean of 2.4%;
59 of 60 cases were faster. The earlier comparison measured 1.6%, with 55 of
60 faster. Other codecs do not use the JPEG cache; differences between A and
A+B on those paths show run variation and possible binary-layout effects,
not a benefit attributable to that cache. Repeated reference runs had a
median ratio of 1.0005; the middle 80% of per-case ratios were 0.9934–1.0136.
Lossless WebP has no consistent overall gain after both changes.

For absolute scale, these are the five-sample medians for **1920×1080 opaque
RGBA graphics encoded at eighth size**, taken from that same paired sequence:

| Output | Reference (ms) | A (ms) | A+B (ms) |
| --- | ---: | ---: | ---: |
| PNG Fast | 5.968 | 5.997 | 6.013 |
| JPEG 85 | 4.217 | 3.956 | 3.666 |
| Lossless WebP | 6.053 | 6.039 | 6.015 |
| Lossy WebP 75 | 13.409 | 13.078 | 13.190 |

The accumulator change retains the existing scratch layout: it combines plain
and alpha-weighted contributions in local block sums, then merges once per
block. It avoids doubling persistent weighted scratch. Peak live heap bytes
were unchanged in all 240 A cases. The JPEG cache adds one cold allocation of
three bytes per output pixel **in one row**, retained in `ImageWorkspace` for
warm reuse. At 1080p/eighth that is 720 bytes; the measured cold JPEG peak rose
from 32,116 to 32,836 bytes. Other codec peaks were unchanged. Warm conversion
allocation-count tests continue to allocate nothing after capacity is ready.

All 240 encoded benchmark outputs were byte-identical to the reference.
An additional 1,200 encoded fingerprints matched across every source pixel
format, both alpha modes, 8/16-bit PNG, all reductions, and partial edge blocks.
At eighth size and quality 85, a 1080-row source now requires 1,080 source-row
conversions instead of 1,152; the final converted component row serves the
codec's nine padding requests. Tests also cover a partial final source block.

These results justify retaining the blocked accumulator and final-row cache
for this corpus. They do not establish a universal speedup, image-quality
preference, production default, multiworker throughput or transport latency.
Measure representative application content and concurrency before choosing
settings.

## Output growth measurements, November 2026

The `workspace_output_growth_resources` harness was run on the unmodified encoder
plus the harness (`4dbc3b5`) before changing reservation, then on the output-hint
implementation. Both used locked dependencies, Rust 1.100.0, default release
settings, Linux x86-64 and an Intel Xeon w7-2475X. Other agents were building on
the same host, so these single-sample runs establish allocation behavior, not a
timing improvement.

The deterministic `photographic` input generated by the harness was converted
to BGRA with 16 bytes of row padding. Each case warmed conversion storage once,
then encoded PNG Fast, JPEG 85 or lossless WebP at full or eighth size. Counts
include codec state and output; peak requested live heap excludes preexisting
source and retained conversion scratch, allocator overhead, and transient
old/new memory inside a realloc. Output lengths were identical before and after.
The writer comparison independently counts capacity changes of the result Vec;
the warm-allocation test compares codec allocations against a retained writer
to establish exactly one output allocation.

```sh
cargo test --release --locked --lib --features image-encoding \
  workspace_output_growth_resources -- --ignored --nocapture
```

| Input | Output | Reduction | Encoded bytes | Output allocations | All allocations | Peak bytes |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| 1920×1080 | Png | Off | 5121433 | 16 → 1 | 23 → 8 | 8499712 → 5148569 |
| 1920×1080 | Png | Eighth | 46335 | 9 → 1 | 16 → 8 | 73168 → 53311 |
| 1920×1080 | Jpeg(85) | Off | 452367 | 17 → 1 | 26 → 10 | 616820 → 544899 |
| 1920×1080 | Jpeg(85) | Eighth | 3897 | 10 → 1 | 19 → 10 | 15988 → 15789 |
| 1920×1080 | WebpLossless | Off | 3356636 | 4 → 1 | 75 → 72 | 12488704 → 12488704 |
| 1920×1080 | WebpLossless | Eighth | 30350 | 5 → 1 | 94 → 90 | 162368 → 162368 |
| 3840×2160 | Png | Off | 20499658 | 18 → 1 | 25 → 8 | 33940480 → 20549834 |
| 3840×2160 | Png | Eighth | 181844 | 11 → 1 | 18 → 8 | 274624 → 191700 |
| 3840×2160 | Jpeg(85) | Off | 1754710 | 19 → 1 | 28 → 10 | 2281844 → 1939402 |
| 3840×2160 | Jpeg(85) | Eighth | 9495 | 12 → 1 | 21 → 10 | 39796 → 32907 |
| 3840×2160 | WebpLossless | Off | 13412924 | 4 → 1 | 77 → 74 | 49954816 → 49954816 |
| 3840×2160 | WebpLossless | Eighth | 116910 | 4 → 1 | 97 → 94 | 649472 → 649472 |

A workspace remembers the last successful encoded length for the same format
settings, rather than retaining its output memory. The reservation is capped
at twice the next image's raw bytes plus 1,024 bytes; it is a growth hint, not
an output limit. `release()` resets it. `encode_to` remains writer-controlled,
and lossy WebP still returns its backend's allocation directly.

Reservation happens on the first write. Eagerly reserving before calling the
lossless WebP backend increased peak heap from 12,488,704 to 15,845,340 bytes at
1080p, because the output overlapped its compression working set. Delaying the
reservation until that backend has completed compression preserves the original
peak while removing three output allocations. PNG and JPEG also reduce output
capacity and their measured peak. These results justify the hint for this
workload; they do not establish throughput or latency gains on other content.
