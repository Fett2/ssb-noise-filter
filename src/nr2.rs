//! Rust wrapper over the vendored SpectralNR engine (NR2 / EMNR).
//!
//! The engine is a C++20 port of the original EMNR (emnr.c, Warren Pratt
//! NR0V, TAPR OpenHPSDR-WDSP) as shipped in AetherSDR; see
//! `third_party/spectralnr/` for provenance, license, and the `extern "C"`
//! bridge it is called through.
//!
//! The instance is created on the GUI thread at Start and moved into the
//! capture callback; all parameter setters are internally `std::atomic` in
//! the C++ class, so they may be called from the GUI thread at any time.
//!
//! The parameter setters are the engine's full API surface (exposed for
//! future per-parameter GUI control) and are not all called yet.
#![allow(dead_code)]

use std::os::raw::{c_int, c_void};

/// Engine selection values for the `filter` atomic (GUI dropdown).
pub const ENGINE_RNNOISE: u8 = 0;
pub const ENGINE_NR2: u8 = 1;

/// NR2 geometry: 512-point FFT at 48 kHz with 50% overlap.
const FFT_SIZE: usize = 512;
const OVERLAP: usize = 2;
/// Hop size in samples. The engine's overlap-add must be driven at exactly
/// this cadence (one call per hop of input), or the output reader laps the
/// writer and the OLA output corrupts. 256 samples = 5.33 ms at 48 kHz.
pub const HOP: usize = FFT_SIZE / OVERLAP;

/// The engine's built-in algorithmic delay, in samples. Its overlap-add
/// writer starts one FFT frame ahead of the reader (`m_outWritePos =
/// fftSize` at construction and re-armed by every reset), so the output at
/// stream position p is the processed input at p - DELAY. Anything blended
/// with the engine's output (e.g. the raw feed) must be delayed by exactly
/// this much, or the two components comb against each other.
pub const DELAY: usize = FFT_SIZE;

extern "C" {
    fn nr2_create(
        fft_size: c_int,
        sample_rate: c_int,
        overlap: c_int,
        use_legacy_gain_methods: c_int,
    ) -> *mut c_void;
    fn nr2_destroy(handle: *mut c_void);
    fn nr2_process(handle: *mut c_void, input: *const f32, output: *mut f32, num_samples: c_int);
    fn nr2_reset(handle: *mut c_void);
    fn nr2_reset_transient(handle: *mut c_void);
    fn nr2_set_gain_max(handle: *mut c_void, v: f32);
    fn nr2_set_gain_floor(handle: *mut c_void, v: f32);
    fn nr2_set_qspp(handle: *mut c_void, v: f32);
    fn nr2_set_gain_smooth(handle: *mut c_void, v: f32);
    fn nr2_set_gain_method(handle: *mut c_void, m: c_int);
    fn nr2_set_npe_method(handle: *mut c_void, m: c_int);
    fn nr2_set_ae_filter(handle: *mut c_void, on: c_int);
    fn nr2_set_post2_run(handle: *mut c_void, on: c_int);
    fn nr2_set_post2_factor(handle: *mut c_void, v: f32);
    fn nr2_set_post2_nlevel(handle: *mut c_void, v: f32);
    fn nr2_set_post2_taper_hz(handle: *mut c_void, hz: f32);
    fn nr2_set_post2_decay_seconds(handle: *mut c_void, seconds: f32);
    fn nr2_gamma_table_valid() -> c_int;
    fn nr2_gamma_table_value(index: c_int) -> f64;
    fn nr2_transient_reset_count(handle: *const c_void) -> u64;
    fn nr2_noise_estimate_reset_count(handle: *const c_void) -> u64;
}

/// One NR2 engine instance (opaque C++ object behind an `extern "C"` bridge).
pub struct Nr2 {
    handle: *mut c_void,
}

// SAFETY: the instance is created on the GUI thread, moved into the
// capture callback closure, and then only touched from that one callback
// thread; the parameter setters are atomic in the C++ class.
unsafe impl Send for Nr2 {}

impl Nr2 {
    /// Production geometry: 512-point FFT at 48 kHz with 50% overlap —
    /// identical spectral/temporal geometry to AetherSDR's 24 kHz / 256
    /// operating point (93.75 Hz bins, 187.5 hops/s), just at double rate.
    pub fn new() -> Self {
        let handle = unsafe {
            nr2_create(FFT_SIZE as c_int, 48_000, OVERLAP as c_int, 0)
        };
        assert!(!handle.is_null(), "NR2 engine creation failed");
        Nr2 { handle }
    }

    /// Feed mono f32 samples; `output` receives the same number of
    /// noise-reduced samples (the engine keeps its own OLA alignment).
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        assert_eq!(input.len(), output.len(), "NR2 in/out length mismatch");
        assert!(!input.is_empty());
        unsafe {
            nr2_process(
                self.handle,
                input.as_ptr(),
                output.as_mut_ptr(),
                input.len() as c_int,
            );
        }
    }

    /// Reset all state (engine toggle, source change). Re-arms the
    /// ~1 s dry→wet startup ramp.
    pub fn reset(&mut self) {
        unsafe { nr2_reset(self.handle) };
    }

    /// Flush only transient state (overlap-add rings, gain masks, ramp)
    /// while keeping the converged noise estimate — for the TX→RX PTT
    /// edge, per AetherSDR's own pattern.
    pub fn reset_transient(&mut self) {
        unsafe { nr2_reset_transient(self.handle) };
    }

    // Parameter setters (thread-safe in the C++ class).

    pub fn set_gain_max(&self, v: f32) {
        unsafe { nr2_set_gain_max(self.handle, v) };
    }

    pub fn set_gain_floor(&self, v: f32) {
        unsafe { nr2_set_gain_floor(self.handle, v) };
    }

    pub fn set_qspp(&self, v: f32) {
        unsafe { nr2_set_qspp(self.handle, v) };
    }

    pub fn set_gain_smooth(&self, v: f32) {
        unsafe { nr2_set_gain_smooth(self.handle, v) };
    }

    /// 0 = Linear, 1 = Log, 2 = Gamma (default, MMSE-LSA), 3 = Trained.
    pub fn set_gain_method(&self, m: i32) {
        unsafe { nr2_set_gain_method(self.handle, m) };
    }

    /// 0 = OSMS (default), 1 = MMSE, 2 = NSTAT.
    pub fn set_npe_method(&self, m: i32) {
        unsafe { nr2_set_npe_method(self.handle, m) };
    }

    pub fn set_ae_filter(&self, on: bool) {
        unsafe { nr2_set_ae_filter(self.handle, on as c_int) };
    }

    pub fn set_post2_run(&self, on: bool) {
        unsafe { nr2_set_post2_run(self.handle, on as c_int) };
    }

    pub fn set_post2_factor(&self, v: f32) {
        unsafe { nr2_set_post2_factor(self.handle, v) };
    }

    pub fn set_post2_nlevel(&self, v: f32) {
        unsafe { nr2_set_post2_nlevel(self.handle, v) };
    }

    pub fn set_post2_taper_hz(&self, hz: f32) {
        unsafe { nr2_set_post2_taper_hz(self.handle, hz) };
    }

    pub fn set_post2_decay_seconds(&self, seconds: f32) {
        unsafe { nr2_set_post2_decay_seconds(self.handle, seconds) };
    }

    pub fn transient_reset_count(&self) -> u64 {
        unsafe { nr2_transient_reset_count(self.handle as *const c_void) }
    }

    pub fn noise_estimate_reset_count(&self) -> u64 {
        unsafe { nr2_noise_estimate_reset_count(self.handle as *const c_void) }
    }
}

impl Drop for Nr2 {
    fn drop(&mut self) {
        unsafe { nr2_destroy(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values pinned by the offline decode (decode_gamma_table.py), which
    /// verified the payload against the upstream SHA-256
    /// de43a4b9c7db4445997e6452852980d58d2558d635ab3c63e6423a347ba326c3.
    const PINNED: [(usize, f64); 6] = [
        (0, 0.725654181154077),
        (1, 0.7050388220982234),
        (29040, 0.5296932056844755),
        (29160, 0.9954256086603032),
        (58081, 0.8000149083353535),
        (116161, 1.0),
    ];
    const TABLE_LEN: i32 = 116_162; // 2 * 241 * 241

    /// Deterministic LCG noise in [-0.5, 0.5]; `hops` of 256 samples —
    /// the engine must be fed at the hop cadence.
    fn feed_noise(engine: &mut Nr2, hops: usize) {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut inbuf = vec![0f32; HOP];
        let mut outbuf = vec![0f32; HOP];
        for _ in 0..hops {
            for s in inbuf.iter_mut() {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                *s = ((state >> 40) as f32 / 16_777_216.0) * 0.5;
            }
            engine.process(&inbuf, &mut outbuf);
        }
    }

    #[test]
    fn gamma_table_decodes_and_matches_upstream() {
        assert_eq!(unsafe { nr2_gamma_table_valid() }, 1);
        for (index, expected) in PINNED {
            let got = unsafe { nr2_gamma_table_value(index as i32) };
            assert_eq!(got, expected, "table value at index {index}");
        }
        // Out-of-range guard.
        assert_eq!(unsafe { nr2_gamma_table_value(-1) }, -1.0);
        assert_eq!(unsafe { nr2_gamma_table_value(TABLE_LEN) }, -1.0);

        // Full scan: finite, non-negative, within the upstream value range.
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for i in 0..TABLE_LEN {
            let v = unsafe { nr2_gamma_table_value(i) };
            assert!(v.is_finite() && v >= 0.0, "bad table value at {i}: {v}");
            min = min.min(v);
            max = max.max(v);
        }
        assert_eq!(min, 0.0);
        assert_eq!(max, 19.119792331180907);
    }

    #[test]
    fn stationary_noise_is_suppressed() {
        let mut engine = Nr2::new();
        // 4 s of white noise at the hop cadence; measure only the last
        // second (after the ~1 s startup ramp and estimator convergence).
        let hops = 4 * 48_000 / HOP;
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        let mut inbuf = vec![0f32; HOP];
        let mut outbuf = vec![0f32; HOP];

        let mut in_energy = 0f64;
        let mut out_energy = 0f64;
        let mut samples = 0u64;
        for f in 0..hops {
            for s in inbuf.iter_mut() {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                *s = ((state >> 40) as f32 / 16_777_216.0) * 0.5;
            }
            engine.process(&inbuf, &mut outbuf);
            if f >= hops - 48_000 / HOP {
                for (i, o) in inbuf.iter().zip(outbuf.iter()) {
                    assert!(o.is_finite(), "non-finite NR2 output");
                    in_energy += f64::from(*i) * f64::from(*i);
                    out_energy += f64::from(*o) * f64::from(*o);
                    samples += 1;
                }
            }
        }
        let in_rms = (in_energy / samples as f64).sqrt();
        let out_rms = (out_energy / samples as f64).sqrt();
        // NR2 on stationary white noise should suppress well below half the
        // input power (>= 12 dB).
        assert!(
            out_rms < 0.25 * in_rms,
            "NR2 did not suppress noise: in_rms={in_rms:.4} out_rms={out_rms:.4}"
        );
    }

    #[test]
    fn tone_passes_through() {
        let mut engine = Nr2::new();
        let sr = 48_000usize;
        let mut inbuf = vec![0f32; HOP];
        let mut outbuf = vec![0f32; HOP];

        // 2 s of noise to learn the floor, then 1 s of a 1 kHz tone at 0.5
        // amplitude.
        feed_noise(&mut engine, 2 * sr / HOP);

        // The engine's OLA output is delayed by DELAY samples (see
        // output_delay_is_one_fft_frame), so the tone must line up with
        // the input delayed by exactly that much.
        let tone_hops = sr / HOP;
        let mut in_v: Vec<f32> = Vec::with_capacity(sr);
        let mut out_v: Vec<f32> = Vec::with_capacity(sr);
        for f in 0..tone_hops {
            for (k, s) in inbuf.iter_mut().enumerate() {
                let t = (f * HOP + k) as f64 / sr as f64;
                *s = (0.5 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin()) as f32;
            }
            engine.process(&inbuf, &mut outbuf);
            in_v.extend_from_slice(&inbuf);
            out_v.extend_from_slice(&outbuf);
        }

        // Skip the first DELAY + 128 samples (delay handover and mask
        // release), then correlate the output with the input at exactly
        // the engine's delay.
        let skip = DELAY + 128;
        let n = in_v.len() - skip - DELAY;
        let mut s_io = 0.0f64;
        let mut s_ii = 0.0f64;
        let mut s_oo = 0.0f64;
        for p in 0..n {
            let i = f64::from(in_v[skip + p]);
            let o = f64::from(out_v[skip + p + DELAY]);
            s_io += i * o;
            s_ii += i * i;
            s_oo += o * o;
        }
        let corr = s_io / (s_ii.sqrt() * s_oo.sqrt());
        assert!(
            corr > 0.5,
            "tone not preserved at the engine's delay: corr={corr:.3}"
        );
    }

    /// Pins the engine's absolute algorithmic delay: a single impulse
    /// must appear at the output exactly DELAY samples later (the COLA
    /// window sums all of the impulse's bin contributions onto one
    /// sample). A steady tone can't do this — any delay differing from
    /// DELAY by a whole tone period is phase-identical at that tone.
    #[test]
    fn output_delay_is_one_fft_frame() {
        let mut engine = Nr2::new();
        let sr = 48_000usize;
        let mut inbuf = vec![0f32; HOP];
        let mut outbuf = vec![0f32; HOP];
        let mut out_v: Vec<f32> = Vec::with_capacity(2 * sr);

        // 1 s of silence (noise estimate converges to the floor, the
        // dry→wet ramp completes), then one impulse, then 1 s more.
        const IMPULSE_HOP: usize = 188;
        for f in 0..(2 * sr) / HOP {
            if f == IMPULSE_HOP {
                inbuf[0] = 0.5;
            }
            engine.process(&inbuf, &mut outbuf);
            out_v.extend_from_slice(&outbuf);
            inbuf.fill(0.0);
        }

        let expected = IMPULSE_HOP * HOP + DELAY;
        // Search a ±128-sample window around the expected position.
        let lo = expected.saturating_sub(128);
        let hi = (expected + 128).min(out_v.len());
        let (peak_idx, peak_val) = (lo..hi)
            .map(|p| (p, out_v[p].abs()))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap();
        assert!(
            (peak_idx as i64 - expected as i64).abs() <= 16,
            "impulse peak at {peak_idx}, expected {expected}"
        );
        assert!(peak_val > 0.1, "impulse too small at the delay: {peak_val:.4}");
    }

    #[test]
    fn reset_counters() {
        let mut engine = Nr2::new();
        let t0 = engine.transient_reset_count();
        let n0 = engine.noise_estimate_reset_count();
        engine.reset_transient();
        assert_eq!(engine.transient_reset_count(), t0 + 1);
        assert_eq!(engine.noise_estimate_reset_count(), n0);
        engine.reset();
        assert!(engine.transient_reset_count() > t0 + 1);
        assert!(engine.noise_estimate_reset_count() > n0);
    }
}
