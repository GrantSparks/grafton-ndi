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

## Configuration

The NDI SDK URLs can be overridden using GitHub secrets:
- `NDI_SDK_URL` - Windows NDI SDK installer URL
- `NDI_SDK_URL_MACOS` - macOS NDI SDK installer URL

## Notes

- The macOS action needs the correct SHA256 hash for the NDI SDK installer to be updated
- Both actions cache their installations to speed up CI runs
- The Windows action also installs LLVM which is required for building on Windows