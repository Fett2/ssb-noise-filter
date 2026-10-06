## *This entire project is vibe coded.*

# SSB Noise Filter

A real-time noise filter for ham radio SSB voice on Windows. It sits between
a Windows audio input device and a Windows audio output device: it listens
to the band, removes the white noise and static, and hands you clean voices
back. The moment your radio keys
up (PTT is polled from a rigctld server, e.g. WSJT-X's `rigctld-wsjtx.exe`),
the filter steps out of the way, so what you transmit is exactly what you hear.

## What you get

- **Two denoising engines** — RNNoise (a small neural network) and NR2
  (EMNR, the engine AetherSDR ships), switchable from a dropdown. Your choice
  is remembered between runs.
- **One slider, one trade** — "Noise reduction" blends the cleaned signal
  with the raw band-passed signal. 100% = the static is gone. 0% = the raw
  band, static and all. Everything in between is you choosing how much of
  each you want.
- **Transmitting is transparent** — while keyed the filter bypasses itself,
  and the TX monitor has the same latency as with no filter at all.
- **Volume to 300%** — with a meter showing what actually reaches your
  speakers, so you know when you're clipping.

## Requirements

- Windows, one audio input device (set to 48 kHz / 32-bit float in Sound settings)
  and any audio output
- Optional: a rigctld server for PTT,
  default `localhost:4532`). Without it the filter simply stays on.
- That's it — one `.exe`, no installer, no dependencies.

## Building

It's Rust. Toolchain details live in [AGENTS.md](AGENTS.md); the Windows exe
is:

```
cargo build --release --target x86_64-pc-windows-gnu
```

## License

GPL-3.0 — required by the vendored EMNR engine. See [LICENSE](LICENSE).

## Future plans

More ways to detect whether the radio is transmitting, alongside the current
rigctld polling — for example via OmniRIG, or by simply reading the radio's
COM port directly.
