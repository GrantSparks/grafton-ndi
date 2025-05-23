# Contributing to grafton-ndi

Thank you for your interest in contributing to grafton-ndi! This document provides guidelines and information for contributors.

## Code of Conduct

By participating in this project, you agree to abide by our code of conduct: be respectful, constructive, and professional.

## Getting Started

1. Fork the repository on GitHub
2. Clone your fork locally
3. Create a new branch for your feature or fix
4. Make your changes
5. Run tests and ensure they pass
6. Submit a pull request

## Development Setup

### Prerequisites

- Rust 1.75 or later
- NDI SDK 6.x installed (see README for platform-specific instructions)
- Git

### Building

```bash
# Clone the repository
git clone https://github.com/GrantSparks/grafton-ndi.git
cd grafton-ndi

# Build the project
cargo build

# Run tests
cargo test

# Run examples
cargo run --example NDIlib_Find
```

## Contribution Guidelines

### Code Style

- Follow Rust idioms and conventions
- Use `cargo fmt` to format code
- Run `cargo clippy` and address warnings
- Add rustdoc comments for all public APIs
- Include examples in documentation where helpful

### Commit Messages

- Use conventional commit format: `type(scope): description`
- Types: `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `chore`
- Keep the first line under 72 characters
- Reference issues when applicable

Examples:
```
feat(recv): add support for compressed video formats
fix(send): prevent panic on invalid frame dimensions
docs(api): improve VideoFrame builder documentation
```

### Pull Requests

1. **Title**: Use a clear, descriptive title
2. **Description**: Explain what changes you made and why
3. **Testing**: Describe how you tested your changes
4. **Breaking Changes**: Clearly mark any breaking API changes

### Testing

- Add tests for new functionality
- Ensure all existing tests pass
- Include integration tests for complex features
- Test on multiple platforms if possible

### Documentation

- Update relevant documentation for any API changes
- Add rustdoc comments for new public items
- Update README if adding new features
- Include code examples where appropriate

## Areas for Contribution

### High Priority

- macOS platform testing and fixes
- Performance optimizations
- Additional examples
- Documentation improvements

### Feature Ideas

- Async/await support
- Additional color format conversions
- Enhanced metadata handling
- Debugging and diagnostic tools

### Known Issues

Check the [issue tracker](https://github.com/GrantSparks/grafton-ndi/issues) for current bugs and feature requests.

## Architecture Overview

### Project Structure

```
grafton-ndi/
├── src/
│   ├── lib.rs          # Main library interface
│   ├── error.rs        # Error types
│   └── ndi_lib.rs      # Generated FFI bindings
├── build.rs            # Build script for bindgen
├── examples/           # Example applications
└── tests/              # Integration tests
```

### Key Design Principles

1. **Safety First**: Wrap all unsafe FFI in safe Rust APIs
2. **Zero-Cost Abstractions**: Minimize overhead over raw NDI SDK
3. **Idiomatic Rust**: Follow Rust conventions and patterns
4. **Thread Safety**: Properly implement Send/Sync where appropriate

## Questions?

- Open an issue for bugs or feature requests
- Start a discussion for design questions
- Check existing issues before creating new ones

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0.