//! Audio engine: device selection plus the capture -> DSP -> render pipeline.
//! Everything here is set up on the GUI thread; the real-time callbacks only
//! touch pre-allocated state.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use nnnoiseless::DenoiseState;

use crate::biquad::Biquad;
use crate::resampler::Resampler;
use crate::rigctl::{self, Rigctl};
use crate::spsc::SpscRing;

/// RNNoise works at 48 kHz in frames of 480 samples (10 ms).
const SAMPLE_RATE: u32 = 48_000;
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// Ring capacity: 8192 samples ~ 170 ms of headroom between the two threads.
const RING_SAMPLES: usize = 8_192;
/// Shared-mode stream buffer: two 48 kHz engine periods (20 ms). The render
/// engine keeps its ring buffer full, so buffer depth is extra latency; the
/// engine default can be far larger. Shared-mode `Initialize` accepts any
/// positive duration — the callback period is always the engine's own (10 ms
/// at 48 kHz) — and this value only sets ring-buffer latency.
const BUFFER_FRAMES: u32 = 960;
/// RNNoise expects/produces f32 samples in the i16 PCM range, not [-1, 1].
const I16_SCALE: f32 = 32_768.0;
/// SSB voice band.
const HP_FREQ: f64 = 300.0;
const LP_FREQ: f64 = 3_000.0;
const Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Keeps the built streams alive while `running` is `Some`.
struct Running {
    #[allow(dead_code)]
    streams: (cpal::Stream, cpal::Stream),
    input_idx: usize,
    output_idx: usize,
}

impl Running {
    fn selection_changed(&self, input_idx: usize, output_idx: usize) -> bool {
        self.input_idx != input_idx || self.output_idx != output_idx
    }
}

/// App state shared with the GUI layer.
pub struct Engine {
    pub host: cpal::Host,
    pub devices: Vec<cpal::Device>,
    pub device_names: Vec<String>,
    pub input_idx: usize,
    pub output_idx: usize,
    pub error: Option<String>,
    pub in_peak: Arc<AtomicU32>,
    pub out_peak: Arc<AtomicU32>,
    /// 0.0-1.0: fraction of the denoised output used (the rest is the
    /// band-passed original). Published by the GUI, read per frame by the
    /// capture callback.
    pub nr_amount: Arc<AtomicU32>,
    /// Rig PTT via rigctld; the poller thread updates `rig.ptt` and the
    /// capture callback bypasses the denoiser while the rig is keyed.
    pub rig: Rigctl,
    pub display_in: f32,
    pub display_out: f32,
    running: Option<Running>,
}

impl Engine {
    pub fn new() -> Self {
        let host = cpal::default_host();
        let devices = host
            .devices()
            .map(|d| d.collect::<Vec<_>>())
            .unwrap_or_default();
        let device_names = devices.iter().map(name_of).collect();
        let input_idx = default_index(&devices, host.default_input_device().as_ref());
        let output_idx = default_index(&devices, host.default_output_device().as_ref());
        let rig = Rigctl::new();
        rig.spawn_poller();
        Self {
            host,
            devices,
            device_names,
            input_idx,
            output_idx,
            error: None,
            in_peak: Arc::new(AtomicU32::new(0.0f32.to_bits())),
            out_peak: Arc::new(AtomicU32::new(0.0f32.to_bits())),
            nr_amount: Arc::new(AtomicU32::new(0.8f32.to_bits())),
            rig,
            display_in: 0.0,
            display_out: 0.0,
            running: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Re-enumerate devices, preserving the current selection when possible.
    pub fn refresh_devices(&mut self) {
        let in_name = self.device_names.get(self.input_idx).cloned();
        let out_name = self.device_names.get(self.output_idx).cloned();
        self.devices = self
            .host
            .devices()
            .map(|d| d.collect::<Vec<_>>())
            .unwrap_or_default();
        self.device_names = self.devices.iter().map(name_of).collect();
        self.input_idx = index_by_name(&self.device_names, in_name.as_deref())
            .unwrap_or_else(|| default_index(&self.devices, self.host.default_input_device().as_ref()));
        self.output_idx = index_by_name(&self.device_names, out_name.as_deref())
            .unwrap_or_else(|| default_index(&self.devices, self.host.default_output_device().as_ref()));
    }

    /// If the user picked different devices while running, restart the streams.
    pub fn sync_selection(&mut self) {
        let changed = self
            .running
            .as_ref()
            .is_some_and(|r| r.selection_changed(self.input_idx, self.output_idx));
        if changed {
            self.stop();
            if let Err(e) = self.start() {
                self.error = Some(e);
            }
        }
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.input_idx >= self.devices.len() {
            return Err("select an input device".into());
        }
        if self.output_idx >= self.devices.len() {
            return Err("select an output device".into());
        }
        let in_device = self.devices[self.input_idx].clone();
        let out_device = self.devices[self.output_idx].clone();
        let in_config = stream_config(&in_device, true)?;
        let out_config = stream_config(&out_device, false)?;
        let in_ch = in_config.channels as usize;
        let out_ch = out_config.channels as usize;

        let ring = Arc::new(SpscRing::new(RING_SAMPLES));
        let in_peak = Arc::clone(&self.in_peak);
        let out_peak = Arc::clone(&self.out_peak);
        let nr_amount = Arc::clone(&self.nr_amount);
        let ptt = Arc::clone(&self.rig.ptt);

        // All DSP state is created on the GUI thread and moved into the
        // real-time capture closure.
        let mut den = DenoiseState::new();
        let mut hp = Biquad::highpass(HP_FREQ, SAMPLE_RATE as f64, Q);
        let mut lp = Biquad::lowpass(LP_FREQ, SAMPLE_RATE as f64, Q);
        let mut acc: Vec<f32> = Vec::with_capacity(FRAME * 4);
        let mut frame = vec![0.0f32; FRAME];
        let mut first_frame = true;
        let ring_for_render = Arc::clone(&ring);
        let in_stream = in_device
            .build_input_stream(
                in_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let peak = data.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
                    in_peak.store(peak.to_bits(), Ordering::Relaxed);

                    // Average every frame's channels down to mono.
                    for i in 0..data.len() / in_ch {
                        let mut s = 0.0f32;
                        for &x in &data[i * in_ch..(i + 1) * in_ch] {
                            s += x;
                        }
                        acc.push(lp.process(hp.process(s / in_ch as f32)));
                    }
                    while acc.len() >= FRAME {
                        for x in &mut acc[..FRAME] {
                            *x *= I16_SCALE;
                        }
                        let _ = den.process_frame(&mut frame, &acc[..FRAME]);
                        // Blend the denoised frame with the band-passed
                        // original (both still in i16 range), then scale back
                        // to [-1, 1]. RNNoise's per-band gain snaps then only
                        // move the output by the wet fraction, and signals the
                        // RNN crushes keep a stable floor. While the rig is
                        // keyed the wet fraction drops to 0 (pure band-pass
                        // through) so the RNN doesn't gain-ride our own
                        // transmitted voice; it keeps being fed, so it is
                        // warm when the key drops.
                        let wet = if rigctl::is_keyed(ptt.load(Ordering::Relaxed)) {
                            0.0
                        } else {
                            f32::from_bits(nr_amount.load(Ordering::Relaxed))
                        };
                        for (out, &x) in frame.iter_mut().zip(acc[..FRAME].iter()) {
                            *out = (*out * wet + x * (1.0 - wet)) / I16_SCALE;
                        }
                        if first_frame {
                            // Discard RNNoise's first frame (fade-in artifact).
                            first_frame = false;
                        } else {
                            ring.push(&frame);
                        }
                        acc.drain(..FRAME);
                    }
                },
                |err| eprintln!("capture error: {err}"),
                None,
            )
            .map_err(|e| format!("capture on {in_device}: {e}"))?;

        // Output at the device's mix rate: 48 kHz devices take the mono
        // samples straight from the ring, anything else goes through a
        // linear resampler.
        let out_stream = if out_config.sample_rate == SAMPLE_RATE {
            // Scratch buffer for the render callback: mono samples popped
            // from the ring before being duplicated into the output channels.
            let mut mono_buf = vec![0.0f32; RING_SAMPLES];
            out_device.build_output_stream(
                out_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // Duplicate each popped mono sample into every channel.
                    let frames = data.len() / out_ch;
                    let n = frames.min(mono_buf.len());
                    let n = ring_for_render.pop(&mut mono_buf[..n]);
                    for i in 0..frames {
                        let v = if i < n {
                            mono_buf[i]
                        } else {
                            0.0
                        };
                        for c in 0..out_ch {
                            data[i * out_ch + c] = v;
                        }
                    }
                    let peak = data.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
                    out_peak.store(peak.to_bits(), Ordering::Relaxed);
                },
                |err| eprintln!("render error: {err}"),
                None,
            )
        } else {
            let mut res = Resampler::new(SAMPLE_RATE, out_config.sample_rate);
            // Source samples queued for the resampler: last callback's
            // leftovers plus freshly popped ring samples.
            let mut pending = vec![0.0f32; RING_SAMPLES];
            // Scratch for the resampler's mono output before duplication.
            let mut mono_buf = vec![0.0f32; RING_SAMPLES];
            out_device.build_output_stream(
                out_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let frames = data.len() / out_ch;
                    mono_buf[..frames].fill(0.0);
                    // Source samples this callback's output will need,
                    // plus the one-sample lookahead the resampler keeps.
                    let need = (frames as f64 * res.ratio() + 2.0) as usize;
                    let base = pending.len();
                    let take = need.min(RING_SAMPLES - base);
                    // Grow within the pre-allocated capacity (no reallocation),
                    // then trim any slots the pop did not fill.
                    pending.resize(base + take, 0.0);
                    let n = ring_for_render.pop(&mut pending[base..base + take]);
                    pending.truncate(base + n);
                    let consumed = res.process(&pending, &mut mono_buf[..frames]);
                    pending.drain(..consumed);
                    for i in 0..frames {
                        let v = mono_buf[i];
                        for c in 0..out_ch {
                            data[i * out_ch + c] = v;
                        }
                    }
                    let peak = data.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
                    out_peak.store(peak.to_bits(), Ordering::Relaxed);
                },
                |err| eprintln!("render error: {err}"),
                None,
            )
        }
        .map_err(|e| format!("render on {out_device}: {e}"))?;

        // cpal 0.18 streams start stopped.
        in_stream.play().map_err(|e| format!("start capture: {e}"))?;
        out_stream.play().map_err(|e| format!("start render: {e}"))?;

        self.running = Some(Running {
            streams: (in_stream, out_stream),
            input_idx: self.input_idx,
            output_idx: self.output_idx,
        });
        Ok(())
    }

    pub fn stop(&mut self) {
        self.running = None;
    }
}

/// Build a stream config that exactly matches the device's WASAPI mix format
/// (shared mode rejects anything else). Input must be 48 kHz (RNNoise); the
/// output may run at any rate, the render callback resamples it. Input
/// channels are averaged to mono, the mono output is duplicated across the
/// output channels. The buffer is a small fixed size (not the engine
/// default), which is what keeps the echo latency down.
fn stream_config(device: &cpal::Device, is_input: bool) -> Result<cpal::StreamConfig, String> {
    let name = device.to_string();
    let mix = if is_input {
        device.default_input_config()
    } else {
        device.default_output_config()
    }
    .map_err(|e| format!("{name}: {e}"))?;
    if is_input && mix.sample_rate() != SAMPLE_RATE {
        return Err(format!(
            "{name}: device format is {} Hz; set the microphone to 48000 Hz in Windows Sound settings",
            mix.sample_rate()
        ));
    }
    if mix.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!(
            "{name}: device format is {:?}; set it to 32-bit float in Windows Sound settings",
            mix.sample_format()
        ));
    }
    Ok(cpal::StreamConfig {
        channels: mix.channels(),
        sample_rate: mix.sample_rate(),
        buffer_size: cpal::BufferSize::Fixed(BUFFER_FRAMES),
    })
}

/// Device name annotated with its WASAPI mix-format rates, so the user can
/// see which devices run at which rate (input must be 48 kHz; the output is
/// resampled to whatever its rate is).
fn name_of(device: &cpal::Device) -> String {
    let khz = |r: u32| r as f64 / 1000.0;
    match (
        device.default_input_config().ok().map(|c| c.sample_rate()),
        device.default_output_config().ok().map(|c| c.sample_rate()),
    ) {
        (Some(i), Some(o)) if i != o => format!("{device} ({} kHz in / {} kHz out)", khz(i), khz(o)),
        (Some(i), _) | (_, Some(i)) => format!("{device} ({} kHz)", khz(i)),
        _ => device.to_string(),
    }
}

fn index_by_name(names: &[String], name: Option<&str>) -> Option<usize> {
    name.and_then(|n| names.iter().position(|x| x == n))
}

fn default_index(devices: &[cpal::Device], default: Option<&cpal::Device>) -> usize {
    default
        .and_then(|d| devices.iter().position(|x| x.id().ok() == d.id().ok()))
        .unwrap_or(0)
}
