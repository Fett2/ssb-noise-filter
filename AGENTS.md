# AGENTS.md

## What it is
Real-time noise filter for ham radio SSB voice on Windows. It captures live audio from a standard Windows audio device (WASAPI endpoint), removes white noise, and plays the result to a standard Windows audio device. While the radio is keyed (PTT state polled from a rigctld server, e.g. WSJT-X's rigctld-wsjtx.exe) the denoiser is bypassed and the raw band-passed signal passes through.

## Stack
- Rust (rustup `stable`, 1.98 at scaffold time, x86_64-pc-windows-msvc), builds a single Windows `.exe`
- Release build: `cargo build --release` (binary in `target\release\`)
- Fast feedback: `cargo check`; lint: `cargo clippy`; tests: `cargo test`
- `cpal 0.18` for audio I/O (WASAPI backend), `egui`/`eframe 0.36` for the GUI — deliberately the `glow` (OpenGL) renderer instead of eframe's default `wgpu` (Vulkan): some Intel Vulkan drivers crash at startup on other people's machines
- Noise reduction: `nnnoiseless 0.5.2` (pure-Rust port of Xiph's RNNoise, BSD-3); with `default-features = false` its only deps are `easyfft` + `once_cell`
- Pre-filter: homegrown 2nd-order biquad bandpass (HP 300 Hz + LP 3 kHz, RBJ cookbook coefficients, direct form II transposed) in `src/biquad.rs`
- Output rate: homegrown streaming linear-interpolation resampler (`src/resampler.rs`), used on the render path when the output device's mix rate is not 48 kHz
- Rig PTT: homegrown minimal rigctld TCP client + background poller thread (`src/rigctl.rs`), std-only (no new deps)

## Decided: noise reduction approach
- RNNoise (recurrent neural net, 48 kHz, 480-sample = 10 ms frames) via `nnnoiseless`, low-level `DenoiseState` API
- Samples are f32 but must be in the i16 PCM range `[-32768, 32767]` (not `[-1, 1]`): scale cpal's f32 by 32768 before and after `process_frame`
- The first `process_frame` output has a fade-in artifact — discard it
- SSB voice is 300–3000 Hz, so the mic signal is bandpassed *before* the denoiser (avoids out-of-band "underwater" artifacts)
- The denoised output is blended with the band-passed original, `out = w·denoised + (1−w)·bandpassed`, where w is the GUI "Noise reduction" slider (default 80%). RNNoise's per-band gains attack instantly (only release is smoothed), so on flickering weak SSB signals the gain snaps cause pops and untrusted speech gets crushed; blending in the raw signal damps the snaps, restores timbre, and keeps a floor under weak signals. 100% = plain RNNoise
- The *input* must be 48 kHz / 32-bit float (WASAPI shared mode + RNNoise); the *output* may run at any mix rate — the render callback linearly resamples 48 kHz to the device rate (safe because the signal is band-limited to 3 kHz)
- Rejected: EMNR (Thetis NR2 — needs C FFI + FFTW + GPL), WebRTC NS (less proven on SSB), spectral subtraction (musical noise)

## Decided: rig PTT via rigctld
- A detached poller thread keeps a TCP connection to a rigctld server (rigctld "default" protocol: no banner, no ACK; the short command `t\n` answers a bare PTT state `0` RX / `1` TX / `2` TX mic / `3` TX data, or `RPRT <n>` when the rig errors) — polled every 100 ms, reconnecting every 1 s while a target is set; a dead link (timeout/close/malformed line) drops the connection and sets state to `DISCONNECTED` (255)
- The PTT state is published as `Arc<AtomicU8>`; the capture callback forces the NR blend to 0 while the rig is keyed (1–3), so our own transmitted voice is not gain-ridged by the RNN. `process_frame` keeps running while keyed so the RNN stays warm (no fade-in when the key drops); `DISCONNECTED` falls back to the GUI slider value
- A `RPRT` answer (rig error) keeps the last PTT state; the GUI has editable host/port (default `localhost:4532`) with Connect/Disconnect, a colored RX/TX indicator, and the poller's status line
- v1 is poll-only (state in); keying the rig from the GUI (`T 0/1`) is a likely follow-up

## Real-time audio gotchas
- DSP runs inside cpal's audio callbacks (real-time threads): no allocations, no locks/mutexes, no file/network I/O, no `println!` — any of these cause glitches
- Device enumeration/selection happens on the GUI thread (startup, or on Refresh), never inside the callback
- Capture and render callbacks run on separate threads and communicate only via the lock-free SPSC ring (`src/spsc.rs`) and `Arc<AtomicU32>` peak meters
- The GUI thread talks to the capture callback only through atomics: `Arc<AtomicU32>` peak meters in, the NR-amount `f32`-bits and the `Arc<AtomicU8>` rig PTT state out; the callback reads the NR amount and PTT with a `Relaxed` load once per 480-sample frame (no mutexes in the callback)
- All DSP state (DenoiseState, biquads, frame accumulators, ring, resampler buffers) is allocated on the GUI thread at Start and moved into the stream closures

## Conventions
- Conventional commits (`feat:`, `fix:`, `refactor:`, `chore:`)
- Run `cargo test` before committing
- CI on PR once a workflow is added

## Housekeeping
- Keep this file in sync with the code. It replaced the original placeholder now that the crate is scaffolded; verify claims against the code when touching them.
