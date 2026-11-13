# Troubleshoot NDI

## Runtime loading

For `RuntimeLoadFailed`, read the attempted paths in the error and check the
[runtime search order](setup.md#finding-the-runtime). For `RuntimeConflict`,
check the selected path and edition: a successful selection lasts for the process.
For `UnsupportedCapability`, inspect the reason and the [requirements](setup.md#runtime-capabilities).

## SDK limitations

The following limitations affect supported runtimes. Measurements apply to Linux
x86_64 unless a platform is named. See [SDK validation](sdk-validation.md) for
evidence and the [File guide](file-io.md#limits-of-the-ndi-64-file-library) for recording precautions.

## Discovery Server

- **Destroying an object aborts the process on 6.3.x.** On a 6.3.2 runtime, destroying a `SendAdvertiser`, `RecvAdvertiser`, `SendListener` or `RecvListener` while it is connected to a server aborts the process ("pure virtual method called"). No order of calls avoids it, so the crate refuses all four before NDI 6.4.0 with `Error::UnsupportedCapability`. 6.4.0.2 does not abort.
- **A quick re-registration crashes listeners (6.3.2 and 6.4.0.2).** Moving one sender or receiver from one advertiser to another on the same server within a few tens of milliseconds can crash a listener on that server, in any process. Wait about a second between unregistering and registering again, or drop the first advertiser before registering with the second.
- **An unsubscribe can block a receiver's connect for good (6.3.2 and 6.4.0.2).** When a listener, in this process or another, unsubscribes from a receiver that a `RecvAdvertiser` registers while that receiver connects, its connect can block forever. Drop the listener to stop receiving a receiver's events; this crate's listeners never unsubscribe on their own.
- **A send advertiser's destroy can hang (6.4.0.2).** Dropping a `SendAdvertiser` can block for good, about once in a million drops under heavy load.
- **A listener that connects late misses earlier registrations (6.4.0.2).** An advertiser or listener that finds no server tries again every 10 seconds. A listener that connects on such a later attempt lists only the senders and receivers registered after it connected. Create listeners once their server is up, or replace a listener that took ten seconds or more to report `is_connected`.

## Receiving and sending

- **NDI|HX video needs a decoder the runtime can find (6.4).** Without one, a receiver of an NDI|HX source gets black 1920×1080 frames instead of the picture, and no error. On Linux the runtime decodes NDI|HX (H.264 and HEVC) only with the NDI|HX Driver for Linux, `libndihx.so` and its bundled FFmpeg libraries copied beside the runtime library, or with FFmpeg 7's `libavcodec` on the loader's path; NDI grants no H.264 or AAC licence with the driver. The Windows and macOS runtimes decode H.264 themselves. H.265 the SDK documents only in 4:2:0 and where decoding hardware supports it: macOS decodes HEVC 4:4:4, and Windows without a GPU delivers the black frames for it.
- **A FrameSync takes its receiver's audio.** Raw audio capture beside a FrameSync (measured with 6.3.2 and 6.4) takes the frames the FrameSync needs, and the FrameSync returns silence in their place. A FrameSync's receiver therefore has no audio capture.
- **Clocked output can outlive input.** FrameSync can provide silent audio or retained video while an input stream is stalled. A known input format, positive queue depth or active connection does not prove new media arrived. Use the [per-media reception monitor](usage.md#monitor-reception-of-the-expected-media) on the diagnostics path.
- **The 32-bit x86 Linux runtime returns 3840×2160 video with wrong chroma in places (6.4).** A UYVY picture of that size, sent and received over loopback, comes back with the chroma inside flat areas off by up to 41 code values, and two receivers of one stream are not given the same picture. Its luma is intact, and so are 720×576 and 1920×1080 frames.
- **Standard encoding may convert 4:4:4 video to 4:2:2.** Sending a planar 4:4:4 frame does not guarantee that a receiver gets 4:4:4.
- **Sender and receiver names are reserved machine-wide** through lock files in the temporary directory (`NTK_SEND_<name>`, `NTK_RECV_<name>`). Two processes that share `TMPDIR` cannot each create a sender, or a receiver, of the same name at the same time.
- **Some waits cannot be cut short.** A `Genlock` clock's wait cannot be cancelled or given a deadline. Dropping a `QueuedSender` waits, without a timeout, for the SDK to finish with every queued frame. An async call whose SDK work has started runs to the end of that SDK call even if its future is dropped.
- **On Windows a receiver may never connect when multicast DNS is unavailable (6.3.2 and 6.4).** Even a receiver given an explicit loopback address: its connection count stays 0 and it opens no connection. With multicast DNS blocked, for example by a firewall, this happens to receivers of any source. With a Discovery Server configured that the SDK cannot reach, it happens to about half of the receivers of a source that the process connected to before. Keep multicast DNS open to the program or the Discovery Server reachable; `Receiver::reconnect` revives most receivers that have not connected.
- **On Windows the 6.4 runtime's genlock does not acquire a reference** (Advanced). A `Genlock` connects to a reference sent from the same machine, whose sender counts the connection, but `is_active` stays false and its clocks keep to the system clock. The 6.3.2 runtime on Windows, and 6.4 on Linux and macOS, acquire the same reference.
- **Stream tier and stream selection need a licensed vendor ID** (Advanced). Without one, the SDK ignores `set_stream_tier` and `set_streams` and reports no change, which the crate cannot tell from "already in effect".

## File library

The NDI 6.4 File library can crash on a write failure during recording and can
report a file closed without all data reaching disk. On macOS, a source switch
can leave the previous file unfinalized despite a closed event. Provision enough
space, inspect `finish` and its report, and read back recordings whose completeness
matters. See [File library limits](file-io.md#limits-of-the-ndi-64-file-library).
