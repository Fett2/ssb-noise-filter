//! Streaming linear-interpolation resampler for the render path. The DSP runs
//! at a fixed 48 kHz (RNNoise) while the output device's WASAPI mix format
//! can be at any rate. The signal was already limited to ~3 kHz by the
//! low-pass pre-filter, so linear interpolation is inaudible for voice, both
//! when up- (48 -> 96 kHz) and downsampling (48 -> 44.1 kHz).

/// Resamples a mono stream between two sample rates by linear interpolation.
///
/// Used as a streaming filter: [`process`](Self::process) reports how many
/// samples at the front of `src` have been read and may be discarded. Keep
/// the remainder at the front of your buffer and present it again ahead of
/// the next input. Output slots that cannot be filled because `src` runs out
/// are left untouched, so pre-fill `out` with silence.
#[derive(Debug, Clone)]
pub struct Resampler {
    /// Source-stream position of the next output sample, relative to the
    /// start of the slice passed to the next `process` call. May exceed 1
    /// after a call that ran out of source samples while downsampling.
    pos: f64,
    /// Source samples consumed per output sample.
    ratio: f64,
}

impl Resampler {
    /// Create a resampler from `src_rate` Hz to `dst_rate` Hz.
    pub fn new(src_rate: u32, dst_rate: u32) -> Self {
        Self {
            pos: 0.0,
            ratio: src_rate as f64 / dst_rate as f64,
        }
    }

    /// Source-to-destination sample-rate ratio.
    pub fn ratio(&self) -> f64 {
        self.ratio
    }

    /// Fill `out` from `src`, a continuation of the source stream. Returns
    /// how many samples were consumed from the front of `src`.
    pub fn process(&mut self, src: &[f32], out: &mut [f32]) -> usize {
        let len = src.len();
        let mut produced = 0usize;
        for slot in out.iter_mut() {
            let a = self.pos + produced as f64 * self.ratio;
            let idx = a as usize;
            if idx + 1 >= len {
                // A read needs src[idx] and src[idx + 1]; not enough source.
                break;
            }
            let frac = (a - idx as f64) as f32;
            *slot = src[idx] + (src[idx + 1] - src[idx]) * frac;
            produced += 1;
        }
        let a_next = self.pos + produced as f64 * self.ratio;
        let consumed = (a_next as usize).min(len);
        self.pos = a_next - consumed as f64;
        consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_rate_is_exact_passthrough() {
        let mut res = Resampler::new(48_000, 48_000);
        // The trailing sample is the lookahead the resampler keeps for the
        // last read of each call, as in real streaming use.
        let src = [0.1f32, 0.3, 0.5, 0.7, -0.2, -0.4, -0.6, -0.8, 0.9];
        let mut pending = src.to_vec();
        let mut out = vec![9.0f32; 4];

        let consumed = res.process(&pending, &mut out);
        assert_eq!(&out, &src[..4], "zero-fraction reads return source samples exactly");
        assert_eq!(consumed, 4);
        pending.drain(..consumed);

        // The second call continues on the remainder of the stream.
        let consumed = res.process(&pending, &mut out);
        assert_eq!(&out, &src[4..8]);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn upsampling_inserts_linear_midpoints() {
        let mut res = Resampler::new(48_000, 96_000);
        let src = [0.0f32, 1.0, 2.0, 3.0];
        let mut out = vec![9.0f32; 5];
        let consumed = res.process(&src, &mut out);
        assert_eq!(out, vec![0.0, 0.5, 1.0, 1.5, 2.0], "48 -> 96 kHz inserts midpoints");
        assert_eq!(consumed, 2, "each source sample anchors two output samples");
    }

    #[test]
    fn downsampling_steps_by_ratio() {
        let mut res = Resampler::new(48_000, 24_000);
        let src = [0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0];
        let mut out = vec![9.0f32; 3];
        let consumed = res.process(&src, &mut out);
        assert_eq!(out, vec![0.0, 2.0, 4.0], "48 -> 24 kHz takes every second sample");
        assert_eq!(consumed, 6);
    }

    #[test]
    fn short_source_waits_for_more_input() {
        let mut res = Resampler::new(48_000, 96_000);
        let mut out = [9.0f32; 2];
        assert_eq!(
            res.process(&[1.0f32], &mut out),
            0,
            "a lone sample cannot form a read pair"
        );
        assert_eq!(out, [9.0f32; 2], "unfillable slots are left untouched");

        // The kept sample plus one more form the first read pair.
        assert_eq!(res.process(&[1.0, 2.0], &mut out), 1);
        assert_eq!(out, [1.0, 1.5]);
    }

    #[test]
    fn exhausted_buffer_carries_position_forward() {
        let mut res = Resampler::new(48_000, 16_000); // ratio 3
        let mut out = [9.0f32; 2];
        let consumed = res.process(&[10.0f32, 11.0], &mut out);
        assert_eq!(out, [10.0, 9.0], "only the first slot can be filled");
        assert_eq!(consumed, 2, "the next read lies past this buffer");

        // A fresh buffer continuing the stream: its second sample is the
        // next read anchor, so the first output is built from it.
        out = [9.0f32; 2];
        let consumed = res.process(&[12.0, 13.0, 14.0, 15.0], &mut out);
        assert_eq!(out, [13.0, 9.0]);
        assert_eq!(consumed, 4);

        // And the pattern continues into the buffer after that.
        out = [9.0f32; 2];
        let consumed = res.process(&[16.0, 17.0], &mut out);
        assert_eq!(out, [16.0, 9.0]);
        assert_eq!(consumed, 2);
    }
}
