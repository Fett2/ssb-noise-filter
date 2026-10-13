//! PTT detection: a background poller thread tracks the radio's transmit
//! state from one of three user-selectable sources and publishes it to the
//! audio path and the GUI:
//!
//! - `rigctld`: a TCP rigctld server (e.g. `rigctld-wsjtx.exe`). Wire format
//!   ("default" protocol: no banner, no ACK): the client sends the short PTT
//!   command `t\n`; the server answers with one line carrying the PTT state -
//!   `0` RX, `1` TX, `2` TX mic, `3` TX data - or `RPRT <n>` when the rig
//!   itself reports an error.
//! - `OmniRig`: the classic OmniRig freeware's COM object. The selected rig
//!   slot (1 or 2) is polled: the `Tx` property reads PM_RX / PM_TX.
//! - `serial`: direct CAT polling of the radio on a COM port, with a
//!   per-model status command (see `crate::radio`).
//!
//! Only one source is active at a time; the three are mutually exclusive
//! because they each own the path to the rig (the TCP link, OmniRig's port
//! handle, or the port itself).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::omnirig;
use crate::radio;

/// Published PTT state: `0` = RX, `1` = TX, `2` = TX mic, `3` = TX data
/// (rigctld only), `DISCONNECTED` = no live connection.
pub const DISCONNECTED: u8 = u8::MAX;

/// The rig is keyed for any of these states (transmit in any mode).
pub fn is_keyed(state: u8) -> bool {
    matches!(state, 1..=3)
}

/// How often to ask for the PTT state while connected.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How often to retry while a target is set but the connection is down.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
/// How long to wait for a rigctld answer before declaring the connection
/// dead. Rigs polled over a serial line can be slow, so keep this generous.
const READ_TIMEOUT: Duration = Duration::from_millis(500);
/// How long a TCP connect may take before it counts as failed.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Consecutive poll failures on a serial link before the port is closed and
/// reopened fresh (a radio that stopped answering usually needs a clean
/// re-open).
const MAX_SERIAL_FAILURES: u32 = 5;
/// Refresh the OmniRig link status into the message line every N polls
/// (20 x 100 ms = 2 s), so a cable coming loose shows up without waiting
/// for a reconnect.
const OMNI_STATUS_EVERY: u32 = 20;

/// The PTT source the user selected in the GUI.
#[derive(Clone, Debug)]
pub enum PttSource {
    /// A rigctld server (host + port).
    Rigctld { host: String, port: u16 },
    /// The classic OmniRig freeware; `rig` is the rig slot (1 or 2).
    OmniRig { rig: u8 },
    /// Direct CAT polling of the radio on a serial port.
    Serial {
        port: String,
        baud: u32,
        /// Name of a model in `crate::radio::RADIOS`.
        radio: String,
    },
}

/// A target plus a nonce: every change (even to an identical value) bumps
/// the nonce, which forces the poller to re-establish the connection.
#[derive(Clone)]
struct Target {
    nonce: u64,
    source: PttSource,
}

/// PTT state shared between the poller thread, the capture callback, and the
/// GUI.
pub struct Ptt {
    /// Last PTT state (see DISCONNECTED). Written by the poller thread; the
    /// capture callback reads it with a Relaxed load, once per frame.
    pub ptt: Arc<AtomicU8>,
    /// The source the poller thread should be connected to; None means no
    /// rig is being tracked. Written by the GUI thread, read by the poller.
    target: Arc<Mutex<Option<Target>>>,
    /// One-line status for the GUI (never touched by the audio path).
    pub message: Arc<Mutex<String>>,
    nonce: AtomicU64,
}

impl Ptt {
    pub fn new() -> Self {
        Self {
            ptt: Arc::new(AtomicU8::new(DISCONNECTED)),
            target: Arc::default(),
            message: Arc::new(Mutex::new(String::new())),
            nonce: AtomicU64::new(0),
        }
    }

    /// Start the background poller thread (detached; lives for the process).
    pub fn spawn_poller(&self) {
        let ptt = Arc::clone(&self.ptt);
        let target = Arc::clone(&self.target);
        let message = Arc::clone(&self.message);
        thread::spawn(move || poll_loop(&ptt, &target, &message));
    }

    /// Point the poller thread at a PTT source.
    pub fn set_target(&self, source: PttSource) {
        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed) + 1;
        *self.target.lock().unwrap() = Some(Target { nonce, source });
    }

    /// Tell the poller thread to drop the connection.
    pub fn disconnect(&self) {
        *self.target.lock().unwrap() = None;
    }

    /// Whether a source is currently being tracked (GUI thread only).
    pub fn has_target(&self) -> bool {
        self.target.lock().unwrap().is_some()
    }
}

/// The live connection to whichever source is active.
enum Conn {
    Rigctld(TcpStream),
    OmniRig(omnirig::Client),
    Serial(Box<dyn serialport::SerialPort>),
}

/// Outcome of one poll.
enum PollResult {
    /// A new PTT state.
    State(u8),
    /// The rig reported an error but the link is fine (rigctld RPRT): keep
    /// the last state.
    Hold,
    /// The link must be dropped and re-established.
    Dead(String),
    /// The poll failed but the connection may stay open (serial timeouts);
    /// the loop counts these toward `MAX_SERIAL_FAILURES`.
    Transient(String),
}

fn poll_loop(
    ptt: &AtomicU8,
    target: &Mutex<Option<Target>>,
    message: &Mutex<String>,
) {
    // (nonce the connection was established for, live connection).
    let mut conn: Option<(u64, Conn)> = None;
    let mut failures: u32 = 0;
    let mut status_ticks: u32 = 0;
    let mut last_omni_status: Option<omnirig::Status> = None;
    loop {
        // Clone the target out so the lock is released before any I/O
        // below (a serial poll can take up to the probe's read timeout).
        let current = target.lock().unwrap().clone();
        let wait = match current {
            None => {
                if conn.take().is_some() {
                    set_message(message, "ptt: disconnected");
                }
                ptt.store(DISCONNECTED, Ordering::Relaxed);
                failures = 0;
                status_ticks = 0;
                last_omni_status = None;
                RECONNECT_INTERVAL
            }
            Some(t) => {
                // A different target (method or parameters changed): drop
                // the old connection.
                if conn.as_ref().is_some_and(|(n, _)| *n != t.nonce) {
                    conn = None;
                    failures = 0;
                    status_ticks = 0;
                    last_omni_status = None;
                }
                let alive = match conn.as_mut() {
                    Some((_, c)) => poll_conn(c, &t.source, ptt, message, &mut failures),
                    None => match connect(&t.source) {
                        Ok(c) => {
                            let msg = connect_message(&c, &t.source);
                            last_omni_status = match &c {
                                Conn::OmniRig(client) => client.status(),
                                _ => None,
                            };
                            conn = Some((t.nonce, c));
                            failures = 0;
                            status_ticks = 0;
                            set_message(message, &msg);
                            true
                        }
                        Err(e) => {
                            ptt.store(DISCONNECTED, Ordering::Relaxed);
                            set_message(message, &e);
                            false
                        }
                    },
                };
                // Surface changes in the OmniRig link status (a cable coming
                // loose shows up as "not responding" while the COM link is
                // still alive).
                if alive && matches!(conn, Some((_, Conn::OmniRig(_)))) {
                    status_ticks = status_ticks.wrapping_add(1);
                    if status_ticks >= OMNI_STATUS_EVERY {
                        status_ticks = 0;
                        if let Some((_, Conn::OmniRig(client))) = conn.as_ref() {
                            if let Some(s) = client.status() {
                                if Some(s) != last_omni_status {
                                    last_omni_status = Some(s);
                                    set_message(
                                        message,
                                        &format!("omnirig: connected ({})", s.as_str()),
                                    );
                                }
                            }
                        }
                    }
                }
                if alive { POLL_INTERVAL } else { RECONNECT_INTERVAL }
            }
        };
        thread::sleep(wait);
    }
}

/// One poll of the current connection; false = the connection was dropped
/// and must be re-established.
fn poll_conn(
    conn: &mut Conn,
    source: &PttSource,
    ptt: &AtomicU8,
    message: &Mutex<String>,
    failures: &mut u32,
) -> bool {
    let outcome = match conn {
        Conn::Rigctld(stream) => match read_ptt(stream) {
            Ok(Some(v)) => PollResult::State(v),
            Ok(None) => {
                set_message(message, "rigctld: rig error, holding last PTT state");
                PollResult::Hold
            }
            Err(e) => PollResult::Dead(format!("rigctld: {e}")),
        },
        Conn::OmniRig(client) => match client.tx() {
            Ok(v) => PollResult::State(v),
            // A failed COM call usually means the server side died.
            Err(e) => PollResult::Dead(format!("omnirig: {e}")),
        },
        Conn::Serial(port) => {
            let radio_name = match source {
                PttSource::Serial { radio, .. } => radio.as_str(),
                _ => unreachable!("Conn::Serial requires a serial target"),
            };
            match poll_serial(port, radio_name) {
                Ok(v) => PollResult::State(v),
                Err(e) => PollResult::Transient(format!("serial: {e}")),
            }
        }
    };
    match outcome {
        PollResult::State(v) => {
            *failures = 0;
            ptt.store(v, Ordering::Relaxed);
            true
        }
        PollResult::Hold => true,
        PollResult::Dead(err) => {
            *failures = 0;
            ptt.store(DISCONNECTED, Ordering::Relaxed);
            set_message(message, &err);
            false
        }
        PollResult::Transient(err) => {
            *failures += 1;
            ptt.store(DISCONNECTED, Ordering::Relaxed);
            set_message(message, &err);
            if *failures >= MAX_SERIAL_FAILURES {
                set_message(message, &format!("serial: {err}; retrying with a fresh port"));
                false
            } else {
                true
            }
        }
    }
}

/// Establish a fresh connection to the source.
fn connect(source: &PttSource) -> Result<Conn, String> {
    match source {
        PttSource::Rigctld { host, port } => {
            let stream = rigctld_connect(host, *port)
                .map_err(|e| format!("rigctld: {host}:{port}: {e}"))?;
            Ok(Conn::Rigctld(stream))
        }
        PttSource::OmniRig { rig } => Ok(Conn::OmniRig(omnirig::Client::connect(*rig)?)),
        PttSource::Serial { port, baud, .. } => {
            let name = port.trim();
            if name.is_empty() {
                return Err("serial: no port name".to_string());
            }
            let mut p = serialport::new(name, *baud)
                .open()
                .map_err(|e| format!("serial: {name}: {e}"))?;
            // Yaesu radios with a CAT RTS setting (the FTDX10 defaults it ON)
            // only accept CAT commands while the computer asserts RTS. The crate
            // leaves both control lines at the driver default, so assert them on
            // open; failures are ignored because some adapters report no support.
            let _ = p.write_request_to_send(true);
            let _ = p.write_data_terminal_ready(true);
            Ok(Conn::Serial(p))
        }
    }
}

/// Message shown right after a connection is established.
fn connect_message(conn: &Conn, source: &PttSource) -> String {
    match (conn, source) {
        (Conn::Rigctld(_), PttSource::Rigctld { host, port }) => {
            format!("rigctld: connected to {host}:{port}")
        }
        (Conn::OmniRig(client), PttSource::OmniRig { rig }) => match client.status() {
            Some(s) => format!("omnirig: connected (rig {rig}, {})", s.as_str()),
            None => format!("omnirig: connected (rig {rig})"),
        },
        (Conn::Serial(_), PttSource::Serial { port, .. }) => {
            format!("serial: connected to {port}")
        }
        _ => unreachable!("connection and target must match"),
    }
}

/// Send the model's status command and parse the response.
///
/// The read is a `bytes_to_read` + `read` loop bounded by the probe's
/// timeout, not the port's read timeout: the serialport crate reports a
/// timed-out blocking read as an error (and on Windows the crate's timeout
/// settings do not bound a no-response wait at all), so a radio that
/// answers without a terminator would never parse through a blocking read.
/// Checking `bytes_to_read` first also keeps the loop responsive: the port
/// is only read while data is actually queued.
fn poll_serial(
    port: &mut Box<dyn serialport::SerialPort>,
    radio_name: &str,
) -> Result<u8, String> {
    let def = radio::radio_by_name(radio_name)
        .ok_or_else(|| format!("unknown radio '{radio_name}'"))?;
    let probe = &def.probe;
    // Bounds every I/O below to the probe's timeout: the port opens with
    // the builder's default, a *blocking* timeout.
    port.set_timeout(Duration::from_millis(probe.timeout_ms as u64))
        .map_err(|e| format!("timeout: {e}"))?;
    port.clear(serialport::ClearBuffer::Input)
        .map_err(|e| format!("clear input: {e}"))?;
    port.write_all(probe.cmd).map_err(|e| format!("write: {e}"))?;

    let deadline = Instant::now() + Duration::from_millis(probe.timeout_ms as u64);
    let mut buf = [0u8; 128];
    let mut len = 0usize;
    loop {
        // `bytes_to_read` guards the read: it is only issued while data is
        // actually queued, so it returns promptly on both platforms.
        let mut got = 0usize;
        if port.bytes_to_read().map_err(|e| e.to_string())? > 0 {
            got = port.read(&mut buf[len..]).map_err(|e| e.to_string())?;
            len += got;
        }
        let done = match probe.read {
            radio::ReadSpec::Until(term, cap) => {
                len >= cap || (len > 0 && buf[len - 1] == term)
            }
            radio::ReadSpec::Count(c) => len >= c,
        };
        if done || len >= buf.len() {
            return probe
                .parse(&buf[..len])
                .ok_or_else(|| "no usable response from radio".to_string());
        }
        if Instant::now() >= deadline {
            // Timed out: some radios (the 1-byte Yaesu status replies)
            // never send a terminator, so use whatever arrived.
            return probe
                .parse(&buf[..len])
                .ok_or_else(|| "no response from radio".to_string());
        }
        // Pace the wait; a response already in hand needs no sleep before
        // the next (immediate) read of the remainder.
        if got == 0 {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn set_message(message: &Mutex<String>, text: &str) {
    *message.lock().unwrap() = text.to_string();
}

fn rigctld_connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("no address for host".to_string())?;
    let stream =
        TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Send the short PTT command and parse the one-line answer. `Ok(Some(v))`
/// is a new PTT state, `Ok(None)` means the rig reported an error (keep the
/// last state), `Err` means the connection is dead.
fn read_ptt(stream: &mut TcpStream) -> Result<Option<u8>, String> {
    stream.write_all(b"t\n").map_err(|e| e.to_string())?;
    let line = read_line(stream)?;
    parse_ptt_line(&line)
}

/// Read until the end of the current line (responses are `\n`-terminated).
fn read_line(stream: &mut TcpStream) -> Result<String, String> {
    let mut line = Vec::with_capacity(16);
    let mut buf = [0u8; 64];
    loop {
        let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed".to_string());
        }
        match buf[..n].iter().position(|&b| b == b'\n') {
            Some(i) => {
                line.extend_from_slice(&buf[..i]);
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            None => {
                line.extend_from_slice(&buf);
                if line.len() > 128 {
                    return Err("malformed response".to_string());
                }
            }
        }
    }
}

/// Parse one PTT answer line: a bare `0`-`3`, or `RPRT <n>` when the rig
/// reports an error.
fn parse_ptt_line(line: &str) -> Result<Option<u8>, String> {
    match line {
        "0" => Ok(Some(0)),
        "1" => Ok(Some(1)),
        "2" => Ok(Some(2)),
        "3" => Ok(Some(3)),
        l if l.starts_with("RPRT") => Ok(None),
        other => Err(format!("unexpected response: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ptt_states() {
        assert_eq!(parse_ptt_line("0"), Ok(Some(0)));
        assert_eq!(parse_ptt_line("1"), Ok(Some(1)));
        assert_eq!(parse_ptt_line("2"), Ok(Some(2)));
        assert_eq!(parse_ptt_line("3"), Ok(Some(3)));
    }

    #[test]
    fn rpt_means_keep_last_state() {
        assert_eq!(parse_ptt_line("RPRT -5"), Ok(None));
        assert_eq!(parse_ptt_line("RPRT -110"), Ok(None));
    }

    #[test]
    fn anything_else_is_malformed() {
        assert!(parse_ptt_line("").is_err());
        assert!(parse_ptt_line("4").is_err());
        assert!(parse_ptt_line("garbage").is_err());
    }

    #[test]
    fn disconnected_is_not_a_ptt_state() {
        assert!(!matches!(DISCONNECTED, 0..=3));
        assert!(!is_keyed(DISCONNECTED));
    }

    #[test]
    fn target_roundtrip() {
        let p = Ptt::new();
        assert!(!p.has_target());
        p.set_target(PttSource::OmniRig { rig: 1 });
        assert!(p.has_target());
        p.set_target(PttSource::Rigctld {
            host: "localhost".into(),
            port: 4532,
        });
        assert!(p.has_target());
        p.disconnect();
        assert!(!p.has_target());
    }
}
