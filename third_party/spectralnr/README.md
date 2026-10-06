# SpectralNR (vendored)

The "NR2" noise-reduction engine: a C++20 port of the original EMNR
(`emnr.c`, Warren Pratt NR0V, TAPR OpenHPSDR-WDSP) as shipped in AetherSDR.

## Provenance

- `SpectralNR.h` / `SpectralNR.cpp` — the AetherSDR port, verbatim except
  for the de-Qt'ing below (full license text in `SpectralNR.h`).
- `decode_gamma_table.py` — one-time offline script written for this repo:
  performs the base64 + zlib stages of AetherSDR's gamma-table payload and
  verifies the result against the upstream-pinned SHA-256.
- `nr2_gamma_table.inc` — the pre-decoded table (116,162 values,
  2 × 241 × 241) produced by that script, verified against the
  upstream-pinned SHA-256 of the unpacked table (in the file's own
  header); integrity is re-pinned by a Rust test
  (`nr2::tests::gamma_table_decodes_and_matches_upstream`).
- `nr2_bridge.cpp` — small `extern "C"` bridge (this project's own code)
  because MSVC's C++ name mangling differs from Rust's `extern "C++"`.

## License

The port is GPL: the WDSP-derived portions are GPL-2.0-or-later and the
AetherSDR adaptation is GPL-3.0-or-later (see `SpectralNR.h`). This
incompatibility-free combination is why the whole project is GPL-3.0 —
see `LICENSE` at the repository root.

## De-Qt'ing

The port's Qt usage was limited to the one-time gamma-table startup decode
(base64/zlib/SHA-256) and two AetherSDR includes. That decode was
replaced by the pre-decoded `nr2_gamma_table.inc` plus standard-library
checks, so the runtime needs no Qt, no base64/zlib, and no FFTW (the
port's FFTW path is behind `#ifdef HAVE_FFTW3`, which is undefined here —
the built-in radix-2 FFT is used). One debug-only `Q_ASSERT` on the OLA
read was replaced with a comment: during the ~2-hop OLA startup ramp the
ring legitimately holds zeros (expected startup silence, matching
upstream release builds).

## How it's built

`build.rs` compiles `SpectralNR.cpp` + `nr2_bridge.cpp` with the `cc`
crate (C++20) into a static library; `src/nr2.rs` wraps it. Operating
point: FFT 512 / overlap 2 at 48 kHz — AetherSDR's 24 kHz/256 geometry at
double rate. The C++ stdlib is static-linked (`cpp_link_stdlib_static`),
so the Windows exe carries no `libstdc++-6.dll` dependency.
