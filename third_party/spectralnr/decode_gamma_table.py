"""One-time offline decode of AetherSDR's Nr2GammaTables.inc.

Performs the base64 + zlib stages of the upstream payload, verifies the
result against the upstream-pinned SHA-256 of the unpacked table, and
emits a plain C byte-array header (nr2_gamma_table.inc) so the vendored
C++ needs no base64/zlib/Qt at runtime.

Usage: python decode_gamma_table.py <upstream Nr2GammaTables.inc> <out inc path>
"""
import base64
import hashlib
import math
import re
import struct
import sys
import zlib

GRID = 241
PLANE = GRID * GRID  # 58081
N = 2 * PLANE  # 116162 values
PACKED = 8 * N  # 929296 bytes
MASK64 = (1 << 64) - 1


def main() -> None:
    src, dst = sys.argv[1], sys.argv[2]
    text = open(src, encoding="utf-8").read()
    m = re.search(r'kNr2GammaTablesRawSha256\[\]\s*=\s*"([0-9a-f]{64})"', text)
    assert m, "expected SHA-256 constant not found"
    sha_expected = m.group(1)
    # Drop the SHA-256 line so its hex literal is not mistaken for a chunk.
    text = re.sub(r'kNr2GammaTablesRawSha256.*', "", text)
    chunks = re.findall(r'"([A-Za-z0-9+/=]{16,})"', text)
    assert chunks, "no base64 chunks found"
    blob = base64.b64decode("".join(chunks))
    print(f"base64 decoded: {len(blob)} bytes")

    # Qt qCompress format: 4-byte big-endian original size, then a zlib stream.
    original_size = int.from_bytes(blob[:4], "big")
    print(f"qCompress header: original size {original_size}")
    packed = zlib.decompress(blob[4:], 15)
    print(f"zlib: {len(packed)} bytes")
    assert len(packed) == PACKED, (len(packed), PACKED)

    # Integral-image (2D prefix-sum) decode, mirroring SpectralNR.cpp.
    bits = [0] * N
    values = [0.0] * N
    for index in range(N):
        residual = 0
        for byte in range(8):
            residual |= packed[byte * N + index] << (8 * byte)
        plane_index = index % PLANE
        row = plane_index // GRID
        col = plane_index % GRID
        left = bits[index - 1] if col > 0 else 0
        up = bits[index - GRID] if row > 0 else 0
        upper_left = bits[index - GRID - 1] if (row > 0 and col > 0) else 0
        b = (residual + left + up - upper_left) & MASK64
        bits[index] = b
        (v,) = struct.unpack("<d", b.to_bytes(8, "little"))
        if not math.isfinite(v) or v < 0:
            raise AssertionError(f"bad value {v!r} at index {index}")
        values[index] = v

    # Repack in index order, little-endian, and check the upstream hash.
    raw = b"".join(b.to_bytes(8, "little") for b in bits)
    sha_actual = hashlib.sha256(raw).hexdigest()
    print(f"sha expected: {sha_expected}")
    print(f"sha actual:   {sha_actual}")
    assert sha_actual == sha_expected, "SHA-256 mismatch"

    # Emit the byte-array header.
    lines = [
        "/* Pre-decoded payload for the NR2 gamma gain tables.",
        "",
        " * Source: aethersdr/AetherSDR src/core/Nr2GammaTables.inc",
        " * (SPDX-License-Identifier: GPL-2.0-or-later, exact GG/GGS values",
        " * from TAPR/OpenHPSDR-wdsp Source/calculus). The base64 + zlib",
        " * encoding stages were performed offline and the result verified",
        f" * against the upstream-pinned SHA-256 of the unpacked table:",
        f" * {sha_expected}.",
        " * Layout: 8 x kGammaTableValueCount bytes, plane-major",
        " * little-endian uint64 integral-image encoding, exactly as the",
        " * upstream decode in SpectralNR.cpp consumes it.",
        " * Regenerate with: python decode_gamma_table.py <upstream .inc> <this file>",
        " */",
        f"static const unsigned char kNr2GammaTablesPacked[{PACKED}] = {{",
    ]
    per_line = 32
    for i in range(0, PACKED, per_line):
        chunk = ", ".join(f"0x{b:02x}" for b in packed[i : i + per_line])
        lines.append((" " + chunk + ",") if i + per_line < PACKED else (" " + chunk))
    lines.append("};")
    with open(dst, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines) + "\n")
    print(f"wrote {dst}")

    # Pin values for the Rust unit test.
    idxs = [0, 1, PLANE // 2, PLANE // 2 + GRID // 2, N // 2, N - 1]
    for i in idxs:
        print(f"pin[{i}] = {values[i]!r}")
    print(f"min = {min(values)!r}")
    print(f"max = {max(values)!r}")


if __name__ == "__main__":
    main()
