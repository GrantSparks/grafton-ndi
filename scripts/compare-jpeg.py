#!/usr/bin/env python3
"""Compare resource-harness dumps with Pillow and NumPy (independent decoder).

Usage: python scripts/compare-jpeg.py SCALAR_DIR SIMD_DIR DIRECT_DIR
Requires Pillow and NumPy; emits raw CSV and checks a decoded SIMD tolerance.
The tolerance compares decoded images, not encoded byte identity.
"""

import csv
from pathlib import Path
import sys

import numpy as np
from PIL import Image


def compare(directories):
    writer = csv.writer(sys.stdout)
    writer.writerow(["variant", "image", "bytes", "psnr_source_rgb_db", "scalar_mae", "scalar_max"])
    baseline = Path(directories[0])
    for directory in map(Path, directories):
        for path in sorted(directory.glob("*.image")):
            name = path.stem
            _, content, size, output = name.split("-", 3)
            width, height = map(int, size.split("x"))
            shift = 4 if output.endswith("quarter") else 1
            with Image.open(path) as image:
                assert image.size == (width // shift, height // shift)
                decoded = np.asarray(image.convert("RGB"), dtype=np.float64)
            with Image.open(baseline / path.name) as image:
                scalar = np.asarray(image.convert("RGB"), dtype=np.float64)
            source = np.frombuffer((directory / f"{content}-{size}.rgba").read_bytes(), dtype=np.uint8)
            source = source.reshape(height, width, 4)[:, :, :3].astype(np.float64)
            source = source.reshape(height // shift, shift, width // shift, shift, 3).mean(axis=(1, 3))
            mse = np.square(decoded - source).mean()
            delta = np.abs(decoded - scalar)
            writer.writerow([directory.name, name, path.stat().st_size,
                             f"{10 * np.log10(255 ** 2 / mse):.6f}",
                             f"{delta.mean():.6f}", int(delta.max())])
            # Includes YUV conversion and downscale fallbacks. PSNR is against
            # the pre-YUV synthetic RGB, not the frame's quantized code values.
            assert delta.mean() <= 1.0 and delta.max() <= 32, name


if __name__ == "__main__":
    compare(sys.argv[1:])
