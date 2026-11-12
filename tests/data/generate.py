#!/usr/bin/env python3
"""Regenerate the original, synthetic HEVC 4:4:4 test picture with FFmpeg/libx265.

The lossless frame has alternating chroma in adjacent pixels, so subsampling
cannot accidentally satisfy the decoder test. No vendor media is included.
Only regeneration needs FFmpeg; tests use the checked-in bitstream.
"""
from pathlib import Path
import subprocess
import tempfile


def main():
    width, height = 64, 32
    planes = [
        bytes(64 + (x + y) % 64 for y in range(height) for x in range(width)),
        bytes(64 + 64 * (x % 2) for y in range(height) for x in range(width)),
        bytes(80 + 64 * (y % 2) for y in range(height) for x in range(width)),
    ]
    with tempfile.TemporaryDirectory() as directory:
        raw = Path(directory) / 'chroma.yuv'
        raw.write_bytes(b''.join(planes))
        subprocess.run([
            'ffmpeg', '-hide_banner', '-loglevel', 'error', '-y',
            '-f', 'rawvideo', '-pixel_format', 'yuv444p', '-video_size', '64x32',
            '-framerate', '30', '-i', str(raw), '-frames:v', '1',
            '-c:v', 'libx265', '-preset', 'veryslow', '-x265-params',
            'lossless=1:repeat-headers=1:info=0:pools=none:frame-threads=1:wpp=0',
            '-f', 'hevc', str(Path(__file__).with_name('chroma-444.hevc')),
        ], check=True)


if __name__ == '__main__':
    main()
