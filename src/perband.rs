//! Per-band adaptive spectral blend: the "wet" stage.
//!
//! RNNoise's per-band gains attack instantly and only the release is
//! smoothed, so on a flickering weak SSB signal the gain snaps are heard as
//! pops and untrusted speech gets crushed. This stage blends the RNN's
//! output with the band-passed raw signal *per 50 Hz band* (RNNoise's own
//! bands are far coarser): a noise-floor tracker per band opens a gate when
//! the band's power rises above the floor (activity), and the blend weight
//! crossfades from the denoised signal (quiet, at the floor -> full NR) to
//! the raw signal (+6 dB above the floor -> no NR). The GUI's "Noise
//! reduction" slider scales the wet fraction overall, exactly as before.
//!
//! The stage owns the RNNoise `DenoiseState` and runs an overlap-add FFT
//! on every 10 ms frame: a 960-sample periodic Hann window (50% overlap) is
//! windowed and FFT'ed for both the raw and the denoised signal, the
//! spectra are blended per band, IFFT'ed and overlapped by 50%. The
//! completed output is the *previous* input frame (+10 ms latency) at unit
//! gain when no band is gated.
//!
//! The first two frames are discarded while the RNN's fade-in settles.
//! While the rig is keyed the wet fraction is 0 (pure band-pass through)
//! and the floor trackers freeze, so our own transmitted voice is not
//! treated as noise; the RNN keeps being fed and stays warm.
//!
//! All state is pre-allocated on the GUI thread; `process` allocates
//! nothing and takes no locks (the FFT planner is thread-local), so it is
//! safe on the capture thread.

const TAU: f64 = std::f64::consts::TAU;

use easyfft::const_size::{FftMut, IfftMut};
use easyfft::num_complex::Complex;
use nnnoiseless::DenoiseState;

/// RNNoise frame: 480 samples at 48 kHz (10 ms).
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// OLA analysis/synthesis window: two frames (50% overlap).
const WIN: usize = FRAME * 2;
/// easyfft's IFFT is not 1/N-normalized (`ifft(fft(x)) == N*x`), so each
/// window half is scaled by 1/N in the OLA sum.
const INV_WIN: f32 = 1.0 / WIN as f32;
/// RNNoise expects/produces f32 samples in the i16 PCM range, not [-1, 1].
/// The scaling is linear, so it cancels in the OLA and `out` is already in
/// [-1, 1] after the 1/N scale.
const I16_SCALE: f32 = 32_768.0;
const INV_I16_SCALE: f32 = 1.0 / I16_SCALE;
/// Guard for the `power / floor` ratio: the FFT rounding residual on power
/// is ~1e-12, so this is well above it, and it bounds the activity at 1.
const EPS: f32 = 1e-3;
/// Gate hysteresis: opens at 2x the floor (~3 dB), closes below 1.3x
/// (~1.2 dB), so a signal hovering near the floor does not chatter.
const GATE_OPEN: f32 = 2.0;
const GATE_CLOSE: f32 = 1.3;
/// Floor tracker: slow attack (a signal that stops is not noise), fast
/// decay (noise that disappears is gone again).
const ATTACK: f32 = 0.01;
const DECAY: f32 = 0.3;
/// Activity scale: 0 at the floor (full NR), 1 at +6 dB above it (full raw).
const CROSSFADE_DB: f32 = 6.0;

/// SSB voice band 300-3000 Hz. At 48 kHz with a 960-pt FFT the bins are 50
/// Hz wide: the in-band bins 6..=59 are grouped into 6 bands of 9 bins
/// (300-750, 750-1200, 1200-1650, 1650-2100, 2100-2550, 2550-3000 Hz);
/// everything else (DC-300 and 3000-Nyquist) is one out-of-band bucket that
/// is always noise (full wet, no activity gating).
const FIRST_BAND_BIN: usize = 6;
const LAST_BAND_BIN: usize = 59;
const BINS_PER_BAND: usize = 9;
const NBANDS: usize = (LAST_BAND_BIN - FIRST_BAND_BIN + 1) / BINS_PER_BAND;
/// Index of the out-of-band bucket in the floor/gate tables.
const OUT: usize = NBANDS;
/// In-band bands plus the out-of-band bucket.
const NB_FLOORS: usize = NBANDS + 1;
/// Out-of-band bins in the positive half of the spectrum (0..=480).
const OUT_HALF_BINS: usize = (FRAME + 1) - (LAST_BAND_BIN - FIRST_BAND_BIN + 1);

/// Per-band wet stage: owns the RNNoise state and the OLA FFT machinery.
/// Construct on the GUI thread and move into the capture callback; the RNN
/// and the ~35 KB of buffers are allocated once and `process` never
/// allocates.
pub struct PerBand {
    /// RNNoise in i16-land, unboxed (the RNN state is large).
    den: DenoiseState<'static>,
    /// `raw * I16_SCALE`, the RNN's input format.
    scratch_in: [f32; FRAME],
    /// RNN output rescaled to [-1, 1].
    den_frame: [f32; FRAME],
    /// The previous raw/denoised frames: the first half of the 960 window.
    prev_raw: [f32; FRAME],
    prev_den: [f32; FRAME],
    /// Windowed spectra; `fft_raw` doubles as the blended spectrum after
    /// the raw/denoise mix. `tail` carries the second half of the current
    /// window into the next frame's OLA sum; `out` is the completed frame.
    fft_raw: [Complex<f32>; WIN],
    fft_den: [Complex<f32>; WIN],
    tail: [f32; FRAME],
    out: [f32; FRAME],
    /// Periodic Hann window and per-bin band index, built once.
    hann: [f32; WIN],
    band_of: [u8; WIN],
    /// Noise floor per band (FFT power, `floor[b] + ATTACK*(p-floor)` /
    /// `floor[b] + DECAY*(p-floor)` updates, always >= EPS once seeded).
    /// Seeded on the first non-keyed frame; frozen while gated or keyed.
    floor: [f32; NB_FLOORS],
    /// While open, the band is fully raw and its floor is frozen.
    gate_open: [bool; NB_FLOORS],
    /// Frames processed; the first two are warmup (RNN fade-in).
    frames: u32,
}

impl PerBand {
    /// Create the stage on the GUI thread (the RNN allocates) and
    /// precompute the window and band table.
    pub fn new() -> Self {
        // Periodic Hann: w[i] + w[i + WIN/2] == 1, which is what makes the
        // 50% OLA unit-gain.
        let mut hann = [0.0f32; WIN];
        for (i, w) in hann.iter_mut().enumerate() {
            *w = 0.5 * (1.0 - (TAU * i as f64 / WIN as f64).cos() as f32);
        }
        let mut band_of = [0u8; WIN];
        for (k, slot) in band_of.iter_mut().enumerate() {
            // The spectrum of a real signal is conjugate symmetric: fold
            // the negative half back onto the positive one.
            let m = k.min(WIN - k);
            *slot = if (FIRST_BAND_BIN..=LAST_BAND_BIN).contains(&m) {
                ((m - FIRST_BAND_BIN) / BINS_PER_BAND) as u8
            } else {
                OUT as u8
            };
        }
        Self {
            den: *DenoiseState::new(),
            scratch_in: [0.0; FRAME],
            den_frame: [0.0; FRAME],
            prev_raw: [0.0; FRAME],
            prev_den: [0.0; FRAME],
            fft_raw: [Complex::new(0.0, 0.0); WIN],
            fft_den: [Complex::new(0.0, 0.0); WIN],
            tail: [0.0; FRAME],
            out: [0.0; FRAME],
            hann,
            band_of,
            floor: [0.0; NB_FLOORS],
            gate_open: [false; NB_FLOORS],
            frames: 0,
        }
    }

    /// Run one 10 ms frame through the stage.
    ///
    /// `raw` is the band-passed input in [-1, 1]; `wet` is the overall
    /// noise-reduction fraction (pass 0.0 while the rig is keyed); `keyed`
    /// additionally freezes the noise-floor trackers, since the transmitted
    /// voice is not noise. Returns the completed frame — the *previous*
    /// input frame, delayed by 10 ms, unit gain — or `None` during the
    /// 2-frame warmup while the RNN's fade-in settles.
    #[inline]
    pub fn process(&mut self, raw: &[f32; FRAME], wet: f32, keyed: bool) -> Option<&[f32; FRAME]> {
        // Feed the RNN in i16-land, bring its output back to [-1, 1]. The
        // scale cancels in the OLA below, so no rescaling is needed at the
        // end.
        for (x, &s) in self.scratch_in.iter_mut().zip(raw) {
            *x = s * I16_SCALE;
        }
        let _ = self.den.process_frame(&mut self.den_frame, &self.scratch_in);
        for x in &mut self.den_frame {
            *x *= INV_I16_SCALE;
        }

        // Build the 960-sample analysis windows: previous frame (rising
        // side of the Hann) followed by the current one (falling side).
        for (i, &s) in raw.iter().enumerate() {
            self.fft_raw[i] = Complex::new(self.hann[i] * self.prev_raw[i], 0.0);
            self.fft_raw[FRAME + i] = Complex::new(self.hann[FRAME + i] * s, 0.0);
            self.fft_den[i] = Complex::new(self.hann[i] * self.prev_den[i], 0.0);
            self.fft_den[FRAME + i] = Complex::new(self.hann[FRAME + i] * self.den_frame[i], 0.0);
        }
        self.fft_raw.fft_mut();
        self.fft_den.fft_mut();

        // Per-band power from the RAW spectrum (positive half only; the
        // negative half mirrors it), then the floor/gate state machine.
        let mut power = [0.0f32; NB_FLOORS];
        for k in 0..=FRAME {
            let c = self.fft_raw[k];
            power[self.band_of[k] as usize] += c.re * c.re + c.im * c.im;
        }
        for b in power.iter_mut().take(NBANDS) {
            *b /= BINS_PER_BAND as f32;
        }
        power[OUT] /= OUT_HALF_BINS as f32;
        if !keyed {
            for (p, (floor, gate)) in power.iter().copied().zip(self.floor.iter_mut().zip(self.gate_open.iter_mut()))
            {
                if *floor == 0.0 {
                    // First sight of this band: seed the floor.
                    *floor = p.max(EPS);
                } else if *gate {
                    if p < GATE_CLOSE * *floor {
                        *gate = false;
                    }
                } else if p > GATE_OPEN * *floor {
                    *gate = true;
                } else if p > *floor {
                    *floor = (*floor + ATTACK * (p - *floor)).max(EPS);
                } else {
                    *floor = (*floor + DECAY * (p - *floor)).max(EPS);
                }
            }
        }

        // Activity -> per-band wet weight: quiet (at the floor) takes the
        // full NR fraction, +6 dB above it is raw. `.max(EPS)` keeps the
        // ratio finite while a band is still unseeded (keyed from startup).
        let mut w = [wet; NB_FLOORS];
        for b in 0..NBANDS {
            let a = (10.0 * (power[b] / self.floor[b].max(EPS)).log10() / CROSSFADE_DB).clamp(0.0, 1.0);
            w[b] = wet * (1.0 - a);
        }
        // The out-of-band bucket is always noise and keeps the full
        // fraction (its `w[OUT]` was initialised to `wet` above).
        for k in 0..WIN {
            let w = w[self.band_of[k] as usize];
            let d = self.fft_den[k];
            let r = self.fft_raw[k];
            self.fft_raw[k] = Complex::new(w * d.re + (1.0 - w) * r.re, w * d.im + (1.0 - w) * r.im);
        }
        self.fft_raw.ifft_mut();

        // Overlap-add the two window halves. With every weight at 1 the
        // two halves of the periodic Hann sum to exactly the previous
        // frame (unit gain); gating a band to 0 splices in the raw signal
        // there.
        for i in 0..FRAME {
            self.out[i] = self.fft_raw[i].re * INV_WIN + self.tail[i];
            self.tail[i] = self.fft_raw[FRAME + i].re * INV_WIN;
        }

        self.prev_raw.copy_from_slice(raw);
        self.prev_den.copy_from_slice(&self.den_frame);
        self.frames += 1;
        (self.frames > 2).then_some(&self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_quiet(pb: &mut PerBand, frames: usize) {
        let quiet = [0.0f32; FRAME];
        for _ in 0..frames {
            pb.process(&quiet, 1.0, false);
        }
    }

    /// Advance `n` frames (each a continuous 1 kHz sine, amplitude 0.5) and
    /// return the peak amplitude of the last completed output frame.
    fn feed_tone(pb: &mut PerBand, n: usize, wet: f32, keyed: bool) -> f32 {
        let omega = TAU * 1000.0 / 48_000.0;
        let mut phase = 0.0f64;
        let mut tone = [0.0f32; FRAME];
        let mut peak = 0.0f32;
        for _ in 0..n {
            for x in tone.iter_mut() {
                phase += omega;
                *x = 0.5 * phase.sin() as f32;
            }
            if let Some(out) = pb.process(&tone, wet, keyed) {
                peak = out.iter().cloned().fold(0.0f32, |m, x| m.max(x.abs()));
            }
        }
        peak
    }

    #[test]
    fn warmup_discards_the_first_two_frames() {
        let mut pb = PerBand::new();
        assert!(pb.process(&[0.1; FRAME], 0.0, false).is_none());
        assert!(pb.process(&[0.2; FRAME], 0.0, false).is_none());
        let out = pb.process(&[0.3; FRAME], 0.0, false).expect("warmup ends on the third frame");
        for &x in out {
            assert!((x - 0.2).abs() < 1e-4, "OLA must reconstruct the previous frame, got {x}");
        }
    }

    #[test]
    fn activity_opens_the_gate_and_passes_raw() {
        let mut pb = PerBand::new();
        feed_quiet(&mut pb, 24);
        // 1 kHz sits in band 1 (bins 15..=23).
        assert!(!pb.gate_open[1], "gate should be closed over a settled quiet floor");
        let peak = feed_tone(&mut pb, 16, 1.0, false);
        assert!(pb.gate_open[1], "a tone well above the floor must open its band's gate");
        assert!((0.40..=0.55).contains(&peak), "active band should pass the raw tone, peak {peak}");
    }

    #[test]
    fn keyed_freezes_the_floor_and_bypasses_to_raw() {
        let mut pb = PerBand::new();
        feed_quiet(&mut pb, 24);
        let floor_at_rx = pb.floor[1];
        let peak = feed_tone(&mut pb, 8, 0.0, true);
        assert!((pb.floor[1] - floor_at_rx).abs() < 1e-6, "the floor must not track our own TX voice");
        assert!((0.45..=0.55).contains(&peak), "keyed output is the raw band-pass signal, peak {peak}");
    }

    #[test]
    fn gate_closes_when_the_band_quiets_down() {
        let mut pb = PerBand::new();
        feed_quiet(&mut pb, 24);
        feed_tone(&mut pb, 8, 1.0, false);
        assert!(pb.gate_open[1]);
        feed_quiet(&mut pb, 32);
        assert!(!pb.gate_open[1], "the gate must close once the tone stops");
        assert!((pb.floor[1] - EPS).abs() < 1e-6, "the floor decays back to the guard level");
    }
}
