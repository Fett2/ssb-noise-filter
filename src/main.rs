//! GUI: an egui window wrapping the audio engine. All device/stream
//! management happens on the GUI thread; the audio threads only talk back
//! through atomics (peak meters).

mod app;
mod biquad;
mod nr2;
mod omnirig;
mod ptt;
mod radio;
mod resampler;
mod settings;
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
                .with_inner_size([480.0, 560.0]),
            ..Default::default()
        },
        Box::new(|_cc| Ok(Box::new(FilterApp::new()) as Box<dyn eframe::App>)),
    )
}

/// The PTT methods offered in the "PTT method" dropdown (keep in sync with
/// `settings::METHOD_*`).
const PTT_METHODS: [&str; 3] = ["rigctld (TCP)", "OmniRig", "COM port"];
/// Baud rates for the COM port method, in dropdown order.
const BAUD_RATES: [u32; 6] = [4800, 9600, 19200, 38400, 57600, 115200];

struct FilterApp {
    engine: Engine,
    /// PTT method (index into PTT_METHODS).
    ptt_method: u8,
    /// OmniRig rig slot index (0 = Rig 1, 1 = Rig 2).
    omnirig_rig: usize,
    /// rigctld endpoint the user types into the GUI (sent on Connect).
    rig_host: String,
    rig_port: String,
    /// COM port method parameters.
    serial_port: String,
    serial_baud: usize, // index into BAUD_RATES
    serial_radio: usize, // index into radio::RADIOS
}

impl FilterApp {
    fn new() -> Self {
        let settings = settings::Settings::load();
        let app = Self {
            engine: Engine::new(),
            ptt_method: settings.ptt_method.min(PTT_METHODS.len() as u8 - 1),
            omnirig_rig: (settings.omnirig_rig.clamp(1, 2) - 1) as usize,
            rig_host: settings.rig_host,
            rig_port: settings.rig_port,
            serial_port: settings.serial_port,
            serial_baud: BAUD_RATES
                .iter()
                .position(|&b| b == settings.serial_baud)
                .unwrap_or(1),
            serial_radio: settings.serial_radio.min(radio::RADIOS.len() - 1),
        };
        // Restore the last-selected filter engine.
        app.engine.filter.store(settings.filter, Ordering::Relaxed);
        // The app was connected when it last exited -> connect again.
        if settings.ptt_connected {
            if let Some(source) = ptt_source(
                app.ptt_method,
                &app.rig_host,
                &app.rig_port,
                app.omnirig_rig,
                &app.serial_port,
                app.serial_baud,
                app.serial_radio,
            ) {
                app.engine.ptt.set_target(source);
            }
        }
        app
    }

    /// Point the poller at the PTT source the user configured; an
    /// unusable field (bad port number, empty COM port) goes to the error
    /// line instead.
    fn connect_ptt(&mut self) {
        match ptt_source(
            self.ptt_method,
            &self.rig_host,
            &self.rig_port,
            self.omnirig_rig,
            &self.serial_port,
            self.serial_baud,
            self.serial_radio,
        ) {
            Some(source) => {
                self.engine.ptt.set_target(source);
                self.save_settings();
            }
            None => {
                self.engine.error = Some(match self.ptt_method {
                    settings::METHOD_RIGCTLD => "port must be a number, e.g. 4532".to_string(),
                    settings::METHOD_SERIAL => "enter the COM port name, e.g. COM3".to_string(),
                    _ => "nothing to connect".to_string(),
                });
            }
        }
    }

    /// Persist the PTT settings and the selected filter engine so they
    /// survive a restart; a file error goes to the GUI error line.
    fn save_settings(&mut self) {
        let s = settings::Settings {
            rig_host: self.rig_host.trim().to_string(),
            rig_port: self.rig_port.trim().to_string(),
            ptt_connected: self.engine.ptt.has_target(),
            filter: self.engine.filter.load(Ordering::Relaxed),
            ptt_method: self.ptt_method,
            omnirig_rig: (self.omnirig_rig + 1) as u8,
            serial_port: self.serial_port.trim().to_string(),
            serial_baud: BAUD_RATES[self.serial_baud],
            serial_radio: self.serial_radio,
        };
        if let Err(e) = s.save() {
            self.engine.error = Some(e);
        }
    }

    /// Filter (engine) selection; persists the choice when it changes.
    fn filter_dropdown(&mut self, ui: &mut egui::Ui) {
        let mut idx = self.engine.filter.load(Ordering::Relaxed) as usize;
        let before = idx;
        egui::ComboBox::from_label("Filter")
            .show_index(ui, &mut idx, FILTER_NAMES.len(), |i| FILTER_NAMES[i]);
        if idx != before {
            self.engine.filter.store(idx as u8, Ordering::Relaxed);
            self.save_settings();
        }
    }

    /// The PTT section: method selection, the method's parameters,
    /// Connect/Disconnect, the RX/TX indicator, and the poller's status
    /// line.
    fn ptt_section(&mut self, ui: &mut egui::Ui) {
        // Method selection; switching methods drops any live connection.
        let mut method = self.ptt_method as usize;
        egui::ComboBox::from_label("PTT method")
            .show_index(ui, &mut method, PTT_METHODS.len(), |i| PTT_METHODS[i]);
        if method != self.ptt_method as usize {
            self.ptt_method = method as u8;
            if self.engine.ptt.has_target() {
                self.engine.ptt.disconnect();
            }
            self.save_settings();
        }

        // The parameters of the selected method.
        match self.ptt_method {
            settings::METHOD_RIGCTLD => {
                ui.horizontal(|ui| {
                    ui.label("host");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.rig_host).desired_width(120.0),
                    );
                    ui.label("port");
                    ui.add(egui::TextEdit::singleline(&mut self.rig_port).desired_width(48.0));
                });
            }
            settings::METHOD_OMNIRIG => {
                let mut rig = self.omnirig_rig;
                egui::ComboBox::from_label("rig slot")
                    .show_index(ui, &mut rig, 2, |i| format!("Rig {}", i + 1));
                if rig != self.omnirig_rig {
                    self.omnirig_rig = rig;
                    self.save_settings();
                }
                ui.weak(
                    "OmniRig (the classic freeware from dxatlas.com) is polled through its COM interface; the selected rig slot is used. It must be installed.",
                );
            }
            settings::METHOD_SERIAL => {
                ui.horizontal(|ui| {
                    ui.label("port");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.serial_port)
                            .desired_width(64.0)
                            .hint_text("COM3"),
                    );
                    let mut baud = self.serial_baud;
                    egui::ComboBox::from_label("baud")
                        .show_index(ui, &mut baud, BAUD_RATES.len(), |i| {
                            BAUD_RATES[i].to_string()
                        });
                    if baud != self.serial_baud {
                        self.serial_baud = baud;
                        self.save_settings();
                    }
                    let mut radio_idx = self.serial_radio;
                    egui::ComboBox::from_label("radio")
                        .show_index(ui, &mut radio_idx, radio::RADIOS.len(), |i| {
                            radio::RADIOS[i].name
                        });
                    if radio_idx != self.serial_radio {
                        self.serial_radio = radio_idx;
                        self.save_settings();
                    }
                });
                ui.weak("The radio's CAT port; the baud rate must match the radio's setting. RTS is asserted on open (Yaesu's CAT RTS gate) — if the radio keys on RTS/DTR, leave that setting OFF.");
            }
            _ => {}
        }

        ui.horizontal(|ui| {
            if ui.button("Connect").clicked() {
                self.connect_ptt();
            }
            if ui.button("Disconnect").clicked() {
                self.engine.ptt.disconnect();
                self.save_settings();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let state = self.engine.ptt.ptt.load(Ordering::Relaxed);
                match state {
                    ptt::DISCONNECTED => {
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

        let state = self.engine.ptt.ptt.load(Ordering::Relaxed);
        let msg = self.engine.ptt.message.lock().unwrap().clone();
        if !msg.is_empty() {
            let color = if state == ptt::DISCONNECTED {
                egui::Color32::GRAY
            } else {
                egui::Color32::LIGHT_GREEN
            };
            ui.colored_label(color, msg);
        }
    }
}

impl eframe::App for FilterApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Peak meters with a slow decay; values are published by the audio
        // threads as f32 bit patterns.
        let in_peak = f32::from_bits(self.engine.in_peak.load(Ordering::Relaxed));
        let out_peak = f32::from_bits(self.engine.out_peak.load(Ordering::Relaxed));
        self.engine.display_in = in_peak.max(self.engine.display_in * 0.8);
        self.engine.display_out = out_peak.max(self.engine.display_out * 0.8);

        // Device changed while running -> restart the streams.
        self.engine.sync_selection();

        CentralPanel::default().show(ui, |ui| {
            ui.heading("SSB Noise Filter");
            ui.add_space(4.0);
            ui.label("RNNoise / NR2 + 300 Hz - 3 kHz bandpass; 48 kHz core, output resampled as needed");
            ui.add_space(8.0);

            dropdown(
                ui,
                "Input (mic)",
                &self.engine.input_names,
                &mut self.engine.input_idx,
            );
            ui.add_space(4.0);
            dropdown(
                ui,
                "Output (speaker)",
                &self.engine.output_names,
                &mut self.engine.output_idx,
            );
            ui.add_space(4.0);
            self.filter_dropdown(ui);
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let running = self.engine.is_running();
                let label = if running { "Stop" } else { "Start" };
                if ui.button(label).clicked() {
                    if running {
                        self.engine.stop();
                    } else if let Err(e) = self.engine.start() {
                        self.engine.error = Some(e);
                    }
                }
                if ui.button("Refresh devices").clicked() {
                    self.engine.refresh_devices();
                }
                if self.engine.is_running() {
                    ui.colored_label(egui::Color32::LIGHT_GREEN, "running");
                }
            });

            ui.add_space(8.0);
            let mut pct =
                f32::from_bits(self.engine.nr_amount.load(Ordering::Relaxed)) * 100.0;
            if ui
                .add(
                    egui::Slider::new(&mut pct, 0.0..=100.0)
                        .text("Noise reduction")
                        .suffix("%"),
                )
                .changed()
            {
                self.engine
                    .nr_amount
                    .store((pct / 100.0).to_bits(), Ordering::Relaxed);
            }
            ui.small("Blends the denoised signal with the raw band-passed one: lower it to soften pops and keep weak signals; 100% is the full denoiser (selected engine).");
            ui.add_space(4.0);
            let mut vol = f32::from_bits(self.engine.out_gain.load(Ordering::Relaxed)) * 100.0;
            if ui
                .add(
                    egui::Slider::new(&mut vol, 0.0..=300.0)
                        .text("Output volume")
                        .suffix("%"),
                )
                .changed()
            {
                self.engine
                    .out_gain
                    .store((vol / 100.0).to_bits(), Ordering::Relaxed);
            }
            ui.small("Above 100% boosts the level; loud peaks clip at the device.");
            ui.add_space(4.0);
            meter(ui, "In", self.engine.display_in);
            meter(ui, "Out", self.engine.display_out);

            ui.add_space(8.0);
            ui.separator();
            self.ptt_section(ui);

            if let Some(err) = &self.engine.error {
                ui.add_space(8.0);
                ui.colored_label(egui::Color32::RED, format!("Error: {err}"));
            }
        });

        // Repaint periodically so the meters animate without input.
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
}

/// Build the PTT source the user configured; None when the fields don't
/// form a usable target (bad port number, empty COM port name).
fn ptt_source(
    method: u8,
    rig_host: &str,
    rig_port: &str,
    omnirig_rig: usize,
    serial_port: &str,
    serial_baud: usize,
    serial_radio: usize,
) -> Option<ptt::PttSource> {
    match method {
        settings::METHOD_RIGCTLD => rig_port.trim().parse::<u16>().ok().map(|port| {
            ptt::PttSource::Rigctld {
                host: rig_host.trim().to_string(),
                port,
            }
        }),
        settings::METHOD_OMNIRIG => Some(ptt::PttSource::OmniRig {
            rig: (omnirig_rig + 1) as u8,
        }),
        settings::METHOD_SERIAL => {
            let port = serial_port.trim().to_uppercase();
            (!port.is_empty()).then(|| ptt::PttSource::Serial {
                port,
                baud: BAUD_RATES[serial_baud],
                radio: radio::RADIOS[serial_radio].name.to_string(),
            })
        }
        _ => None,
    }
}

fn dropdown(ui: &mut egui::Ui, label: &str, names: &[String], selected: &mut usize) {
    if names.is_empty() {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.weak("(no devices)");
        });
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

const FILTER_NAMES: [&str; 2] = ["RNNoise", "NR2 (spectral)"];
