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

https://github.com/GrantSparks/grafton-ndi/blob/e3841377cc2f26447b6239165c086e89aa11b2ad/examples/NDIlib_Find.rs?rust

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
