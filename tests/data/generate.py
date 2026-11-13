#!/usr/bin/env python3
"""Regenerate the original, synthetic NDI|HX test pictures with FFmpeg.

`chroma-444.hevc` (libx265): a lossless HEVC 4:4:4 frame with alternating
chroma in adjacent pixels, so subsampling cannot accidentally satisfy the
decoder test.

`quadrants-420.h264` (libx264): one H.264 High profile IDR frame, 4:2:0, of
four flat quadrants that each have their own luma and chroma, at QP 1. x264
codes QP 0 as lossless, which needs the High 4:4:4 Predictive profile, so QP 1
is the finest High allows. The picture is 64x64 because the Media Foundation
H.264 decoder documents 48x48 as its smallest. Without B-frames the SPS lets a
decoder output the frame at once, and the frame holds only the SPS, the PPS
and the slice: x264's informational SEI is removed.

No vendor media is included. Only regeneration needs FFmpeg; tests use the
checked-in bitstreams.
"""
from pathlib import Path
import subprocess
import tempfile

# The H.264 picture's quadrants as (luma, Cb, Cr), in the order top left, top
# right, bottom left, bottom right. tests/advanced_runtime.rs states the same.
QUADRANTS = ((48, 96, 176), (96, 176, 96), (160, 64, 128), (208, 160, 64))


def ffmpeg(raw, pixel_format, size, codec, output):
    subprocess.run([
        'ffmpeg', '-hide_banner', '-loglevel', 'error', '-y',
        '-f', 'rawvideo', '-pixel_format', pixel_format, '-video_size', size,
        '-framerate', '30', '-i', str(raw), '-frames:v', '1',
        *codec, str(Path(__file__).with_name(output)),
    ], check=True)


def hevc(directory):
    width, height = 64, 32
    planes = [
        bytes(64 + (x + y) % 64 for y in range(height) for x in range(width)),
        bytes(64 + 64 * (x % 2) for y in range(height) for x in range(width)),
        bytes(80 + 64 * (y % 2) for y in range(height) for x in range(width)),
    ]
    raw = Path(directory) / 'chroma.yuv'
    raw.write_bytes(b''.join(planes))
    ffmpeg(raw, 'yuv444p', f'{width}x{height}', [
        '-c:v', 'libx265', '-preset', 'veryslow', '-x265-params',
        'lossless=1:repeat-headers=1:info=0:pools=none:frame-threads=1:wpp=0',
        '-f', 'hevc',
    ], 'chroma-444.hevc')


def h264(directory):
    width, height = 64, 64

    def plane(component, subsampling):
        columns, rows = width // subsampling, height // subsampling
        return bytes(QUADRANTS[2 * (y >= rows // 2) + (x >= columns // 2)][component]
                     for y in range(rows) for x in range(columns))

    raw = Path(directory) / 'quadrants.yuv'
    raw.write_bytes(plane(0, 1) + plane(1, 2) + plane(2, 2))
    ffmpeg(raw, 'yuv420p', f'{width}x{height}', [
        '-c:v', 'libx264', '-profile:v', 'high', '-preset', 'veryslow', '-qp', '1',
        '-x264-params', 'threads=1:aud=0:bframes=0', '-bsf:v', 'filter_units=remove_types=6',
        '-f', 'h264',
    ], 'quadrants-420.h264')


def main():
    with tempfile.TemporaryDirectory() as directory:
        hevc(directory)
        h264(directory)


if __name__ == '__main__':
    main()
