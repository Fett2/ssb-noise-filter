//! GUI: an egui window wrapping the audio engine. All device/stream
//! management happens on the GUI thread; the audio threads only talk back
//! through atomics (peak meters).

mod app;
mod biquad;
mod resampler;
mod rigctl;
mod spsc;

use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui;
use egui::CentralPanel;

use app::Engine;

fn main() -> eframe::Result {
    eframe::run_native(
        "SSB Noise Filter",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("SSB Noise Filter")
                .with_inner_size([480.0, 480.0]),
            ..Default::default()
        },
        Box::new(|_cc| Ok(Box::new(FilterApp::new()) as Box<dyn eframe::App>)),
    )
}

struct FilterApp {
    engine: Engine,
    /// rigctld endpoint the user types into the GUI (sent on Connect).
    rig_host: String,
    rig_port: String,
}

impl FilterApp {
    fn new() -> Self {
        Self {
            engine: Engine::new(),
            rig_host: "localhost".into(),
            rig_port: "4532".into(),
        }
    }
}

impl eframe::App for FilterApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let Self {
            engine,
            rig_host,
            rig_port,
        } = self;

        // Peak meters with a slow decay; values are published by the audio
        // threads as f32 bit patterns.
        let in_peak = f32::from_bits(engine.in_peak.load(Ordering::Relaxed));
        let out_peak = f32::from_bits(engine.out_peak.load(Ordering::Relaxed));
        engine.display_in = in_peak.max(engine.display_in * 0.8);
        engine.display_out = out_peak.max(engine.display_out * 0.8);

        // Device changed while running -> restart the streams.
        engine.sync_selection();

        CentralPanel::default().show(ui, |ui| {
            ui.heading("SSB Noise Filter");
            ui.add_space(4.0);
            ui.label("RNNoise + 300 Hz - 3 kHz bandpass; 48 kHz core, output resampled as needed");
            ui.add_space(8.0);

            dropdown(ui, "Input (mic)", &engine.device_names, &mut engine.input_idx);
            ui.add_space(4.0);
            dropdown(ui, "Output (speaker)", &engine.device_names, &mut engine.output_idx);
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let running = engine.is_running();
                let label = if running { "Stop" } else { "Start" };
                if ui.button(label).clicked() {
                    if running {
                        engine.stop();
                    } else if let Err(e) = engine.start() {
                        engine.error = Some(e);
                    }
                }
                if ui.button("Refresh devices").clicked() {
                    engine.refresh_devices();
                }
                if engine.is_running() {
                    ui.colored_label(egui::Color32::LIGHT_GREEN, "running");
                }
            });

            ui.add_space(8.0);
            let mut pct = f32::from_bits(engine.nr_amount.load(Ordering::Relaxed)) * 100.0;
            if ui
                .add(egui::Slider::new(&mut pct, 0.0..=100.0).text("Noise reduction").suffix("%"))
                .changed()
            {
                engine.nr_amount.store((pct / 100.0).to_bits(), Ordering::Relaxed);
            }
            ui.small("Blends the denoised signal with the raw band-passed one: lower it to soften pops and keep weak signals; 100% is full RNNoise.");
            ui.add_space(4.0);
            meter(ui, "In", engine.display_in);
            meter(ui, "Out", engine.display_out);

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Rig PTT (rigctld):");
                ui.label("host");
                ui.add(egui::TextEdit::singleline(rig_host).desired_width(120.0));
                ui.label("port");
                ui.add(egui::TextEdit::singleline(rig_port).desired_width(48.0));
            });
            ui.horizontal(|ui| {
                if ui.button("Connect").clicked() {
                    match rig_port.trim().parse::<u16>() {
                        Ok(port) => engine.rig.connect_to(rig_host.trim(), port),
                        Err(_) => {
                            engine.error = Some("port must be a number, e.g. 4532".into())
                        }
                    }
                }
                if ui.button("Disconnect").clicked() {
                    engine.rig.disconnect();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let state = engine.rig.ptt.load(Ordering::Relaxed);
                    match state {
                        rigctl::DISCONNECTED => {
                            ui.colored_label(egui::Color32::GRAY, "PTT: no rig")
                        }
                        0 => ui.colored_label(egui::Color32::LIGHT_GREEN, "PTT: RX"),
                        _ => ui.colored_label(
                            egui::Color32::RED,
                            "PTT: TX - noise reduction bypassed",
                        ),
                    };
                });
            });
            let state = engine.rig.ptt.load(Ordering::Relaxed);
            let msg = engine.rig.message.lock().unwrap().clone();
            if !msg.is_empty() {
                let color = if state == rigctl::DISCONNECTED {
                    egui::Color32::GRAY
                } else {
                    egui::Color32::LIGHT_GREEN
                };
                ui.colored_label(color, msg);
            }

            if let Some(err) = &engine.error {
                ui.add_space(8.0);
                ui.colored_label(egui::Color32::RED, format!("Error: {err}"));
            }
        });

        // Repaint periodically so the meters animate without input.
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
}

fn dropdown(ui: &mut egui::Ui, label: &str, names: &[String], selected: &mut usize) {
    if names.is_empty() {
        return;
    }
    egui::ComboBox::from_label(label)
        .show_index(ui, selected, names.len(), |i| names[i].clone());
}

fn meter(ui: &mut egui::Ui, label: &str, level: f32) {
    let db = 20.0 * (level.max(1e-4)).log10();
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::ProgressBar::new(level));
        ui.label(format!("{db:5.1} dB"));
    });
}
