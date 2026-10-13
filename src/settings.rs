//! User settings persisted across runs: the PTT source (method and its
//! parameters) and whether the app was connected, as a tiny `key=value`
//! file at `%APPDATA%\SSB Noise Filter\config.ini`. Std only, no serde.

use std::env;
use std::fs;
use std::path::PathBuf;

pub const DEFAULT_HOST: &str = "localhost";
pub const DEFAULT_PORT: &str = "4532";
pub const DEFAULT_SERIAL_BAUD: u32 = 9600;

/// PTT method indices (the GUI "PTT method" dropdown).
pub const METHOD_RIGCTLD: u8 = 0;
pub const METHOD_OMNIRIG: u8 = 1;
pub const METHOD_SERIAL: u8 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub rig_host: String,
    pub rig_port: String,
    /// Connect to the saved PTT source automatically at startup.
    /// Persisted under the historical `rig_connected` key.
    pub ptt_connected: bool,
    /// Selected filter engine: 0 = RNNoise, 1 = NR2 (see `nr2::ENGINE_*`).
    pub filter: u8,
    /// PTT method: 0 = rigctld, 1 = OmniRig, 2 = COM port
    /// (see `METHOD_*`).
    pub ptt_method: u8,
    /// OmniRig rig slot for the OmniRig method (1 or 2).
    pub omnirig_rig: u8,
    /// COM port name for the serial method (e.g. "COM3").
    pub serial_port: String,
    /// Baud rate for the serial method.
    pub serial_baud: u32,
    /// Index into `radio::RADIOS` for the serial method.
    pub serial_radio: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rig_host: DEFAULT_HOST.to_owned(),
            rig_port: DEFAULT_PORT.to_owned(),
            ptt_connected: false,
            filter: 0,
            ptt_method: METHOD_RIGCTLD,
            omnirig_rig: 1,
            serial_port: String::new(),
            serial_baud: DEFAULT_SERIAL_BAUD,
            serial_radio: 0,
        }
    }
}

impl Settings {
    /// Load the saved settings; a missing or unreadable file just yields the
    /// defaults.
    pub fn load() -> Self {
        match path().and_then(|p| fs::read_to_string(p).ok()) {
            Some(text) => parse(&text),
            None => Self::default(),
        }
    }

    /// Best-effort save; returns a message for the GUI if it failed.
    pub fn save(&self) -> Result<(), String> {
        let p = path().ok_or("APPDATA is not set")?;
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("creating settings dir: {e}"))?;
        }
        let text = format!(
            "rig_host={}\nrig_port={}\nrig_connected={}\nfilter={}\nptt_method={}\nomnirig_rig={}\nserial_port={}\nserial_baud={}\nserial_radio={}\n",
            self.rig_host,
            self.rig_port,
            u8::from(self.ptt_connected),
            self.filter,
            self.ptt_method,
            self.omnirig_rig,
            self.serial_port,
            self.serial_baud,
            self.serial_radio
        );
        fs::write(&p, text).map_err(|e| format!("writing {}: {e}", p.display()))
    }
}

/// Parse the `key=value` file. Unknown keys are ignored; missing or empty
/// values fall back to the defaults.
fn parse(text: &str) -> Settings {
    let mut s = Settings::default();
    for line in text.lines() {
        let (k, v) = match line.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        match (k.trim(), v.trim()) {
            ("rig_host", v) if !v.is_empty() => s.rig_host = v.to_owned(),
            ("rig_port", v) if !v.is_empty() => s.rig_port = v.to_owned(),
            // The key keeps its historical name for compatibility with
            // config files written before the multi-method PTT.
            ("rig_connected", "1") => s.ptt_connected = true,
            ("rig_connected", "0") => s.ptt_connected = false,
            ("filter", "1") => s.filter = 1,
            ("filter", "0") => s.filter = 0,
            ("ptt_method", v) => {
                if let Ok(n) = v.parse::<u8>() {
                    s.ptt_method = n.min(2);
                }
            }
            ("omnirig_rig", v) => {
                if let Ok(n) = v.parse::<u8>() {
                    if (1..=2).contains(&n) {
                        s.omnirig_rig = n;
                    }
                }
            }
            ("serial_port", v) if !v.is_empty() => s.serial_port = v.to_owned(),
            ("serial_baud", v) => {
                if let Ok(n) = v.parse::<u32>() {
                    if n > 0 {
                        s.serial_baud = n;
                    }
                }
            }
            ("serial_radio", v) => {
                if let Ok(n) = v.parse::<usize>() {
                    s.serial_radio = n;
                }
            }
            _ => {}
        }
    }
    s
}

fn path() -> Option<PathBuf> {
    env::var_os("APPDATA").map(|appdata| {
        PathBuf::from(appdata)
            .join("SSB Noise Filter")
            .join("config.ini")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_all_keys() {
        let s = parse(
            "rig_host=rig.example.com\nrig_port=4533\nrig_connected=1\nfilter=1\nptt_method=2\nomnirig_rig=2\nserial_port=COM3\nserial_baud=19200\nserial_radio=7\n",
        );
        assert_eq!(s.rig_host, "rig.example.com");
        assert_eq!(s.rig_port, "4533");
        assert!(s.ptt_connected);
        assert_eq!(s.filter, 1);
        assert_eq!(s.ptt_method, 2);
        assert_eq!(s.omnirig_rig, 2);
        assert_eq!(s.serial_port, "COM3");
        assert_eq!(s.serial_baud, 19200);
        assert_eq!(s.serial_radio, 7);
    }

    #[test]
    fn parse_defaults_on_missing_or_empty_values() {
        assert_eq!(parse(""), Settings::default());
        assert_eq!(parse("rig_host=\nrig_port=\n"), Settings::default());
        assert_eq!(parse("bogus=1\n"), Settings::default());
    }

    #[test]
    fn parse_ignores_bad_numbers_and_clamps_method() {
        let s = parse("ptt_method=9\nomnirig_rig=5\nserial_baud=abc\nserial_baud=\n");
        assert_eq!(s.ptt_method, 2); // clamped from 9
        assert_eq!(s.omnirig_rig, 1); // out-of-range slot ignored
        let d = Settings::default();
        assert_eq!(s.serial_baud, d.serial_baud); // bad values ignored
    }

    #[test]
    fn round_trip_through_save_format() {
        let s = Settings {
            rig_host: "192.0.2.10".to_owned(), // RFC 5737 TEST-NET-1
            rig_port: "4532".to_owned(),
            ptt_connected: true,
            filter: 1,
            ptt_method: METHOD_SERIAL,
            omnirig_rig: 2,
            serial_port: "COM5".to_owned(),
            serial_baud: 115200,
            serial_radio: 3,
        };
        // save() writes the same format parse() reads.
        let text = format!(
            "rig_host={}\nrig_port={}\nrig_connected={}\nfilter={}\nptt_method={}\nomnirig_rig={}\nserial_port={}\nserial_baud={}\nserial_radio={}\n",
            s.rig_host,
            s.rig_port,
            u8::from(s.ptt_connected),
            s.filter,
            s.ptt_method,
            s.omnirig_rig,
            s.serial_port,
            s.serial_baud,
            s.serial_radio
        );
        assert_eq!(parse(&text), s);
    }
}
