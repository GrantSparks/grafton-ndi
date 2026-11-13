# Image encoding performance

Time and peak memory of image encoding for 3.0, by source pixel format, image
size, content and output. The [image encoding guide](image-encoding.md) says
what each setting does; this page says what it costs.

The original single-encode tables below cover PNG, JPEG and lossless WebP.
The lossy WebP sections add downstream snapshot batch measurements and a
reproducible comparison of encoding effort. These have different inputs and
measurement boundaries; compare results within each experiment.

Measured on Debian 12 x86_64, Intel Xeon w7-2475X, Rust 1.100.0, release
build for the default x86-64 target, with `png` 0.18.1, `jpeg-encoder` 0.7.1
and `image-webp` 0.2.4. One encode at a time on one thread; treat differences
under about 10% as noise.
Times are the median of five encodes, or of one for PNG `Balanced` and `High`
and at 7680×4320. Peaks are the largest total of live allocations the encode
made on its thread, codec state and the encoded image included, and exclude
the source frame.

```bash
GRAFTON_NDI_PHOTO=photo.rgba GRAFTON_NDI_GRAPHICS=graphics.rgba \
  cargo test --release --lib --features image-encoding image_encoding_resources -- --ignored --nocapture
```

The two inputs are raw 1920×1080 RGBA files, scaled to each size: a photograph,
and a graphics frame of flat boxes, a gradient and text. Without them the test
uses synthetic stand-ins, which compress differently. YUV sources are the same
pictures converted to each format with BT.709 limited range.
`GRAFTON_NDI_SIZES=1920x1080` limits the run to one size.

## Lossy WebP for snapshot consumers

Quality controls color fidelity. `webp_effort(level)` controls compression work
separately, from 0 to 6 (default 4), and `webp_sharp_yuv(false)` trades sharper chroma
conversion for less CPU work (default true). Alpha remains exact. For a latency
comparison, hold quality fixed and measure both settings on your content.

### Five concurrent snapshot round trips

Synemantic's downstream evaluation used two photographic proxies resized to
preserve aspect ratio, five CPU worker threads encoding the same scene
independently, and no GPU. Host: Intel Xeon w7-2475X, Linux x86-64, Rust 1.95.0,
optimized release build with the default CPU target. Each codec had a warmup
followed by three measured rounds; the figures are medians of elapsed batch
wall time, including encoding, final RGB decoding, synchronization and joins.
They exclude thread, frame and options setup. They are neither sums of five
encodes nor per-frame encoding times.

The JPEG comparator reproduces JPEG 80 capture → RGB decode → canonical JPEG
90, then final RGB decode. WebP uses quality 75, effort 4 and SharpYUV, with
alpha discarded. Quality numbers do not imply equal visual quality. The
evaluation pins grafton-ndi `908a441`, `libwebpop` 0.1.0, `image` 0.25.10 and
`image-webp` 0.2.4; the new effort controls preserve those WebP defaults.

| Scene | Dimensions | JPEG pipeline batch ms | WebP 75 batch ms | JPEG bytes/image | WebP bytes/image |
| --- | --- | ---: | ---: | ---: | ---: |
| Audience preview | 560×360 | 12.712 | 56.632 | 52,359 | 21,900 |
| Lobby preview | 624×360 | 14.564 | 65.268 | 53,307 | 18,686 |
| Audience larger | 1680×1080 | 105.222 | 412.551 | 388,353 | 134,464 |
| Lobby larger | 1500×864 | 75.496 | 283.168 | 223,758 | 65,600 |

The [96 recorded rows](benchmarks/image-formats-2026-11-13.csv) include one-
and five-worker runs, other quality levels, lossless WebP and reconstruction
error. Inputs were the JPEG photographs
[Audience](https://www.nasa.gov/wp-content/uploads/2025/05/grc-1977-c-01735.jpg)
and [Lobby](https://www.nasa.gov/wp-content/uploads/2025/05/grc-2019-c-03388.jpg),
decoded before comparison. These are photographic proxies, not live camera
feeds or original raw pixels. The CSV's source paths name temporary inputs.

The harness excludes NDI wait, YUV conversion, network transfer, filesystem
publication, peak memory and GPU inference. Five workers processing the same
scene do not qualify sustained camera scheduling. The larger WebP batches
already exceed a 200 ms batch budget before those additional costs. The
smaller payloads can suit bandwidth-constrained snapshots; they do not justify
changing a latency-sensitive JPEG default without measuring the complete path.

### Whole-image buffering and concurrency

Lossy WebP holds a complete encoded image before writing, including with
`encode_image_to`. Unless an unchanged contiguous RGBA image can be borrowed,
the wrapper also holds four converted bytes per output pixel. Alpha discard
still uses opaque RGBA. The backend's image planes, compression state and
output, any reduction scratch, retained source frames, and the consumer's RGB
decode buffers add to that storage.

For example, five 1500×864 encodes that need conversion use about 24.7 MiB for
the wrapper's converted images alone; this is not their peak memory. Concurrent
jobs have independent buffers, so bound active workers and pending requests
as well as measuring end-to-end latency. Reducing effort does not turn WebP
into a row-streaming codec or impose a memory limit. PNG and JPEG convert rows
incrementally; their codec state and encoded output still consume memory.

### Encoding effort on a synthetic input

The crate's resource harness below measures a single encode at a time, without
RGB decoding or NDI. These additional measurements use its built-in synthetic
`photographic` stand-in, not the photographs used in either table above or
below. Host: the same Xeon w7-2475X and Debian 12 as the original single-encode
tables, Rust 1.100.0, optimized release build, default CPU target, `libwebpop`
0.1.0. Each time is the median of five encodes, with no separate warmup. Peak
is live heap allocation during encoding, excluding the preexisting source
frame and allocator overhead; it is not process RSS.

| Dimensions | Output | RGBA ms | UYVY ms | RGBA bytes/image | RGBA peak MiB |
| --- | --- | ---: | ---: | ---: | ---: |
| 640×360 | JPEG 85 | 2.6 | 3.2 | 59,480 | 0.09 |
| 640×360 | WebP 75, effort 4, SharpYUV | 58.0 | 58.6 | 30,854 | 2.21 |
| 640×360 | WebP 75, effort 0, SharpYUV | 24.2 | 24.6 | 38,184 | 2.21 |
| 640×360 | WebP 75, effort 0, standard conversion | 8.8 | 9.5 | 38,904 | 0.44 |
| 1920×1080 | JPEG 85 | 22.9 | 28.2 | 491,480 | 0.59 |
| 1920×1080 | WebP 75, effort 4, SharpYUV | 506.9 | 509.9 | 251,334 | 19.81 |
| 1920×1080 | WebP 75, effort 0, SharpYUV | 212.0 | 215.6 | 286,794 | 19.81 |
| 1920×1080 | WebP 75, effort 0, standard conversion | 73.8 | 79.1 | 298,056 | 3.92 |

These peaks use borrowed contiguous RGBA for that source; a converted UYVY
image adds width × height × 4 bytes. Lower effort and standard conversion
reduce work here but still buffer the full encoded image. Payload size and
reconstructed pixels can both change when effort or conversion changes; equal
quality settings do not imply identical output. Synthetic results demonstrate
the controls, and do not select a photographic production default.

[All 80 rows](benchmarks/image-effort-2026-11-13.csv) also cover the synthetic
graphics input and NV12, P216 and NA4G sources. Reproduce without image files:

```bash
GRAFTON_NDI_SIZES=640x360,1920x1080 \
GRAFTON_NDI_OUTPUTS=jpeg85,webp-lossy75,webp-lossy75-effort0,webp-lossy75-fast \
  cargo test --release --locked --lib --features image-encoding \
  image_encoding_resources -- --ignored --nocapture
```

## 1920×1080

Milliseconds, and the image's size for an RGBA source.

| Output | RGBA | UYVY | NV12 | P216 | NA4G | Image from RGBA |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| **Photographic** | | | | | | |
| PNG `Fast` | 24.2 | 30.3 | 29.6 | 30.2 | 31.2 | 3.76 MB |
| PNG `Balanced` | 554 | 550 | 556 | 560 | 562 | 3.38 MB |
| PNG `High` | 1,198 | 1,046 | 1,038 | 1,201 | 1,224 | 3.35 MB |
| 16-bit PNG `Fast` | 52.9 | 67.4 | 57.7 | 58.6 | 58.9 | 7.34 MB |
| 16-bit PNG `Balanced` | 823 | 1,336 | 1,274 | 1,153 | 1,143 | 4.23 MB |
| JPEG 85 | 20.8 | 26.2 | 25.4 | 25.5 | 26.7 | 0.45 MB |
| Lossless WebP | 18.8 | 26.3 | 25.4 | 25.6 | 27.3 | 2.80 MB |
| PNG `Fast`, quarter size | 8.1 | 11.3 | 10.7 | 10.8 | 11.7 | 0.29 MB |
| JPEG 85, quarter size | 5.9 | 11.0 | 10.4 | 10.5 | 11.4 | 0.04 MB |
| Lossless WebP, quarter size | 7.5 | 11.2 | 10.6 | 10.7 | 11.7 | 0.23 MB |
| **Graphics** | | | | | | |
| PNG `Fast` | 7.7 | 13.7 | 13.1 | 13.1 | 14.2 | 492 KB |
| PNG `Balanced` | 39.9 | 54.3 | 55.9 | 48.5 | 47.1 | 150 KB |
| PNG `High` | 104 | 116 | 122 | 111 | 112 | 135 KB |
| 16-bit PNG `Fast` | 20.8 | 27.5 | 24.7 | 30.8 | 26.7 | 667 KB |
| 16-bit PNG `Balanced` | 70.4 | 84.6 | 84.4 | 132 | 109 | 220 KB |
| JPEG 85 | 15.8 | 21.0 | 20.3 | 20.3 | 21.1 | 97 KB |
| Lossless WebP | 7.9 | 15.4 | 14.2 | 14.0 | 14.9 | 329 KB |
| PNG `Fast`, quarter size | 7.2 | 10.4 | 9.9 | 9.7 | 10.5 | 92 KB |
| JPEG 85, quarter size | 5.0 | 10.3 | 9.7 | 9.8 | 10.3 | 15 KB |
| Lossless WebP, quarter size | 7.2 | 11.2 | 10.6 | 10.4 | 10.5 | 55 KB |

- PNG's cost is compression: `Balanced` takes 23 times as long as `Fast` on
  the photograph for 10% fewer bytes, and `High` twice as long again.
- Lossless WebP is 17% smaller than PNG `Balanced` on the photograph and 29
  times faster; on graphics it is 2.2 times PNG's size.
- A 16-bit PNG of a YUV source holds real precision in every channel, so it
  is larger and slower to compress than a 16-bit PNG of an 8-bit RGBA source,
  whose low bytes repeat its high ones.

## 3840×2160 and 7680×4320

Milliseconds for the photograph and the graphics frame.

| Output | 4K RGBA | 4K UYVY | 8K RGBA | 8K UYVY |
| --- | ---: | ---: | ---: | ---: |
| **Photographic** | | | | |
| PNG `Fast` | 38.9 | 63.3 | 71.8 | 167 |
| PNG `Balanced` | 697 | 714 | 844 | 937 |
| JPEG 85 | 77.2 | 98.2 | 275 | 357 |
| Lossless WebP | 39.8 | 70.2 | 161 | 305 |
| JPEG 85, quarter size | 21.3 | 43.0 | 83.6 | 168 |
| **Graphics** | | | | |
| PNG `Fast` | 15.2 | 40.2 | 43.5 | 141 |
| PNG `Balanced` | 87.5 | 109 | 260 | 349 |
| JPEG 85 | 61.1 | 81.7 | 241 | 322 |
| Lossless WebP | 25.2 | 76.1 | 143 | 298 |
| JPEG 85, quarter size | 21.8 | 40.8 | 78.3 | 162 |

The photograph is a 1920×1080 picture scaled up, so its larger sizes compress
better than real 4K or 8K camera video would.

## YUV against RGBA

Converting YUV costs about 2.9 ns a pixel on the default x86-64 target: 6 ms
at 1080p, 24 ms at 4K and 95 ms at 8K. An RGBA frame costs nothing to convert,
because PNG and WebP read its rows in place. A UYVY source stays within 1.25
times an RGBA source's time where the codec's own work dominates, and not where
the codec is about as fast as the conversion:

| UYVY time over RGBA time | 1080p photo | 1080p graphics | 4K photo | 8K photo |
| --- | ---: | ---: | ---: | ---: |
| PNG `Balanced` | 0.99 | 1.36 | 1.02 | 1.11 |
| PNG `High` | 0.87 | 1.11 | 1.02 | 1.05 |
| PNG `Fast` | 1.25 | 1.78 | 1.63 | 2.33 |
| JPEG 85 | 1.26 | 1.33 | 1.27 | 1.30 |
| Lossless WebP | 1.40 | 1.95 | 1.76 | 1.89 |

The conversion is safe scalar Rust that the compiler vectorizes where the
target allows: built with `RUSTFLAGS="-C target-cpu=x86-64-v3"` it takes
1.6 ns a pixel. Bringing the fast codecs within 1.25 times on every target
would require additional SIMD optimization. A receiver set to an RGB
color format avoids the conversion altogether, at the cost of the SDK
converting every frame instead of only the ones encoded.

## Memory

PNG and JPEG convert one row at a time, so their conversion scratch does not
grow with the frame's height; both WebP codecs may hold a whole converted
image. The following table describes lossless WebP; lossy WebP always uses
four converted bytes per pixel when it cannot borrow RGBA, even with alpha
discarded, and also buffers its complete encoded output. The unit tests count
these allocations for every format, depth and
reduction, with the height varied at a fixed width, and check the WebP buffer's
exact size.

| Configuration | PNG conversion scratch | JPEG heap scratch | WebP buffer |
| --- | --- | --- | --- |
| RGBA, contiguous rows | None | None | None |
| RGBA with padded rows | None | None | width × height × 4 bytes |
| Otherwise, 8 bits | width × 4 bytes, × 3 with `AlphaMode::Discard` | None | width × height × 4, × 3 with `Discard` |
| 16 bits | width × 8 bytes, × 6 with `Discard` | — | — |
| With `Downscale` by f | ceil(width / f) × 16 bytes of sums (× 32 at 16 bits) and one reduced row | The sums | The reduced image, and the sums |

The measured peaks show the same: a UYVY source's peak exceeds an RGBA
source's by one row for PNG, by nothing for JPEG, and by exactly the converted
image for WebP.

| Peak, photograph | 1080p RGBA | 1080p UYVY | 4K RGBA | 4K UYVY | 8K RGBA | 8K UYVY |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| PNG `Fast` | 4,263,424 | 4,271,104 | 8,522,752 | 8,538,112 | 8,568,832 | 8,599,552 |
| JPEG 85 | 616,820 | 616,820 | 2,281,844 | 2,281,844 | 4,563,316 | 4,563,316 |
| Lossless WebP | 12,488,704 | 20,783,104 | 37,371,904 | 70,549,504 | 136,904,704 | 269,615,104 |
| JPEG 85, quarter size | 96,628 | 96,628 | 323,956 | 323,956 | 647,540 | 647,540 |

Bytes. The rest of each peak is the codec's own state and the encoded image,
growing in a `Vec` that doubles its capacity; lossless WebP's encoder holds a
copy of the image besides.

## Memory safety

`tests/video_padding_sdk.rs` encodes UYVY and P216 frames read in place from
the SDK's padded rows, 1,312-byte strides for 642-pixel rows, with every
output setting, and compares each image with the image of the frame's packed
copy. Run under Valgrind Memcheck, it reports no errors in the crate's code
with the Standard 6.3.2 and 6.4.0.2 runtimes. The x86 builds of the SDK read up
to 28 bytes past the end of many UYVY frames they send. The crate keeps room for
that after every frame it sends, which
[SDK validation](sdk-validation.md#reads-past-the-end-of-sent-video)
describes, and the test sends from a vector that ends with the frame.
