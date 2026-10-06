//! User settings persisted across runs: the rigctld host/port and whether
//! the app was connected, as a tiny `key=value` file at
//! `%APPDATA%\SSB Noise Filter\config.ini`. Std only, no serde.

use std::env;
use std::fs;
use std::path::PathBuf;

pub const DEFAULT_HOST: &str = "localhost";
pub const DEFAULT_PORT: &str = "4532";

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub rig_host: String,
    pub rig_port: String,
    /// Connect to the saved endpoint automatically at startup.
    pub rig_connected: bool,
    /// Selected filter engine: 0 = RNNoise, 1 = NR2 (see `nr2::ENGINE_*`).
    pub filter: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rig_host: DEFAULT_HOST.to_owned(),
            rig_port: DEFAULT_PORT.to_owned(),
            rig_connected: false,
            filter: 0,
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
            "rig_host={}\nrig_port={}\nrig_connected={}\nfilter={}\n",
            self.rig_host,
            self.rig_port,
            u8::from(self.rig_connected),
            self.filter
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
            ("rig_connected", "1") => s.rig_connected = true,
            ("rig_connected", "0") => s.rig_connected = false,
            ("filter", "1") => s.filter = 1,
            ("filter", "0") => s.filter = 0,
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
        let s = parse("rig_host=rig.example.com\nrig_port=4533\nrig_connected=1\nfilter=1\n");
        assert_eq!(s.rig_host, "rig.example.com");
        assert_eq!(s.rig_port, "4533");
        assert!(s.rig_connected);
        assert_eq!(s.filter, 1);
    }

    #[test]
    fn parse_defaults_on_missing_or_empty_values() {
        assert_eq!(parse(""), Settings::default());
        assert_eq!(parse("rig_host=\nrig_port=\n"), Settings::default());
        assert_eq!(parse("bogus=1\n"), Settings::default());
    }

    #[test]
    fn round_trip_through_save_format() {
        let s = Settings {
            rig_host: "192.0.2.10".to_owned(), // RFC 5737 TEST-NET-1
            rig_port: "4532".to_owned(),
            rig_connected: true,
            filter: 1,
        };
        // save() writes the same format parse() reads.
        let text = format!(
            "rig_host={}\nrig_port={}\nrig_connected={}\nfilter={}\n",
            s.rig_host, s.rig_port, u8::from(s.rig_connected), s.filter
        );
        assert_eq!(parse(&text), s);
    }
}
