//! Compiles the vendored SpectralNR (NR2/EMNR) engine: a C++20 port of the
//! original EMNR (emnr.c, Warren Pratt NR0V, TAPR OpenHPSDR-WDSP) as shipped
//! in AetherSDR, de-Qt'ed and wrapped in an `extern "C"` bridge
//! (third_party/spectralnr/README.md documents provenance and license).
//!
//! No FFTW: the port's FFTW paths are behind `#ifdef HAVE_FFTW3`, which we
//! deliberately do not define; the built-in radix-2 FFT is used.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=third_party/spectralnr/SpectralNR.h");
    println!("cargo:rerun-if-changed=third_party/spectralnr/SpectralNR.cpp");
    println!("cargo:rerun-if-changed=third_party/spectralnr/nr2_bridge.cpp");
    println!("cargo:rerun-if-changed=third_party/spectralnr/nr2_gamma_table.inc");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++20")
        .include("third_party/spectralnr")
        .file("third_party/spectralnr/SpectralNR.cpp")
        .file("third_party/spectralnr/nr2_bridge.cpp");

    if cfg!(target_env = "msvc") {
        // The sources are UTF-8 (em dashes, Greek letters in comments).
        build.flag("/utf-8");
        // cc does not inherit cargo's opt-level; match it explicitly.
        build.flag(if cfg!(debug_assertions) { "/Od" } else { "/O2" });
    } else {
        build.flag(if cfg!(debug_assertions) { "-g" } else { "-O2" });
        // Static-link the C++ stdlib. cc's default emits
        // `rustc-link-lib=stdc++` (dylib kind), which rustc wraps in
        // `-Wl,-Bdynamic` — that per-library flag overrides any global
        // `-static`, so the exe would import libstdc++-6.dll, which is
        // absent on a stock Windows machine. The static-kind emit makes the
        // app a self-contained single .exe.
        build.cpp_link_stdlib_static(true);
    }

    build.compile("spectralnr");
}
