# Examples

Install a runtime using the [setup guide](../docs/setup.md).

Run an example with `cargo run --example <name>`. Most accept extra IP addresses to search as arguments. The comment at the top of each file says what it does and lists its options.

## Discovery and monitoring

| Example | Command | What it shows |
| --- | --- | --- |
| `runtime_selection.rs` | `cargo run --example runtime_selection` | Select a runtime, inspect it, then start NDI. |
| `NDIlib_Find.rs` | `cargo run --example NDIlib_Find` | Discover NDI sources on the network. |
| `status_monitor.rs` | `cargo run --example status_monitor` | Print a receiver's connection status changes. |
| `routing.rs` | `cargo run --example routing` | Route receivers to each discovered source in turn. |
| `discovery_server.rs` | `cargo run --example discovery_server -- 127.0.0.1:5959` | Register a sender with a Discovery Server and list what the server reports. Needs a Discovery Server and an NDI 6.4.0 or later runtime. |

## Receiving

| Example | Command | What it shows |
| --- | --- | --- |
| `NDIlib_Recv_Audio.rs` | `cargo run --example NDIlib_Recv_Audio` | Receive and inspect audio. |
| `NDIlib_Recv_Audio_16bpp.rs` | `cargo run --example NDIlib_Recv_Audio_16bpp` | Receive audio and convert it to 16-bit samples. |
| `NDIlib_Recv_FrameSync.rs` | `cargo run --example NDIlib_Recv_FrameSync` | Clock-corrected capture with `FrameSync`. |
| `NDIlib_Recv_PNG.rs` | `cargo run --features image-encoding --example NDIlib_Recv_PNG` | Receive video and save a PNG. Needs the `image-encoding` feature. |
| `NDIlib_Recv_PTZ.rs` | `cargo run --example NDIlib_Recv_PTZ` | Control PTZ cameras. |
| `concurrent_capture.rs` | `cargo run --example concurrent_capture` | Capture video, audio and metadata from one receiver on separate threads. |

## Sending

| Example | Command | What it shows |
| --- | --- | --- |
| `NDIlib_Send_Audio.rs` | `cargo run --example NDIlib_Send_Audio` | Send audio. |
| `NDIlib_Send_Video.rs` | `cargo run --example NDIlib_Send_Video` | Send video. |
| `async_send.rs` | `cargo run --example async_send` | Reuse two owned frames for async sending and recover completed frames. |
| `async_send_copy.rs` | `cargo run --example async_send_copy` | Copy borrowed pixels into async sends, then into the frames they return. |
| `zero_copy_send.rs` | `cargo run --example zero_copy_send` | Render into two reusable owned frames without per-send pixel copies. |

## Advanced SDK

These need the `advanced` feature, an Advanced SDK at build time and an Advanced runtime:

| Example | Command | What it shows |
| --- | --- | --- |
| `advanced_source_metadata.rs` | `cargo run --features advanced --example advanced_source_metadata` | List sources with the metadata they advertise. |
| `advanced_passthrough.rs` | `cargo run --features advanced --example advanced_passthrough` | Relay compressed video and audio without decoding. |
| `advanced_queued_send.rs` | `cargo run --features advanced --example advanced_queued_send` | Queue video frames and reuse each once it has been sent. |
| `advanced_allocator.rs` | `cargo run --features advanced --example advanced_allocator` | Receive frames into memory the application supplies. |
| `advanced_avsync.rs` | `cargo run --features advanced --example advanced_avsync` | Fetch the audio that belongs to each video frame. |
| `advanced_genlock.rs` | `cargo run --features advanced --example advanced_genlock` | Pace a sender from another source's clock. |
| `advanced_kvm.rs` | `cargo run --features advanced --example advanced_kvm` | Remote keyboard and mouse control of a source that offers it. |

## File API

These need the `file-io` feature and the NDI File library of the 6.4 SDK; on Linux and macOS that means the Advanced SDK runtime:

| Example | Command | What it shows |
| --- | --- | --- |
| `file_record.rs` | `cargo run --example file_record --features file-io -- "MACHINE (Source)" take 10` | Record a source to disk and wait for the file to be finalized. |
| `file_play.rs` | `cargo run --example file_play --features file-io -- take_001.mkv "Replay"` | Play a recording back as an NDI source. |
| `file_thumbnails.rs` | `cargo run --example file_thumbnails --features file-io,image-encoding -- take_001.mkv 8` | Seek through a recording and write a quarter-size JPEG thumbnail at each position. |
