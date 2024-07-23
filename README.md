# grafton-ndi

Unofficial idiomatic Rust bindings for the [NDI 6 SDK](https://ndi.video/for-developers/ndi-sdk/).

## Requirements

This library has been developed and tested on Windows 10, but it should work on other platforms easily enough (please contribute!). You need to have the [NDI 6 SDK](https://ndi.video/for-developers/ndi-sdk/) installed for your platform. After installation, make sure your library path (or system PATH on Windows) includes the NDI library binaries location, (e.g., `%NDI_SDK_DIR%\Bin\x64` for Windows PATH).

You also need to install Rust bindgen [according to the instructions here](https://rust-lang.github.io/rust-bindgen/requirements.html).

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
grafton-ndi = "*"
```

Ensure that you have set up the environment variables correctly for your NDI SDK installation.

## Examples

Examples inspired by the official NDI 6 SDK examples can be found in the `examples` directory. To run them, you will need to have the NDI SDK installed and in your PATH.

To run an example, use the following command:

```sh
cargo run --example NDIlib_Find
```

## Contributing

Contributions are welcome! Please submit a pull request or open an issue to discuss what you would like to change.

## License

This project is licensed under the Apache License, Version 2.0. See the [LICENSE](LICENSE) file for more details.
