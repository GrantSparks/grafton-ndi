# grafton-ndi

Unofficial idiomatic Rust bindings for the [NDI 6 SDK](https://ndi.video/for-developers/ndi-sdk/).

## Requirements

This library has been developed and tested on Windows 10, but it should work on other platforms easily enough (please contribute!). You need to have the [NDI 6 SDK](https://ndi.video/for-developers/ndi-sdk/) installed for your platform. After installation, make sure your library path or system PATH (on Windows) includes the NDI library binaries location, e.g., `%NDI_SDK_DIR%\Bin\x64` (for Windows PATH).

You also need to install Rust bindgen [according to the instructions here](https://rust-lang.github.io/rust-bindgen/requirements.html).

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
grafton-ndi = "*"
```

Ensure that you have set up the environment variables correctly to point to your NDI SDK installation.

## Examples

Examples inspired by the official NDI 6 SDK examples can be found in the `examples` directory. To run them, you will need to have the NDI SDK installed and in your PATH.

To run an example, use the following command:

```sh
cargo run --example NDIlib_Find
```

## Usage

The following example demonstrates how to use the `grafton-ndi` library to find NDI sources on the network and receive frames from a source.

```rust
use std::time::{Duration, Instant};

use grafton_ndi::{Find, Finder, FrameType, Receiver, Recv, RecvBandwidth, RecvColorFormat, NDI};

fn main() -> Result<(), &'static str> {
    // Initialize the NDI library and ensure it's properly cleaned up
    if let Ok(_ndi) = NDI::new() {
        // Create an NDI finder to locate sources on the network
        let finder = Finder::default();
        let ndi_find = Find::new(finder)?;

        // Wait up to 5 seconds to check for new sources
        if !ndi_find.wait_for_sources(5000) {
            println!("No sources found.");
            return Err("No sources found");
        }

        // Get the list of sources
        let sources = ndi_find.get_sources(5000);
        if sources.is_empty() {
            println!("No sources found.");
            return Err("No sources found");
        }

        // Display all the sources
        println!("Network sources ({} found):", sources.len());
        for (i, source) in sources.iter().enumerate() {
            println!("{}. {}", i + 1, source.name);
        }

        // Create a receiver to connect to the first source
        let source_to_connect_to = sources[0].clone();
        let receiver = Receiver::new(
            source_to_connect_to,
            RecvColorFormat::UYVY_BGRA,
            RecvBandwidth::Highest,
            true,
            Some("Example Receiver".to_string()),
        );

        let ndi_recv = Recv::new(receiver).expect("Failed to create NDI recv instance");

        // Run for 5 seconds
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            // Receive something
            match ndi_recv.capture(1000) {
                Ok(FrameType::None) => {}
                Ok(FrameType::Video(_)) => {
                    println!("Received a video frame");
                    // Handle video frame
                }
                Ok(FrameType::Audio(_)) => {
                    println!("Received an audio frame");
                    // Handle audio frame
                }
                Ok(FrameType::Metadata(_)) => {
                    println!("Received a metadata frame");
                    // Handle metadata frame
                }
                Err(_) => {
                    println!("Failed to receive frame");
                }
            }
        }

        // Destroy the receiver
        drop(ndi_recv);
    } else {
        return Err("Failed to initialize NDI library");
    }

    // The Drop trait for NDI will take care of calling NDIlib_destroy()
    Ok(())
}
```

## Best Practices

1. **Initialization and Deinitialization**:

   - Always initialize the NDI library before using it and deinitialize it when done.
   - Use `NDI::new()` for initialization and rely on Rust's ownership and drop semantics for cleanup.

2. **Error Handling**:

   - Check for null pointers and handle errors gracefully.
   - Use Rust's `Result` type to manage potential errors.

3. **Memory Management**:

   - Ensure that you properly destroy any NDI instances you create to avoid memory leaks.
   - Leverage Rust's ownership system to manage resources efficiently.

4. **Concurrency**:
   - If using NDI in a multi-threaded environment, ensure proper synchronization.

## Contributing

Contributions are welcome! Please submit a pull request or open an issue to discuss what you would like to change.

## License

This project is licensed under the Apache License, Version 2.0. See the [LICENSE](LICENSE) file for more details.
