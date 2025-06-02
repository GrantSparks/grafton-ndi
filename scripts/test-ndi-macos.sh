#!/bin/bash
# Test script to help debug NDI SDK installation on macOS

echo "=== NDI SDK Debug Script for macOS ==="
echo ""

# Check common NDI installation locations
echo "1. Checking common NDI installation directories:"
echo "----------------------------------------"

NDI_LOCATIONS=(
  "/Library/NDI SDK for Apple"
  "/Library/NDI SDK for macOS"
  "/Library/Application Support/NDI SDK for Apple"
  "/Applications/NDI SDK for Apple"
  "/usr/local/ndi"
  "/usr/local/lib/ndi"
  "$HOME/Library/NDI SDK for Apple"
)

for location in "${NDI_LOCATIONS[@]}"; do
  if [ -d "$location" ]; then
    echo "✓ Found: $location"
    if [ -f "$location/include/Processing.NDI.Lib.h" ]; then
      echo "  ✓ Header file exists"
    else
      echo "  ✗ Header file NOT found"
    fi
    if [ -d "$location/lib" ]; then
      echo "  ✓ lib directory exists"
      if [ -d "$location/lib/macOS" ]; then
        echo "    ✓ lib/macOS subdirectory exists"
        ls -la "$location/lib/macOS/"*.dylib 2>/dev/null || echo "    ✗ No .dylib files in lib/macOS"
      else
        ls -la "$location/lib/"*.dylib 2>/dev/null || echo "    ✗ No .dylib files in lib"
      fi
    fi
  else
    echo "✗ Not found: $location"
  fi
done

echo ""
echo "2. System-wide NDI search:"
echo "----------------------------------------"
echo "Searching in /Library:"
find /Library -maxdepth 2 -name "*NDI*" -type d 2>/dev/null || echo "No NDI directories found in /Library"

echo ""
echo "Searching in /Applications:"
find /Applications -maxdepth 2 -name "*NDI*" -type d 2>/dev/null || echo "No NDI directories found in /Applications"

echo ""
echo "Searching in /usr/local:"
find /usr/local -maxdepth 2 -name "*ndi*" -type f 2>/dev/null | grep -E "\.(dylib|a)$" || echo "No NDI libraries found in /usr/local"

echo ""
echo "3. Environment check:"
echo "----------------------------------------"
echo "NDI_SDK_DIR: ${NDI_SDK_DIR:-<not set>}"
echo "DYLD_LIBRARY_PATH: ${DYLD_LIBRARY_PATH:-<not set>}"

echo ""
echo "4. Build test:"
echo "----------------------------------------"
if [ -f "build.rs" ]; then
  echo "Testing build.rs with current environment..."
  cargo clean
  cargo build --verbose 2>&1 | grep -E "(NDI|ndi|Processing\.NDI)" | head -20
else
  echo "build.rs not found in current directory"
fi

echo ""
echo "=== End of NDI SDK Debug ==="