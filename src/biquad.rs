//! Second-order biquad filters with RBJ Audio EQ Cookbook coefficients,
//! implemented in direct form II transposed.

const TAU: f64 = std::f64::consts::TAU;

/// A stateful second-order IIR filter (one biquad section).
#[derive(Debug, Clone)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    d1: f64,
    d2: f64,
}

impl Biquad {
    /// 2nd-order high-pass. `f0` is the -3 dB frequency, `q` the Q factor
    /// (use 1/sqrt(2) for a Butterworth response).
    pub fn highpass(f0: f64, fs: f64, q: f64) -> Self {
        let w0 = TAU * f0 / fs;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos_w0) / (2.0 * a0),
            b1: -(1.0 + cos_w0) / a0,
            b2: (1.0 + cos_w0) / (2.0 * a0),
            a1: -2.0 * cos_w0 / a0,
            a2: (1.0 - alpha) / a0,
            d1: 0.0,
            d2: 0.0,
        }
    }

    /// 2nd-order low-pass. `f0` is the -3 dB frequency, `q` the Q factor
    /// (use 1/sqrt(2) for a Butterworth response).
    pub fn lowpass(f0: f64, fs: f64, q: f64) -> Self {
        let w0 = TAU * f0 / fs;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos_w0) / (2.0 * a0),
            b1: (1.0 - cos_w0) / a0,
            b2: (1.0 - cos_w0) / (2.0 * a0),
            a1: -2.0 * cos_w0 / a0,
            a2: (1.0 - alpha) / a0,
            d1: 0.0,
            d2: 0.0,
        }
    }

    /// Filter one sample.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let y = self.b0 * x + self.d1;
        self.d1 = self.b1 * x + self.d2 - self.a1 * y;
        self.d2 = self.b2 * x - self.a2 * y;
        y as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Feed a sine of `freq` Hz with amplitude `amp` for `n` samples and
    /// report the peak amplitude of the output over the second half.
    fn settled_peak(filt: &mut Biquad, freq: f64, fs: f64, amp: f32) -> f32 {
        let n = 16_384;
        let mut peak = 0.0f32;
        for i in 0..n {
            let x = (2.0 * PI * freq * i as f64 / fs).sin() as f32 * amp;
            let y = filt.process(x);
            if i >= n / 2 {
                peak = peak.max(y.abs());
            }
        }
        peak
    }

    #[test]
    fn highpass_kills_bass_and_passes_voice_band() {
        let fs = 48_000.0;
        let q = 1.0 / 2.0f64.sqrt();

        let mut hp = Biquad::highpass(300.0, fs, q);
        let bass = settled_peak(&mut hp, 100.0, fs, 0.5);
        assert!(bass < 0.1, "100 Hz through HP300 should be well attenuated, got {bass}");

        let mut hp = Biquad::highpass(300.0, fs, q);
        let voice = settled_peak(&mut hp, 1_000.0, fs, 0.5);
        assert!((0.35..=0.5).contains(&voice), "1 kHz through HP300 should pass, got {voice}");
    }

    #[test]
    fn lowpass_kills_hiss_and_passes_voice_band() {
        let fs = 48_000.0;
        let q = 1.0 / 2.0f64.sqrt();

        let mut lp = Biquad::lowpass(3_000.0, fs, q);
        let hiss = settled_peak(&mut lp, 12_000.0, fs, 0.5);
        assert!(hiss < 0.1, "12 kHz through LP3k should be well attenuated, got {hiss}");

        let mut lp = Biquad::lowpass(3_000.0, fs, q);
        let voice = settled_peak(&mut lp, 1_000.0, fs, 0.5);
        assert!((0.35..=0.5).contains(&voice), "1 kHz through LP3k should pass, got {voice}");
    }

    #[test]
    fn highpass_removes_dc_offset() {
        let fs = 48_000.0;
        let mut hp = Biquad::highpass(300.0, fs, 1.0 / 2.0f64.sqrt());
        let mut last = 0.5f32;
        for _ in 0..32_768 {
            last = hp.process(0.5);
        }
        assert!((last).abs() < 0.001, "DC through a high-pass should settle to ~0, got {last}");
    }
}
