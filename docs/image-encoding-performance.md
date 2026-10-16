# Image encoding resources in 2.0.0

Measured on 2026-10-16, Debian 12 x86_64, Intel Xeon w7-2475X, Rust 1.99.0,
`png` 0.18.1, and `jpeg-encoder` 0.7.1. These are local resource comparisons,
not a throughput guarantee. No live capture is involved in this measurement.

The ignored `image_encoding_resources` library test compares the former
full-frame BGRA conversion with the production row encoders. It covers
1920×1080, 3840×2160, and 7680×4320, contiguous and 64-byte-padded strides,
uniform and pseudorandom RGB pixels, PNG, and JPEG qualities 1, 85, and 100.
Every reported time is the median of three encodes. The same complete matrix
was run with the dependency's optional SIMD feature unified into the build.

```bash
cargo test --release --lib image_encoding_resources -- --ignored --nocapture
cargo test --release --lib --features jpeg-encoder/simd image_encoding_resources -- --ignored --nocapture
```

## Wrapper conversion scratch

PNG forwards initialized RGBA rows directly, including padded sources, with no
conversion allocation. BGRA/RGBX/BGRX reuse one checked `width * 4` byte row.
JPEG's private `ImageBuffer` adapter writes Y/Cb/Cr directly into codec-owned
component rows, using a fixed 48-byte stack batch and no heap staging buffer.
Codec/component storage, encoded output, and data-URL base64 storage are
additional to this conversion-scratch bound.

| BGRA dimensions | Former PNG staging | Row PNG scratch | Former JPEG staging | Row JPEG heap scratch |
| --- | ---: | ---: | ---: | ---: |
| 1920×1080 | 8,294,400 B | 7,680 B | 6,220,800 B | 0 B |
| 3840×2160 | 33,177,600 B | 15,360 B | 24,883,200 B | 0 B |
| 7680×4320 | 132,710,400 B | 30,720 B | 99,532,800 B | 0 B |

Unit tests hold width fixed while varying height, count the PNG conversion
allocation and capacity, and reserve the JPEG codec rows before asserting zero
allocations during conversion. Differential tests decode exact PNG pixels and
compare JPEG bytes with independently packed RGB for all four formats,
non-MCU-aligned sizes, quality boundaries, and padded layouts. The SIMD-enabled
build exercises the same differential tests.

## Encoding peak and wall time

The following default-feature results use contiguous, uniform BGRA. Peak bytes
are the maximum simultaneously live allocation sizes requested by the encoding
call on its thread, **including codec and output storage**. They exclude the
preexisting source frame and allocator overhead; they are not process RSS.

| Dimensions / encoding | Staged time | Row time | Staged encoding peak | Row encoding peak | Source + row encoding peak |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1920×1080 PNG | 18.60 ms | 14.93 ms | 8,675,416 B | 403,458 B | 8,697,858 B |
| 1920×1080 JPEG 85 | 16.77 ms | 14.50 ms | 6,378,868 B | 158,068 B | 8,452,468 B |
| 3840×2160 PNG | 72.01 ms | 57.60 ms | 33,608,886 B | 483,822 B | 33,661,422 B |
| 3840×2160 JPEG 85 | 64.92 ms | 52.50 ms | 25,198,964 B | 315,764 B | 33,493,364 B |
| 7680×4320 PNG | 290.92 ms | 231.68 ms | 133,303,646 B | 743,838 B | 133,454,238 B |
| 7680×4320 JPEG 85 | 292.17 ms | 210.51 ms | 100,426,100 B | 893,300 B | 133,603,700 B |

Output entropy matters: for contiguous 8K pseudorandom pixels, PNG encoding peak
fell from 583,862,696 to 136,040,286 B; including the source, the new peak was
268,750,686 B. Median time was 5772.47 versus 5671.62 ms. Encoded size changed
from 113,920,228 to 114,253,991 B because streaming changes compression chunking;
decoded pixels remain exact. JPEG 100 still needs 268,620,148 B of codec/output
heap for that input, despite eliminating its 99,532,800-byte RGB staging buffer.

Across the complete default-feature matrix, row/staged median time ratios were
0.796–0.995 for PNG and 0.711–0.933 for JPEG. With JPEG SIMD enabled they were
0.787–0.978 and 0.753–0.964, respectively. These runs found no repeatable
throughput regression requiring a separate contiguous path. JPEG output was
byte-identical to the staged baseline within each feature configuration.
SIMD versus scalar builds need not produce identical JPEG bitstreams.

The SDK regression `tests/video_padding_sdk.rs` separately exercises receiver
and FrameSync active rows, scoped worker reads, and encoding before the guard
frees the allocation on its permitted thread. Under NDI SDK 6.3.2 its Valgrind
Memcheck run reported zero errors; the synthetic uninitialized-padding tests
retain the independent deterministic layout checks.
