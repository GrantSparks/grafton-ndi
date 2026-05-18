# GitHub Actions for grafton-ndi

This directory contains custom GitHub Actions for setting up the NDI SDK on different platforms.

## setup-ndi-windows

Sets up the NDI SDK and LLVM on Windows runners.

- Downloads and installs NDI SDK 6
- Installs LLVM via chocolatey
- Caches installations for faster subsequent runs
- Verifies SDK integrity via SHA256 hash

## setup-ndi-macos

Sets up the NDI SDK on macOS runners.

- Downloads and installs NDI SDK 6 for macOS
- Extracts SDK without requiring system-wide installation
- Caches installations for faster subsequent runs
- Sets up library paths for dynamic linking

## setup-ndi-linux

Sets up the NDI SDK on Linux runners.

- Downloads and extracts NDI SDK 6 for Linux
- Verifies SDK integrity via SHA256 hash allowlist
- Caches installations for faster subsequent runs
- Creates the library symlinks expected by the Rust linker

## Configuration

The NDI SDK URLs can be overridden using GitHub secrets:
- `NDI_SDK_URL` - Windows NDI SDK installer URL
- `NDI_SDK_URL_MACOS` - macOS NDI SDK installer URL
- `NDI_SDK_URL_LINUX` - Linux NDI SDK installer URL

## Notes

- The setup actions verify NDI SDK installers against a small allowlist of known-good SHA256 hashes because NDI can rotate installer payloads behind the same download URL
- All setup actions cache their installations to speed up CI runs
- The Windows action also installs LLVM which is required for building on Windows
