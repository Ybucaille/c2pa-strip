"""Check that two PNG or WebP files carry the same image data, byte for byte.

Usage: python3 same_image_data.py ORIGINAL CLEANED

Compares the chunks that hold pixels (PNG IHDR/PLTE/tRNS/IDAT, WebP
VP8/VP8L/ALPH/ANMF) and ignores metadata chunks. Standard library only.
"""

import struct
import sys

PNG_IMAGE = {b"IHDR", b"PLTE", b"tRNS", b"IDAT"}
WEBP_IMAGE = {b"VP8 ", b"VP8L", b"ALPH", b"ANMF"}


def png_chunks(data):
    pos = 8
    while pos + 8 <= len(data):
        (length,) = struct.unpack(">I", data[pos : pos + 4])
        kind = data[pos + 4 : pos + 8]
        yield kind, data[pos + 8 : pos + 8 + length]
        pos += 12 + length


def webp_chunks(data):
    pos = 12
    while pos + 8 <= len(data):
        kind = data[pos : pos + 4]
        (length,) = struct.unpack("<I", data[pos + 4 : pos + 8])
        yield kind, data[pos + 8 : pos + 8 + length]
        pos += 8 + length + (length & 1)


def image_data(path):
    data = open(path, "rb").read()
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return [c for c in png_chunks(data) if c[0] in PNG_IMAGE]
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return [c for c in webp_chunks(data) if c[0] in WEBP_IMAGE]
    sys.exit(f"{path}: not a PNG or WebP file")


def main():
    original, cleaned = sys.argv[1:3]
    a, b = image_data(original), image_data(cleaned)
    if not a or a != b:
        sys.exit(f"image data differs between {original} and {cleaned}")
    size = sum(len(c) for _, c in a)
    kinds = ", ".join(sorted({k.decode().strip() for k, _ in a}))
    print(f"identical image data: {size} bytes in {kinds}")


if __name__ == "__main__":
    main()
